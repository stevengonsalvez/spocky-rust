#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
plan=$(sh "$repository_root/scripts/phase2/hub-account-recovery-capture.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'baseline=28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'disposable archive and PostgreSQL only; port 6767 excluded'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-recovery-original.json'
