#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
plan=$("$repository_root/scripts/phase2/hub-account-state-compare.sh" --print-plan)

printf '%s\n' "$plan" | grep -F 'normalization: generated identity-preserving account, organization, membership, invitation, slug, link, and expiry values only'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-original.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-rust.json'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/hub-account-state-comparison.json'

fixture=$(mktemp -d /private/tmp/paseo-hub-normalization-test.XXXXXX)
cleanup() {
  case "$fixture" in
    /private/tmp/paseo-hub-normalization-test.*) rm -rf "$fixture" ;;
    *) printf 'refusing to remove unexpected fixture: %s\n' "$fixture" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

original="$repository_root/evidence/raw/phase2/hub-account-state-original.json"
"$repository_root/scripts/phase2/hub-account-state-compare.sh" --normalize "$original" \
  >"$fixture/original.json"

jq 'del(.states.passwordChangeRequired.account.id)' "$original" >"$fixture/missing.json"
"$repository_root/scripts/phase2/hub-account-state-compare.sh" --normalize "$fixture/missing.json" \
  >"$fixture/missing-normalized.json"
if cmp -s "$fixture/original.json" "$fixture/missing-normalized.json"; then
  printf 'normalization concealed missing account id\n' >&2
  exit 1
fi

jq '.states.appSetupRequired.organization.id = null' "$original" >"$fixture/null.json"
"$repository_root/scripts/phase2/hub-account-state-compare.sh" --normalize "$fixture/null.json" \
  >"$fixture/null-normalized.json"
if cmp -s "$fixture/original.json" "$fixture/null-normalized.json"; then
  printf 'normalization concealed null organization id\n' >&2
  exit 1
fi

jq '.states.invitedActive.team.members[0].userId = .states.active.account.id' "$original" \
  >"$fixture/wrong-reference.json"
"$repository_root/scripts/phase2/hub-account-state-compare.sh" \
  --normalize "$fixture/wrong-reference.json" >"$fixture/wrong-reference-normalized.json"
if cmp -s "$fixture/original.json" "$fixture/wrong-reference-normalized.json"; then
  printf 'normalization concealed wrong identity reference\n' >&2
  exit 1
fi
