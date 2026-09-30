#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
output=$("$repository_root/scripts/phase2/hub-runtime-capture.sh" --preflight-only)

printf '%s\n' "$output" | grep -F \
  'Hub baseline preflight passed: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$output" | grep -F 'port 6767 excluded'

plan=$("$repository_root/scripts/phase2/hub-runtime-capture.sh" --print-plan)
printf '%s\n' "$plan" | grep -F 'src/db/runtime/embedded-persistence.integration.test.ts'
printf '%s\n' "$plan" | grep -F 'src/index.embedded.integration.test.ts'
printf '%s\n' "$plan" | grep -F 'src/instance-setup/environment-bootstrap.integration.test.ts'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-runtime-original.json'
printf '%s\n' "$plan" | grep -F 'Ryuk disabled; suite stops exact PostgreSQL container'
