#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
original="$raw_dir/hub-account-state-original.json"
rust="$raw_dir/hub-account-state-rust.json"
comparison="$raw_dir/hub-account-state-comparison.json"

normalize='def replace_generated($raw; $label):
  if type == "string" and . == $raw then $label else . end;
def generated_ids:
  . as $root
  | ($root.states.active.account.id // null) as $owner_account
  | ($root.states.invitedActive.account.id // null) as $invited_account
  | ($root.states.active.organization.id // null) as $organization
  | ($root.states.active.organization.slug // null) as $organization_slug
  | ($root.states.active.membership.id // null) as $owner_membership
  | ($root.states.invitedActive.membership.id // null) as $invited_membership
  | ($root.states.active.team.invitations[0].id // null) as $invitation
  | ($root.states.active.team.invitations[0].expiresAt // null) as $invitation_expiry
  | ($root.states.active.team.invitations[0].link // null) as $invitation_link
  | del(.baseline)
  | (.operations.acceptInvitationBody
      | select(type == "object" and has("organizationId"))
      | .organizationId) |= replace_generated($organization; "<organization-id>")
  | (.states[] | select(.account? | type == "object" and has("id")) | .account.id)
      |= (replace_generated($owner_account; "<owner-account-id>")
          | replace_generated($invited_account; "<invited-account-id>"))
  | (.states[] | .memberships[]? | select(has("id")) | .id)
      |= replace_generated($organization; "<organization-id>")
  | (.states[] | .memberships[]? | select(has("slug")) | .slug)
      |= replace_generated($organization_slug; "<organization-slug>")
  | (.states[] | .memberships[]? | select(has("membershipId")) | .membershipId)
      |= (replace_generated($owner_membership; "<owner-membership-id>")
          | replace_generated($invited_membership; "<invited-membership-id>"))
  | (.states[] | select(.organization? | type == "object" and has("id")) | .organization.id)
      |= replace_generated($organization; "<organization-id>")
  | (.states[] | select(.organization? | type == "object" and has("slug")) | .organization.slug)
      |= replace_generated($organization_slug; "<organization-slug>")
  | (.states[] | select(.membership? | type == "object" and has("id")) | .membership.id)
      |= (replace_generated($owner_membership; "<owner-membership-id>")
          | replace_generated($invited_membership; "<invited-membership-id>"))
  | (.states[] | .team.members[]? | select(has("id")) | .id)
      |= (replace_generated($owner_membership; "<owner-membership-id>")
          | replace_generated($invited_membership; "<invited-membership-id>"))
  | (.states[] | .team.members[]? | select(has("userId")) | .userId)
      |= (replace_generated($owner_account; "<owner-account-id>")
          | replace_generated($invited_account; "<invited-account-id>"))
  | (.states[] | .team.invitations[]? | select(has("id")) | .id)
      |= replace_generated($invitation; "<invitation-id>")
  | (.states[] | .team.invitations[]? | select(has("expiresAt")) | .expiresAt)
      |= replace_generated($invitation_expiry; "<invitation-expiry>")
  | (.states[] | .team.invitations[]? | select(has("link")) | .link)
      |= replace_generated($invitation_link; "<invitation-link>");
  generated_ids'

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'normalization: generated identity-preserving account, organization, membership, invitation, slug, link, and expiry values only'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-state-comparison.json'
  exit 0
fi
if [ "${1:-}" = "--normalize" ] && [ "$#" -eq 2 ]; then
  jq -S "$normalize" "$2"
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan|--normalize FILE]\n' "$0" >&2
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
