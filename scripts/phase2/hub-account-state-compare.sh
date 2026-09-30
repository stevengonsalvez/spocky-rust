#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
original="$raw_dir/hub-account-state-original.json"
rust="$raw_dir/hub-account-state-rust.json"
comparison="$raw_dir/hub-account-state-comparison.json"

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'normalization: generated identity-preserving account, organization, membership, invitation, slug, link, and expiry values only'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-comparison.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if [ ! -f "$original" ]; then
  printf 'missing original evidence: %s\n' "$original" >&2
  exit 1
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded candidate capture\n' >&2
  exit 1
fi

mkdir -p "$raw_dir"
gtimeout 120 cargo run -q \
  --manifest-path "$repository_root/Cargo.toml" \
  -p paseo-hub-pilot \
  --bin hub-account-state-evidence >"$rust"

work_dir=$(mktemp -d /private/tmp/paseo-hub-account-state.XXXXXX)
cleanup() {
  case "$work_dir" in
    /private/tmp/paseo-hub-account-state.*) rm -rf "$work_dir" ;;
    *) printf 'refusing to remove unexpected comparison directory: %s\n' "$work_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

normalize='def account_id:
  if . == "owner@example.test" then "<owner-account-id>"
  elif . == "member@example.test" then "<invited-account-id>"
  else "<account-id>"
  end;
def membership_id:
  if . == "owner" then "<owner-membership-id>"
  elif . == "member" then "<invited-membership-id>"
  else "<membership-id>"
  end;
def generated_ids:
  del(.baseline)
  | .operations.acceptInvitationBody.organizationId = "<organization-id>"
  | .states |= with_entries(
      .value |= (
        if has("account") then .account.id = (.account.email | account_id) else . end
        | if has("memberships") then
            .memberships |= map(
              .id = "<organization-id>"
              | .slug = "<organization-slug>"
              | .membershipId = (.role | membership_id)
            )
          else . end
        | if has("organization") then
            .organization.id = "<organization-id>"
            | .organization.slug = "<organization-slug>"
          else . end
        | if has("membership") then .membership.id = (.membership.role | membership_id) else . end
        | if has("team") then
            .team.members |= map(
              .id = (.role | membership_id)
              | .userId = (.email | account_id)
            )
            | if .team | has("invitations") then
                .team.invitations |= map(
                  .id = "<invitation-id>"
                  | .expiresAt = "<invitation-expiry>"
                  | .link = "<invitation-link>"
                )
              else . end
          else . end
      )
    );
  generated_ids'

jq -S "$normalize" "$original" >"$work_dir/original-normalized.json"
jq -S "$normalize" "$rust" >"$work_dir/rust-normalized.json"
original_sha=$(shasum -a 256 "$original" | awk '{print $1}')
rust_sha=$(shasum -a 256 "$rust" | awk '{print $1}')
matched=false
if cmp -s "$work_dir/original-normalized.json" "$work_dir/rust-normalized.json"; then
  matched=true
fi

jq -n \
  --argjson matched "$matched" \
  --arg originalSha256 "$original_sha" \
  --arg rustSha256 "$rust_sha" \
  --slurpfile originalNormalized "$work_dir/original-normalized.json" \
  --slurpfile rustNormalized "$work_dir/rust-normalized.json" \
  '{
    schemaVersion: 1,
    matched: $matched,
    normalization: "generated identity-preserving account, organization, membership, invitation, slug, link, and expiry values only",
    originalRawSha256: $originalSha256,
    rustRawSha256: $rustSha256,
    originalNormalized: $originalNormalized[0],
    rustNormalized: $rustNormalized[0]
  }' >"$comparison"

if [ "$matched" != true ]; then
  diff -u "$work_dir/original-normalized.json" "$work_dir/rust-normalized.json" >&2 || true
  printf 'Hub account-state differential failed: %s\n' "$comparison" >&2
  exit 1
fi

printf 'Hub account-state differential passed: %s\n' "$comparison"
