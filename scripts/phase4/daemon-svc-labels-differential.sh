#!/bin/sh
# Runs the p4_daemon_services workspace-label acceptance commands and keeps
# their evidence: the pinned-build differential's raw and normalized traces
# (both sides), each command's log, and the digests of the pinned dist modules.
#
# Usage: scripts/phase4/daemon-svc-labels-differential.sh
#
# Evidence lands in evidence/raw/phase4/labels-<utc>/ (untracked). Exit 0 only
# when test, clippy and fmt pass and both sides' normalized traces exist and
# are byte-identical. SPOCKY_ALLOW_SKIP is always unset, so the differential
# can never skip. The run also fails when the worktree has any change outside
# logs/, so evidence always names a clean commit.
#
# Env: SPOCKY_PASEO_DIST (default: the p3_slice_harness build of 5de45e2),
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p4_daemon_services),
#      CARGO_BUILD_JOBS (default 2), SPOCKY_BUILD_GATE (default
#      /private/tmp/spocky-targets/build-gate.sh).
set -eu
unset SPOCKY_ALLOW_SKIP
export GIT_OPTIONAL_LOCKS=0

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

dist=${SPOCKY_PASEO_DIST:-/private/tmp/spocky-targets/p3_slice_harness/paseo-original-$P3_PASEO_COMMIT/packages/server/dist/server}
node_bin=$(p3_node_bin_dir)
modules="server/workspace-labels/index.js
server/workspace-labels/internal/catalog-store.js
server/workspace-labels/internal/sequence.js
server/workspace-labels/internal/service.js
server/workspace-registry.js
server/atomic-file.js"
for module in $modules; do
  [ -f "$dist/$module" ] || p3_fail "missing pinned dist module: $dist/$module (build it with scripts/phase3/build-original.sh)"
done

export SPOCKY_PINNED_NODE="$node_bin/node"
export SPOCKY_PASEO_DIST="$dist"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p4_daemon_services}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_INCREMENTAL=0
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}

run_id=labels-$(date -u +%Y%m%dT%H%M%SZ)
evidence=$repository_root/evidence/raw/phase4/$run_id
mkdir -p "$(dirname "$evidence")"
# A fresh directory, so outputs of an earlier run can never pass this one.
mkdir "$evidence" || p3_fail "evidence directory already exists: $evidence"
export SPOCKY_LABELS_EVIDENCE="$evidence"

{
  printf 'commit %s\n' "$(git -C "$repository_root" rev-parse HEAD)"
  printf 'node %s %s\n' "$("$SPOCKY_PINNED_NODE" --version)" "$SPOCKY_PINNED_NODE"
  printf 'dist %s\n' "$dist"
  for module in $modules; do
    printf 'module %s %s\n' "$(p3_sha256 "$dist/$module")" "$dist/$module"
  done
} >"$evidence/inputs.txt"

cd "$repository_root"
status=0
git status --porcelain --untracked-files=all >"$evidence/git-status.txt"
if [ -n "$(git status --porcelain --untracked-files=all -- . ':(exclude)logs')" ]; then
  printf 'worktree is not clean; see %s\n' "$evidence/git-status.txt" >&2
  status=1
fi
"$gate" cargo test --locked -p spocky-workspace-labels >"$evidence/test.log" 2>&1 || status=1
"$gate" cargo clippy --locked -p spocky-workspace-labels --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 || status=1
"$gate" cargo fmt --package spocky-workspace-labels -- --check >"$evidence/fmt.log" 2>&1 || status=1
node_normalized=$evidence/labels-node-normalized.txt
rust_normalized=$evidence/labels-rust-normalized.txt
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
