#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/renderer-platform-linux.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'rust image digest: sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55'
printf '%s\n' "$plan" | grep -F 'container limits: 2 CPUs, 3 GB memory, 1200 seconds'
printf '%s\n' "$plan" | grep -F 'viewport: 1280x800, scale 1, light theme, en-US, DejaVu Sans'
printf '%s\n' "$plan" | grep -F 'visual: complete SHA-256 membership plus unmasked RGBA RMSE'
printf '%s\n' "$plan" | grep -F 'accessibility: complete AT-SPI tree plus observed focus order'
printf '%s\n' "$plan" | grep -F 'interaction: xdotool Tab then Return on first Plus control'
printf '%s\n' "$plan" | grep -F 'evidence/phase2/renderer-platform-linux/'

if rg -n '\x{2014}' \
  "$repository_root/scripts/phase2/renderer-platform-linux.sh" \
  "$repository_root/scripts/phase2/renderer-platform-linux.test.sh" \
  "$repository_root/scripts/phase2/renderer-platform-linux-atspi.py" \
  "$repository_root/scripts/phase2/renderer-platform-linux-compare.py"; then
  printf '%s\n' 'renderer Linux scripts contain forbidden em dash' >&2
  exit 1
fi

printf '%s\n' '7 renderer platform script assertions passed'
