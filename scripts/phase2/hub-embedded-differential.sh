#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
pglite_version=0.5.4
pglite_integrity='sha512-yYZUyyXrHU7tPlCjwZQJ6hIG9DscdCCn7Uk0mYKwC1FeHX286AbcmFveMiRBEak8e9iPupjsoVImN3yJZVed2g=='

baseline_root=${PASEO_HUB_BASELINE_ROOT:-}
if [ -z "$baseline_root" ]; then
  if [ -d "$repository_root/.baselines/hub" ]; then
    baseline_root="$repository_root/.baselines/hub"
  else
    baseline_root="$repository_root/../../paseo-rust/.baselines/hub"
  fi
fi

preflight() {
  actual=$(git -C "$baseline_root" rev-parse HEAD)
  if [ "$actual" != "$expected_baseline" ]; then
    printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
    exit 1
  fi
  if [ -n "$(git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
    printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
    exit 1
  fi
  locked_version=$(jq -r '.packages["node_modules/@electric-sql/pglite"].version' \
    "$baseline_root/package-lock.json")
  locked_integrity=$(jq -r '.packages["node_modules/@electric-sql/pglite"].integrity' \
    "$baseline_root/package-lock.json")
  if [ "$locked_version" != "$pglite_version" ] || [ "$locked_integrity" != "$pglite_integrity" ]; then
    printf 'PGlite package lock mismatch in pinned Hub baseline\n' >&2
    exit 1
  fi
}

compare_json() {
  pglite_json=$1
  rust_json=$2
  jq -n --slurpfile pglite "$pglite_json" --slurpfile rust "$rust_json" '
    def mismatch($name): select($pglite[0].boundary[$name] != $rust[0].boundary[$name]) | $name;
    {
      observableParity: ($pglite[0].operations == $rust[0].operations),
      exactDatabaseParity: ($pglite[0] == $rust[0]),
      operations: {pglite: $pglite[0].operations, rust: $rust[0].operations},
      observations: {pglite: $pglite[0].observations, rust: $rust[0].observations},
      mismatches: [mismatch("engine"), mismatch("schema"), mismatch("dialect"), mismatch("migrations")],
      boundary: {pglite: $pglite[0].boundary, rust: $rust[0].boundary}
    }'
}

case "${1:-}" in
  --print-plan)
    printf '%s\n' \
      "Hub baseline: $expected_baseline" \
      "PGlite package: $pglite_version" \
      'operations: restart, cross-process rejection, transaction rollback, same-key serialization, stale-owner recovery' \
      'observations: tables, constraints, migration journal, lock owner record' \
      'expected mismatch: engine, schema, dialect, migrations'
    exit 0
    ;;
  --preflight-only)
    preflight
    printf 'Hub baseline preflight passed: %s\n' "$expected_baseline"
    exit 0
    ;;
  --compare-json)
    if [ "$#" -ne 3 ]; then
      printf 'usage: %s --compare-json PGLITE_JSON RUST_JSON\n' "$0" >&2
      exit 2
    fi
    compare_json "$2" "$3"
    exit 0
    ;;
  "") ;;
  *)
    printf 'usage: %s [--print-plan|--preflight-only|--compare-json PGLITE_JSON RUST_JSON]\n' "$0" >&2
    exit 2
    ;;
esac

preflight
if [ -n "${PASEO_HUB_EMBEDDED_OUTPUT:-}" ]; then
  output_root=$PASEO_HUB_EMBEDDED_OUTPUT
  mkdir "$output_root"
else
  mkdir -p "$repository_root/target/phase2"
  output_root=$(mktemp -d "$repository_root/target/phase2/hub-embedded-differential.XXXXXX")
fi
mkdir -p "$output_root/pglite-db" "$output_root/rust-db"
build_root=$(mktemp -d "${TMPDIR:-/tmp}/paseo-hub-embedded-original.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM
git -C "$baseline_root" archive "$expected_baseline" | tar -x -C "$build_root"
(cd "$build_root" && npm ci --ignore-scripts --no-audit --no-fund >/dev/null)
PASEO_HUB_SOURCE_ROOT="$build_root" \
PASEO_HUB_TSX="$build_root/node_modules/.bin/tsx" \
"$build_root/node_modules/.bin/tsx" "$repository_root/scripts/phase2/hub-embedded-pglite.mjs" \
  capture "$output_root/pglite-db" >"$output_root/pglite.json"

cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p paseo-hub-pilot --bin hub-embedded-evidence >/dev/null
"$repository_root/target/debug/hub-embedded-evidence" capture "$output_root/rust-db" \
  >"$output_root/rust.json"

compare_json "$output_root/pglite.json" "$output_root/rust.json" \
  >"$output_root/comparison.json"
jq -e '.observableParity == true and .exactDatabaseParity == false' \
  "$output_root/comparison.json" >/dev/null
jq -e '.mismatches == ["engine", "schema", "dialect", "migrations"]' \
  "$output_root/comparison.json" >/dev/null
cat "$output_root/comparison.json"
shasum -a 256 "$output_root/pglite.json" "$output_root/rust.json" "$output_root/comparison.json"
