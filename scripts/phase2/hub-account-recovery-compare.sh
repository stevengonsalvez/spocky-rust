#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
original="$raw_dir/hub-account-recovery-original.json"
rust="$raw_dir/hub-account-recovery-rust.json"
comparison="$raw_dir/hub-account-recovery-comparison.json"

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'normalization: generated verification token, password-reset token, and session-cookie value only'
  printf '%s\n' 'evidence/raw/phase2/hub-account-recovery-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-recovery-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-account-recovery-comparison.json'
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
  printf 'gtimeout is required for bounded candidate recovery capture\n' >&2
  exit 1
fi

gtimeout 120 cargo run -q --manifest-path "$repository_root/Cargo.toml" \
  -p paseo-hub-pilot --bin hub-account-recovery-evidence >"$rust"

work_dir=$(mktemp -d /private/tmp/paseo-hub-recovery-compare.XXXXXX)
cleanup() {
  case "$work_dir" in
    /private/tmp/paseo-hub-recovery-compare.*) rm -rf "$work_dir" ;;
    *) printf 'refusing unexpected comparison directory: %s\n' "$work_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

normalize='del(.baseline)
  | .verificationEmail.token = "<verification-token>"
  | .verificationEmail.url |= sub("token=[^&]+"; "token=<verification-token>")
  | .verification.setCookie |= sub("better-auth.session_token=[^;]+"; "better-auth.session_token=<session-token>")
  | .passwordResetEmails[0].token = "<password-reset-token>"
  | .passwordResetEmails[0].url |= sub("reset-password/[^?]+"; "reset-password/<password-reset-token>")
  | .resetCallback.location |= sub("token=.*$"; "token=<password-reset-token>")'

jq -S "$normalize" "$original" >"$work_dir/original.json"
jq -S "$normalize" "$rust" >"$work_dir/rust.json"
original_sha=$(shasum -a 256 "$original" | awk '{print $1}')
rust_sha=$(shasum -a 256 "$rust" | awk '{print $1}')
matched=false
if cmp -s "$work_dir/original.json" "$work_dir/rust.json"; then matched=true; fi

jq -n \
  --argjson matched "$matched" \
  --arg originalSha256 "$original_sha" \
  --arg rustSha256 "$rust_sha" \
  --slurpfile originalNormalized "$work_dir/original.json" \
  --slurpfile rustNormalized "$work_dir/rust.json" \
  '{schemaVersion: 1, matched: $matched,
    normalization: "generated verification token, password-reset token, and session-cookie value only",
    originalRawSha256: $originalSha256, rustRawSha256: $rustSha256,
    originalNormalized: $originalNormalized[0], rustNormalized: $rustNormalized[0]}' >"$comparison"

if [ "$matched" != true ]; then
  diff -u "$work_dir/original.json" "$work_dir/rust.json" >&2 || true
  printf 'Hub account recovery differential failed: %s\n' "$comparison" >&2
  exit 1
fi
printf 'Hub account recovery differential passed: %s\n' "$comparison"
