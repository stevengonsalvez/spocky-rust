#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline=5de45e208690b0efc51c59a585ae9729325a9204
image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
docker_image="rust@$image_digest"
timeout_seconds=${SPOCKY_AUDIO_LINUX_TIMEOUT:-900}
default_output="$repository_root/evidence/phase2/audio-linux-runtime"

usage() {
  printf 'usage: %s [--print-plan|--output DIRECTORY]\n' "$0" >&2
}

print_plan() {
  printf '%s\n' "baseline: paseo@$baseline"
  printf '%s\n' "image: rust@$image_digest"
  printf '%s\n' 'runtime: ALSA null playback and capture'
  printf '%s\n' 'inventory only: PulseAudio and PipeWire CLI/version probes'
  printf '%s\n' 'container: --rm, exact name spocky-audio-linux-<epoch>-<pid>, repository mounted read-only'
  printf '%s\n' "timeout: ${timeout_seconds}s hard, then exact-name container cleanup"
  printf '%s\n' 'safety: no physical microphone, audible-output assertion, production service, deployment, or port 6767'
  printf '%s\n' "output: $default_output"
}

case "${1:-}" in
  --print-plan)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    print_plan
    exit 0
    ;;
  --output)
    [ "$#" -eq 2 ] || { usage; exit 2; }
    output=$2
    ;;
  '')
    output=$default_output
    ;;
  *)
    usage
    exit 2
    ;;
esac

command -v gtimeout >/dev/null 2>&1 && timeout_command=gtimeout ||
  { command -v timeout >/dev/null 2>&1 && timeout_command=timeout; } || {
  printf 'gtimeout or timeout is required for bounded Linux audio qualification\n' >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || { printf 'jq is required\n' >&2; exit 1; }
docker info >/dev/null 2>&1 || { printf 'Docker is required for Linux audio qualification\n' >&2; exit 1; }

mkdir -p "$output/run"
output=$(CDPATH='' cd -- "$output" && pwd -P)
rm -f "$output/container.log" "$output/container.cid" "$output/run-metadata.json"
rm -f "$output/run/generated-playback.wav" "$output/run/generated-capture.wav"
rm -f "$output/run/linux-audio-report.json"
log="$output/container.log"
container="spocky-audio-linux-$(date +%s)-$$"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

cleanup_container() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  rm -f "$output/container.cid"
}
trap cleanup_container HUP INT TERM EXIT

# Shell variables in this command expand inside the Linux container.
# shellcheck disable=SC2016
linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  alsa-utils=1.2.8-1 \
  pulseaudio-utils=16.1+dfsg1-2+b1 \
  pipewire-bin=0.3.65-3+deb12u1 \
  jq=1.6-2.1+deb12u2
uname -a
rustc --version
cargo --version
dpkg-query -W -f="\${Package} \${Version}\\n" alsa-utils pulseaudio-utils pipewire-bin jq
set -x
cargo test --locked -p spocky-audio-delivery-pilot --test linux_audio_runtime -- --test-threads=1
cargo run --locked -p spocky-audio-delivery-pilot --bin spocky-linux-audio-runtime -- /output/run
cargo clippy --locked -p spocky-audio-delivery-pilot --all-targets -- -D warnings
set +x
jq -e '\''
  .baseline == "paseo@5de45e208690b0efc51c59a585ae9729325a9204" and
  .contractId == "P2-AUDIO-01" and
  .claim == "linux_alsa_null_runtime" and
  .inventory.alsa.available == true and
  .inventory.pulseaudio.available == true and
  .inventory.pipewire.available == true and
  .playback.exitCode == 0 and
  .playback.samples == 1600 and
  .capture.exitCode == 0 and
  .capture.samples == 16000 and
  .capture.matchesGeneratedInput == true and
  (.limitations | index("no_physical_microphone_capture")) != null and
  (.limitations | index("no_audible_output_assertion")) != null and
  (.limitations | index("pulseaudio_cli_inventory_only")) != null and
  (.limitations | index("pipewire_cli_inventory_only")) != null
'\'' /output/run/linux-audio-report.json >/dev/null
(cd /output && find run -type f | sort | xargs sha256sum)
chown -R "$HOST_UID:$HOST_GID" /output
printf "%s\n" LINUX_AUDIO_RUNTIME_OK'

set +e
"$timeout_command" --kill-after=30 "$timeout_seconds" docker run --rm --name "$container" \
  --cidfile "$output/container.cid" --platform linux/amd64 \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --mount "type=bind,src=$output,dst=/output" \
  --workdir /workspace \
  --env CARGO_TARGET_DIR=/tmp/spocky-audio-target \
  --env "HOST_UID=$(id -u)" --env "HOST_GID=$(id -g)" \
  "$docker_image" bash -c "$linux_command" >"$log" 2>&1
run_status=$?
set -e
finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)

jq -n \
  --arg baseline "paseo@$baseline" --arg image "rust@$image_digest" \
  --arg container "$container" --arg started "$started" --arg finished "$finished" \
  --arg log "container.log" --argjson status "$run_status" \
  --argjson timeout "$timeout_seconds" \
  --arg command "$timeout_command --kill-after=30 $timeout_seconds docker run --rm --name $container --platform linux/amd64 <repo:/workspace:ro> <output:/output> $docker_image bash -c <install pinned audio tools; targeted test; runtime report; all-target clippy>" \
  '{baseline:$baseline,image:$image,container:$container,command:$command,
    timeoutSeconds:$timeout,started:$started,finished:$finished,exitStatus:$status,log:$log}' \
  >"$output/run-metadata.json"

if [ "$run_status" -eq 124 ] || [ "$run_status" -eq 137 ]; then
  printf 'Linux audio qualification timed out after %ss; see %s\n' \
    "$timeout_seconds" "$log" >&2
fi
if [ "$run_status" -ne 0 ]; then
  printf 'Linux audio qualification failed with status %s; see %s\n' \
    "$run_status" "$log" >&2
  exit "$run_status"
fi
grep -Fx 'LINUX_AUDIO_RUNTIME_OK' "$log" >/dev/null || {
  printf 'Linux audio qualification omitted success marker: %s\n' "$log" >&2
  exit 1
}

printf 'Linux audio qualification passed: %s\n' "$output"
(cd "$output" && shasum -a 256 container.log run-metadata.json run/linux-audio-report.json)
