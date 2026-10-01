#!/bin/sh
# End-to-end spike: original JavaScript host versus the Rust PGlite host on
# the pinned package. Writes raw reports under evidence/raw (untracked) and
# prints the comparison. Heavy steps go through the shared build gate.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline_root=$(sh "$repository_root/scripts/phase2/hub-embedded-retained.test.sh" --print-baseline-root)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
node_executable=${SPOCKY_NODE:-/usr/local/Cellar/node/26.7.0/bin/node}
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/pglite-rust-host}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
  exit 1
fi

work=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-pglite-spike.XXXXXX")
cleanup() {
  gtimeout 60 rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

gtimeout 300 git -C "$baseline_root" archive -o "$work/baseline.tar" "$expected_baseline"
mkdir "$work/fixture"
gtimeout 60 tar -xf "$work/baseline.tar" -C "$work/fixture"
(cd "$work/fixture" && gtimeout 600 "$gate" npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
package="$work/fixture/node_modules/@electric-sql/pglite"

raw="$repository_root/evidence/raw/pglite-rust-host-spike"
mkdir -p "$raw"
gtimeout 600 "$node_executable" "$repository_root/scripts/phase2/pglite-rust-host-spike.mjs" \
  "$package" "$work/node-data" >"$raw/node.json"
cp -Rp "$work/node-data" "$work/node-copy"

gtimeout 2400 "$gate" cargo build --locked --release \
  --manifest-path "$repository_root/Cargo.toml" -p spocky-pglite-host --example spike
gtimeout 900 "$CARGO_TARGET_DIR/release/examples/spike" \
  "$package" "$work/rust-data" "$work/node-copy" >"$raw/rust.json"

gtimeout 120 "$node_executable" "$repository_root/scripts/phase2/pglite-rust-host-spike-compare.mjs" \
  "$raw/node.json" "$raw/rust.json" | tee "$raw/comparison.json"
