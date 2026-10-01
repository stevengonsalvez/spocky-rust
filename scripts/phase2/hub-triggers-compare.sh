#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
committed_dir="$repository_root/evidence/phase2"
comparison="$raw_dir/hub-triggers-comparison.json"
target_dir=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/hub-triggers}
# One entry per host time zone: <TZ>:<original capture>:<Rust trace>. The Rust trace is produced
# with TZ set to the same zone, so the store reads its zone from the host like the baseline does.
runs="UTC:hub-triggers-original.json:hub-triggers-rust.json Europe/London:hub-triggers-original-europe-london.json:hub-triggers-rust-europe-london.json"

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' 'comparison: byte-identical, normalization: none'
  printf '%s\n' 'raw original capture is cmp-ed against the committed evidence/phase2 original'
  printf '%s\n' 'SPOCKY_HUB_TRIGGERS_RECAPTURE=1 reruns hub-triggers-capture.sh first'
  for run in $runs; do
    zone=${run%%:*}
    rest=${run#*:}
    printf '%s\n' "TZ=$zone evidence/raw/phase2/${rest%%:*} evidence/raw/phase2/${rest#*:}"
  done
  printf '%s\n' 'evidence/raw/phase2/hub-triggers-comparison.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub trigger comparison\n' >&2
  exit 1
fi

if [ "${SPOCKY_HUB_TRIGGERS_RECAPTURE:-}" = 1 ]; then
  sh "$repository_root/scripts/phase2/hub-triggers-capture.sh"
fi

mkdir -p "$raw_dir"
entries='[]'
failed=0
for run in $runs; do
  zone=${run%%:*}
  rest=${run#*:}
  original="$raw_dir/${rest%%:*}"
  committed="$committed_dir/${rest%%:*}"
  rust="$raw_dir/${rest#*:}"
  if [ ! -f "$original" ]; then
    printf 'missing original capture: %s (run hub-triggers-capture.sh)\n' "$original" >&2
    exit 1
  fi
  if [ ! -f "$committed" ]; then
    printf 'missing committed evidence: %s\n' "$committed" >&2
    exit 1
  fi
  rm -f "$rust"

  # The capture the Rust trace is compared with must be the capture that is committed as evidence.
  committed_matched=false
  if cmp -s "$original" "$committed"; then
    committed_matched=true
  else
    printf 'raw original differs from committed evidence: %s\n' "$committed" >&2
    diff -u "$committed" "$original" >&2 || true
    failed=1
  fi

  # The test also asserts equality with the baseline trace itself; a failure is reported after the
  # byte diff below.
  test_failed=0
  TZ="$zone" \
  SPOCKY_HUB_TRIGGERS_BASELINE="$original" \
  SPOCKY_HUB_TRIGGERS_OUTPUT="$rust" \
  CARGO_TARGET_DIR="$target_dir" \
  gtimeout 900 cargo test --quiet \
    --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-hub-pilot \
    --test hub_triggers_evidence || test_failed=1
  if [ ! -f "$rust" ]; then
    printf 'Rust trace was not written: %s\n' "$rust" >&2
    exit 1
  fi

  # Byte comparison on purpose: object key order is observable in Hub response bodies and traces,
  # so no key sorting or other normalization is applied.
  original_sha=$(shasum -a 256 "$original" | awk '{print $1}')
  rust_sha=$(shasum -a 256 "$rust" | awk '{print $1}')
  matched=false
  if cmp -s "$original" "$rust"; then
    matched=true
  else
    diff -u "$original" "$rust" >&2 || true
  fi
  if [ "$matched" != true ] || [ "$test_failed" != 0 ]; then
    failed=1
  fi
  entries=$(jq -n \
    --argjson entries "$entries" \
    --arg timeZone "$zone" \
    --argjson matched "$matched" \
    --argjson committedMatched "$committed_matched" \
    --arg originalSha256 "$original_sha" \
    --arg rustSha256 "$rust_sha" \
    '$entries + [{
      timeZone: $timeZone,
      matched: $matched,
      originalMatchesCommittedEvidence: $committedMatched,
      originalRawSha256: $originalSha256,
      rustRawSha256: $rustSha256
    }]')
done

jq -n \
  --argjson entries "$entries" \
  '{
    schemaVersion: 2,
    matched: ($entries | all(.matched and .originalMatchesCommittedEvidence)),
    comparison: "byte-identical",
    normalization: "none",
    zones: $entries
  }' >"$comparison"

if [ "$failed" != 0 ]; then
  printf 'Hub trigger differential failed: %s\n' "$comparison" >&2
  exit 1
fi

printf 'Hub trigger differential passed: %s\n' "$comparison"
