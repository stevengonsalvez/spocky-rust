#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
script="$repository_root/scripts/phase2/delivery-platform-qualification.sh"

plan=$("$script" --print-plan)
printf '%s\n' "$plan" | grep -F \
  'baseline: paseo@5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F \
  'macOS: unsigned app lifecycle runtime, bound 600 seconds'
printf '%s\n' "$plan" | grep -F \
  'Linux: retained AppImage and deb runtime, 8+1 tests, digest verified'
printf '%s\n' "$plan" | grep -F \
  'Windows: crate compile only; packaging and runtime unqualified'
printf '%s\n' "$plan" | grep -F \
  'Android: package install and update lifecycle unqualified'
printf '%s\n' "$plan" | grep -F \
  'iOS: SDK and simctl unavailable; package lifecycle unqualified'
printf '%s\n' "$plan" | grep -F \
  'browser: packaging and update lifecycle unqualified'
printf '%s\n' "$plan" | grep -F \
  'signing: no signed artifact, notarization, Gatekeeper, or production key use'
printf '%s\n' "$plan" | grep -F \
  'safety: no host install, deployment, publication, signing, or port 6767'

if "$script" --unknown >/dev/null 2>&1; then
  printf '%s\n' 'unknown delivery qualification mode was accepted' >&2
  exit 1
fi

grep -F 'gtimeout 600 sh "$repository_root/scripts/phase2/delivery-macos-runtime.test.sh"' \
  "$script" >/dev/null
grep -F 'ca80b66d7f25e2faf58587a311abfa46e37e0814c1414c2507a03eb3e02f94b7' \
  "$script" >/dev/null
grep -F '31b4371386bbbd0d0d0004f657f8619716670170b5fed343482e324d9c592f53' \
  "$script" >/dev/null
grep -F 'e81f3644a06089dbed8835d8f6b0745914b0c517b67b0b407c4d8a691e7a5179' \
  "$script" >/dev/null

