#!/bin/sh
# Runs the p3_message_receipts acceptance commands and keeps their evidence:
# the pinned-build differential's raw and normalized outputs (both sides),
# each command's log, and the digests of the pinned dist modules under test.
#
# Usage: scripts/phase3/receipts-differential.sh
#
# Evidence lands in evidence/raw/phase3/receipts-<utc>/ (untracked) and its
# SHA-256 digests are printed. Exit 0 only when test, clippy, and fmt pass.
#
# Env: SPOCKY_PASEO_DIST (default: the p3_slice_harness build of 5de45e2),
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_message_receipts),
#      CARGO_BUILD_JOBS (default 2), SPOCKY_BUILD_GATE (default
#      /private/tmp/spocky-targets/build-gate.sh).
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

dist=${SPOCKY_PASEO_DIST:-/private/tmp/spocky-targets/p3_slice_harness/paseo-original-$P3_PASEO_COMMIT/packages/server/dist/server}
receipts_module=$dist/server/message-receipts/index.js
atomic_module=$dist/server/atomic-file.js
[ -f "$receipts_module" ] || p3_fail "missing pinned dist module: $receipts_module (build it with scripts/phase3/build-original.sh)"
[ -f "$atomic_module" ] || p3_fail "missing pinned dist module: $atomic_module"
node_bin=$(p3_node_bin_dir)

export SPOCKY_PINNED_NODE="$node_bin/node"
export SPOCKY_PASEO_DIST="$dist"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_message_receipts}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}

run_id=receipts-$(date -u +%Y%m%dT%H%M%SZ)
evidence=$repository_root/evidence/raw/phase3/$run_id
mkdir -p "$evidence"
export SPOCKY_RECEIPTS_EVIDENCE="$evidence"

{
  printf 'commit %s\n' "$(git -C "$repository_root" rev-parse HEAD)"
  printf 'node %s %s\n' "$("$SPOCKY_PINNED_NODE" --version)" "$SPOCKY_PINNED_NODE"
  printf 'dist %s\n' "$dist"
  printf 'module %s %s\n' "$(p3_sha256 "$receipts_module")" "$receipts_module"
  printf 'module %s %s\n' "$(p3_sha256 "$atomic_module")" "$atomic_module"
} >"$evidence/inputs.txt"

cd "$repository_root"
status=0
"$gate" cargo test --locked -p spocky-message-receipts >"$evidence/test.log" 2>&1 || status=1
"$gate" cargo clippy --locked -p spocky-message-receipts --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 || status=1
cargo fmt --package spocky-message-receipts -- --check >"$evidence/fmt.log" 2>&1 || status=1

grep -E '^test result|^test ' "$evidence/test.log" || true
printf 'evidence %s\n' "$evidence"
for file in "$evidence"/*; do
  printf '%s  %s\n' "$(p3_sha256 "$file")" "${file#"$repository_root"/}"
done
[ "$status" -eq 0 ] || p3_fail "$run_id failed; see $evidence"
printf '%s passed\n' "$run_id"
