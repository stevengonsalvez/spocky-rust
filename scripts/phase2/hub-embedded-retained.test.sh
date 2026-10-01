#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline_root=${PASEO_HUB_BASELINE_ROOT:-$repository_root/../../paseo-rust/.baselines/hub}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
  exit 1
fi
if [ -n "$(gtimeout 30 git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-retained.XXXXXX")
cleanup() {
  gtimeout 30 rm -rf "$fixture_root"
}
trap cleanup EXIT HUP INT TERM

gtimeout 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root"
(cd "$fixture_root" && gtimeout 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)

SPOCKY_NODE=$(gtimeout 30 command -v node) \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$fixture_root/node_modules/@electric-sql/pglite" \
SPOCKY_HUB_MIGRATIONS="$fixture_root/drizzle" \
  gtimeout 600 cargo test --locked --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-hub-pilot --test retained_pglite_runtime -- --nocapture --test-threads=1
