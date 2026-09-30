#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
plan=$(sh "$repository_root/scripts/phase2/hub-account-recovery-compare.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'normalization: generated verification token, password-reset token, and session-cookie value only'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-recovery-original.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-recovery-rust.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-recovery-comparison.json'
