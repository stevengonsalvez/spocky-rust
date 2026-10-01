#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/audio-linux-runtime.sh"

plan=$($runner --print-plan)
printf '%s\n' "$plan" | grep -F \
  'baseline: paseo@5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F 'runtime: ALSA null playback and capture'
printf '%s\n' "$plan" | grep -F \
  'inventory only: PulseAudio and PipeWire CLI/version probes'
printf '%s\n' "$plan" | grep -F \
  'container: --rm, exact name spocky-audio-linux-<epoch>-<pid>, repository mounted read-only'
printf '%s\n' "$plan" | grep -F \
  'safety: no physical microphone, audible-output assertion, production service, deployment, or port 6767'

if "$runner" --unknown >/dev/null 2>&1; then
  printf '%s\n' 'unknown Linux audio runtime mode was accepted' >&2
  exit 1
fi

grep -F 'cargo test --locked -p spocky-audio-delivery-pilot --test linux_audio_runtime' \
  "$runner" >/dev/null
grep -F 'cargo run --locked -p spocky-audio-delivery-pilot --bin spocky-linux-audio-runtime' \
  "$runner" >/dev/null
grep -F 'cargo clippy --locked -p spocky-audio-delivery-pilot --all-targets' \
  "$runner" >/dev/null
grep -F 'docker rm -f "$container"' "$runner" >/dev/null
grep -F 'pwd -P' "$runner" >/dev/null
grep -F 'dpkg-query -W -f="\${Package} \${Version}\\n"' "$runner" >/dev/null
grep -F 'jq=1.6-2.1+deb12u2' "$runner" >/dev/null
test "$(grep -Fc 'rm -f "$output/container.cid"' "$runner")" -eq 2
