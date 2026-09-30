#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/hub-account-state-compare.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'normalization: generated identity-preserving account, organization, membership, invitation, slug, link, and expiry values only'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-original.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-rust.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-comparison.json'
