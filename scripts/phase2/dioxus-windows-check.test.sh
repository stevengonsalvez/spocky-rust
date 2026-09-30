#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/dioxus-windows-check.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'cargo-xwin: 0.23.1'
printf '%s\n' "$plan" | grep -F 'target: x86_64-pc-windows-msvc'
printf '%s\n' "$plan" | grep -F 'cache and target output: repository-local ignored paths'
printf '%s\n' "$plan" | grep -F 'bound: 1200 seconds'
printf '%s\n' "$plan" | grep -F 'qualification: compile check only, no Windows launch claim'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/dioxus-windows-check.log'
