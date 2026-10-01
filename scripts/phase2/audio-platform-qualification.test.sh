#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
script="$repository_root/scripts/phase2/audio-platform-qualification.sh"

plan=$("$script" --print-plan)
printf '%s\n' "$plan" | grep -F \
  'baseline: paseo@5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F \
  'macOS: targeted audio runtime, 10 tests, bound 600 seconds'
printf '%s\n' "$plan" | grep -F \
  'Linux: crate all-target compilation retained; audio runtime unqualified'
printf '%s\n' "$plan" | grep -F \
  'Windows: locked MSVC all-target compile only, bound 1200 seconds'
printf '%s\n' "$plan" | grep -F \
  'Android: historical OS-command evidence only; selected-app audio unqualified'
printf '%s\n' "$plan" | grep -F \
  'iOS: SDK and simctl unavailable; runtime unqualified'
printf '%s\n' "$plan" | grep -F \
  'browser: contract model only; capture and playback runtime unqualified'
printf '%s\n' "$plan" | grep -F \
  'safety: no device, emulator, signing, deployment, publication, or port 6767'

if "$script" --unknown >/dev/null 2>&1; then
  printf '%s\n' 'unknown audio qualification mode was accepted' >&2
  exit 1
fi

grep -F 'cargo test --locked -p spocky-audio-delivery-pilot' "$script" >/dev/null
grep -F -- '--test audio_contract --test local_runtime' "$script" >/dev/null
grep -F -- '--test macos_audio_runtime --test runtime_report' "$script" >/dev/null
grep -F 'xwin check --locked --target "$windows_target"' "$script" >/dev/null
grep -F -- '-p spocky-audio-delivery-pilot --all-targets' "$script" >/dev/null

