#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/plugin-linux-runtime.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'rust image digest: sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55'
printf '%s\n' "$plan" | grep -F 'repository mount: read-only'
printf '%s\n' "$plan" | grep -F 'platform: linux/amd64'
printf '%s\n' "$plan" | grep -F 'bound: 1200 seconds'
printf '%s\n' "$plan" | grep -F 'tests: runtime_acquisition, process_protocol, plugin_lifecycle, settings_lifecycle'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/plugin-linux-runtime.log'
