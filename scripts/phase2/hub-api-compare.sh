#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
original="$raw_dir/hub-api-original.json"
original_openapi="$raw_dir/hub-api-openapi-original.json"
rust="$raw_dir/hub-api-rust.json"
rust_openapi="$raw_dir/hub-api-openapi-rust.json"
comparison="$raw_dir/hub-api-comparison.json"
target_dir=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/hub-api}

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'comparison: byte-identical, normalization: none'
  printf '%s\n' 'evidence/raw/phase2/hub-api-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-api-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-api-openapi-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-api-openapi-rust.json'
  printf '%s\n' 'evidence/raw/phase2/hub-api-comparison.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
for required in "$original" "$original_openapi"; do
  if [ ! -f "$required" ]; then
    printf 'missing original evidence: %s\n' "$required" >&2
    exit 1
  fi
done
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub API comparison\n' >&2
  exit 1
fi

mkdir -p "$raw_dir"
rm -f "$rust" "$rust_openapi"
# The test also asserts equality with each baseline itself; a failure is reported after the byte
# comparison below so the first difference is shown.
test_failed=0
SPOCKY_HUB_API_BASELINE="$original" \
SPOCKY_HUB_API_OPENAPI_BASELINE="$original_openapi" \
SPOCKY_HUB_API_OUTPUT="$rust" \
SPOCKY_HUB_API_OPENAPI_OUTPUT="$rust_openapi" \
CARGO_TARGET_DIR="$target_dir" \
gtimeout 900 cargo test --quiet \
  --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot \
  --test hub_api_evidence || test_failed=1
for produced in "$rust" "$rust_openapi"; do
  if [ ! -f "$produced" ]; then
    printf 'Rust output was not written: %s\n' "$produced" >&2
    exit 1
  fi
done

# Byte comparison on purpose: object key order and number text are observable in Hub responses,
# so no key sorting or other normalization is applied.
trace_matched=false
if cmp -s "$original" "$rust"; then
  trace_matched=true
fi
openapi_matched=false
if cmp -s "$original_openapi" "$rust_openapi"; then
  openapi_matched=true
fi

jq -n \
  --argjson traceMatched "$trace_matched" \
  --argjson openapiMatched "$openapi_matched" \
  --arg originalSha256 "$(shasum -a 256 "$original" | awk '{print $1}')" \
  --arg rustSha256 "$(shasum -a 256 "$rust" | awk '{print $1}')" \
  --arg originalOpenapiSha256 "$(shasum -a 256 "$original_openapi" | awk '{print $1}')" \
  --arg rustOpenapiSha256 "$(shasum -a 256 "$rust_openapi" | awk '{print $1}')" \
  '{
    schemaVersion: 1,
    matched: ($traceMatched and $openapiMatched),
    comparison: "byte-identical",
    normalization: "none",
    traceMatched: $traceMatched,
    openapiMatched: $openapiMatched,
    originalRawSha256: $originalSha256,
    rustRawSha256: $rustSha256,
    originalOpenapiSha256: $originalOpenapiSha256,
    rustOpenapiSha256: $rustOpenapiSha256
  }' >"$comparison"

if [ "$trace_matched" != true ] || [ "$openapi_matched" != true ] || [ "$test_failed" != 0 ]; then
  diff -u "$original" "$rust" >&2 || true
  cmp "$original_openapi" "$rust_openapi" >&2 || true
  printf 'Hub API differential failed: %s\n' "$comparison" >&2
  exit 1
fi

printf 'Hub API differential passed: %s\n' "$comparison"
