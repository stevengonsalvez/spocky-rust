#!/bin/sh
# Runs the p3_message_receipts acceptance commands and keeps their evidence:
# the pinned-build differential's raw and normalized outputs (both sides),
# each command's log, and the digests of the pinned dist modules under test.
#
# Usage: scripts/phase3/receipts-differential.sh
#
# Evidence lands in evidence/raw/phase3/receipts-<utc>/ (untracked) and its
# SHA-256 digests are printed. Exit 0 only when test, clippy, and fmt pass and
# both sides' normalized differential outputs exist and are byte-identical.
# SPOCKY_ALLOW_SKIP is always unset, so the differential can never skip.
# The run also fails when the worktree has any change, tracked or untracked,
# outside logs/ (session hook output), so evidence always names a clean
# commit; the full `git status --porcelain` is kept in git-status.txt.
# scripts/phase3/receipts-differential.test.sh proves every failure exits
# nonzero.
#
# Env: SPOCKY_PASEO_DIST (default: the p3_slice_harness build of 5de45e2),
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_message_receipts),
#      CARGO_BUILD_JOBS (default 2), SPOCKY_BUILD_GATE (default
#      /private/tmp/spocky-targets/build-gate.sh), SPOCKY_RECEIPTS_EVIDENCE_ROOT
#      (default evidence/raw/phase3).
set -eu
unset SPOCKY_ALLOW_SKIP

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
evidence=${SPOCKY_RECEIPTS_EVIDENCE_ROOT:-$repository_root/evidence/raw/phase3}/$run_id
mkdir -p "$(dirname "$evidence")"
# A fresh directory, so outputs of an earlier run can never pass this one.
mkdir "$evidence" || p3_fail "evidence directory already exists: $evidence"
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
git status --porcelain --untracked-files=all >"$evidence/git-status.txt"
if [ -n "$(git status --porcelain --untracked-files=all -- . ':(exclude)logs')" ]; then
  printf 'worktree is not clean; see %s\n' "$evidence/git-status.txt" >&2
  status=1
fi
"$gate" cargo test --locked -p spocky-message-receipts >"$evidence/test.log" 2>&1 || status=1
"$gate" cargo clippy --locked -p spocky-message-receipts --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 || status=1
"$gate" cargo fmt --package spocky-message-receipts -- --check >"$evidence/fmt.log" 2>&1 || status=1
node_normalized=$evidence/receipts-node-normalized.json
rust_normalized=$evidence/receipts-rust-normalized.json
if [ ! -s "$node_normalized" ] || [ ! -s "$rust_normalized" ]; then
  printf 'missing differential output in %s\n' "$evidence" >&2
  status=1
elif ! cmp -s "$node_normalized" "$rust_normalized"; then
  printf 'normalized differential outputs differ in %s\n' "$evidence" >&2
  status=1
fi

grep -E '^test result|^test ' "$evidence/test.log" || true
printf 'evidence %s\n' "$evidence"
for file in "$evidence"/*; do
  [ -f "$file" ] || continue
  printf '%s  %s\n' "$(p3_sha256 "$file")" "${file#"$repository_root"/}"
done
[ "$status" -eq 0 ] || p3_fail "$run_id failed; see $evidence"
printf '%s passed\n' "$run_id"
