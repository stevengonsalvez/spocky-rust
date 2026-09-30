#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline=5de45e208690b0efc51c59a585ae9729325a9204
image_digest=sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55
docker_image="rust@$image_digest"
timeout_seconds=${SPOCKY_DELIVERY_LINUX_TIMEOUT:-1500}
default_output="$repository_root/evidence/phase2/delivery-linux-runtime"

usage() {
  printf 'usage: %s [--print-plan|--output DIRECTORY]\n' "$0" >&2
}

print_plan() {
  printf '%s\n' "baseline: paseo@$baseline"
  printf '%s\n' "image: rust@$image_digest"
  printf '%s\n' 'lanes: AppImage stable-name update/rollback/uninstall, deb dpkg install/update/rollback/remove/purge'
  printf '%s\n' 'fixtures: baseline launcher.sh, after-install.tpl, electron-builder 26.8.1 after-remove.tpl'
  printf '%s\n' 'container: --rm, exact name spocky-delivery-linux-<epoch>-<pid>, repository mounted read-only'
  printf '%s\n' "timeout: ${timeout_seconds}s hard, then docker rm -f of the exact container name"
  printf '%s\n' 'safety: no host install, no port 6767, no publication, signing, or deployment'
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
  printf 'gtimeout or timeout is required for bounded Linux qualification\n' >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || { printf 'jq is required\n' >&2; exit 1; }
docker info >/dev/null 2>&1 || { printf 'Docker is required for Linux qualification\n' >&2; exit 1; }

mkdir -p "$output"
output=$(CDPATH='' cd -- "$output" && pwd)
rm -rf "$output/run" "$output/container.log" "$output/container.cid"
log="$output/container.log"
container="spocky-delivery-linux-$(date +%s)-$$"
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Shell variables in this command expand inside the Linux container.
# shellcheck disable=SC2016
linux_command='set -eu
export PATH=/usr/local/cargo/bin:$PATH
rustc --version
cargo --version
set -x
cargo clippy --locked -p spocky-audio-delivery-pilot --all-targets -- -D warnings
cargo test --locked -p spocky-audio-delivery-pilot --test linux_delivery_runtime
cargo run --locked -p spocky-audio-delivery-pilot --bin spocky-linux-delivery-runtime -- /output/run /tmp/spocky-delivery-work
cargo test --locked -p spocky-audio-delivery-pilot --test linux_delivery_dpkg -- --ignored
set +x
# Lifecycle ran on container-local disk; keep only the package artifacts as evidence.
mkdir -p /output/run/packages
cp /tmp/spocky-delivery-work/deb-packages/*.deb /tmp/spocky-delivery-work/appimage-packages/* /output/run/packages/
(cd /output && find run -type f | sort | xargs sha256sum)
chown -R "$HOST_UID:$HOST_GID" /output
printf "%s\n" LINUX_DELIVERY_OK'

set +e
"$timeout_command" --kill-after=30 "$timeout_seconds" docker run --rm --name "$container" \
  --cidfile "$output/container.cid" --platform linux/amd64 \
  --mount "type=bind,src=$repository_root,dst=/workspace,readonly" \
  --mount "type=bind,src=$output,dst=/output" \
  --workdir /workspace \
  --env CARGO_TARGET_DIR=/tmp/spocky-target \
  --env SPOCKY_DELIVERY_DISPOSABLE_ROOT=1 \
  --env "HOST_UID=$(id -u)" --env "HOST_GID=$(id -g)" \
  "$docker_image" bash -c "$linux_command" >"$log" 2>&1
status=$?
set -e
finished=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if [ "$status" -eq 124 ] || [ "$status" -eq 137 ]; then
  docker rm -f "$container" >/dev/null 2>&1 || true
  printf 'Linux delivery qualification timed out after %ss; see %s\n' "$timeout_seconds" "$log" >&2
fi

jq -n \
  --arg baseline "paseo@$baseline" --arg image "rust@$image_digest" \
  --arg container "$container" --arg started "$started" --arg finished "$finished" \
  --arg log "container.log" --argjson status "$status" \
  --argjson timeout "$timeout_seconds" \
  --arg command "$timeout_command --kill-after=30 $timeout_seconds docker run --rm --name $container --platform linux/amd64 <repo:/workspace:ro> <output:/output> $docker_image bash -c <cargo clippy; cargo test linux_delivery_runtime; cargo run spocky-linux-delivery-runtime; cargo test linux_delivery_dpkg --ignored>" \
  '{baseline:$baseline,image:$image,container:$container,command:$command,
    timeoutSeconds:$timeout,started:$started,finished:$finished,exitStatus:$status,log:$log}' \
  >"$output/run-metadata.json"

if [ "$status" -ne 0 ]; then
  printf 'Linux delivery qualification failed with status %s; see %s\n' "$status" "$log" >&2
  exit "$status"
fi
grep -F 'LINUX_DELIVERY_OK' "$log" >/dev/null || {
  printf 'Linux delivery qualification omitted success marker: %s\n' "$log" >&2
  exit 1
}

report="$output/run/linux-delivery-report.json"
jq -e '
  .baseline == "paseo@5de45e208690b0efc51c59a585ae9729325a9204" and
  .contractId == "P2-DELIVERY-01" and
  .appimageCorruptUpdatePreservedInstall == true and
  .debCorruptUpdatePreservedInstall == true and
  ([.steps[] | select(.lane == "appimage") | .operation] == [
    "install_launch", "corrupt_update_rejected", "valid_update_launch",
    "rollback_launch", "uninstall_retain_state"]) and
  ([.steps[] | select(.lane == "deb") | .operation] == [
    "install", "install_launch", "corrupt_update_rejected", "corrupt_update_launch",
    "valid_update", "valid_update_launch", "rollback_by_reinstall", "rollback_launch",
    "remove_retain_state", "purge_retain_state"])
' "$report" >/dev/null

printf 'Linux delivery qualification passed: %s\n' "$output"
(cd "$output" && shasum -a 256 container.log run-metadata.json run/linux-delivery-report.json)
