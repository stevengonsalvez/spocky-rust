#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
original="$raw_dir/hub-triggers-original.json"
rust="$raw_dir/hub-triggers-rust.json"
comparison="$raw_dir/hub-triggers-comparison.json"
target_dir=${CARGO_TARGET_DIR:-"$repository_root/.target-hub-triggers"}

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'normalization: none'
  printf '%s\n' 'evidence/raw/phase2/hub-triggers-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-triggers-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-triggers-comparison.json'
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
  printf 'gtimeout is required for bounded Hub trigger comparison\n' >&2
  exit 1
fi

mkdir -p "$raw_dir"
SPOCKY_HUB_TRIGGERS_OUTPUT="$rust" \
CARGO_TARGET_DIR="$target_dir" \
gtimeout 120 cargo test --quiet \
  --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot \
  --test hub_triggers_evidence

work_dir=$(mktemp -d /private/tmp/spocky-hub-triggers-compare.XXXXXX)
cleanup() {
  case "$work_dir" in
    /private/tmp/spocky-hub-triggers-compare.*) rm -rf "$work_dir" ;;
    *) printf 'refusing unexpected comparison directory: %s\n' "$work_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

jq -S . "$original" >"$work_dir/original.json"
jq -S . "$rust" >"$work_dir/rust.json"
original_sha=$(shasum -a 256 "$original" | awk '{print $1}')
rust_sha=$(shasum -a 256 "$rust" | awk '{print $1}')
matched=false
if cmp -s "$work_dir/original.json" "$work_dir/rust.json"; then
  matched=true
fi

jq -n \
  --argjson matched "$matched" \
  --arg originalSha256 "$original_sha" \
  --arg rustSha256 "$rust_sha" \
  --slurpfile originalTrace "$work_dir/original.json" \
  --slurpfile rustTrace "$work_dir/rust.json" \
  '{
    schemaVersion: 1,
    matched: $matched,
    normalization: "none",
    originalRawSha256: $originalSha256,
    rustRawSha256: $rustSha256,
    originalTrace: $originalTrace[0],
    rustTrace: $rustTrace[0]
  }' >"$comparison"

if [ "$matched" != true ]; then
  diff -u "$work_dir/original.json" "$work_dir/rust.json" >&2 || true
  printf 'Hub trigger differential failed: %s\n' "$comparison" >&2
  exit 1
fi

printf 'Hub trigger differential passed: %s\n' "$comparison"
