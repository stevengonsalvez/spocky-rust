#!/bin/sh
# Builds the pinned Paseo CLI and daemon (git archive of .baselines/paseo-runtime
# at the pinned commit) with Node 22.20.0 into a disposable build root outside
# every git worktree. Never writes into the baseline checkout. Prints the build
# root on success. A completed root carries .spocky-build with the commit, lock,
# and Node digests; a root whose marker differs is rebuilt from scratch.
#
# Usage: scripts/phase3/build-original.sh
# Env:   SPOCKY_P3_BUILD_PARENT  parent of build roots
#                                (default /private/tmp/spocky-targets/p3_slice_harness)
#        SPOCKY_BUILD_GATE       heavy-command gate (default
#                                /private/tmp/spocky-targets/build-gate.sh)
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

build_parent=${SPOCKY_P3_BUILD_PARENT:-/private/tmp/spocky-targets/p3_slice_harness}
build_gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}
[ -x "$build_gate" ] || p3_fail "missing heavy-command gate: $build_gate"

baseline=$(p3_paseo_baseline "$repository_root")
node_bin=$(p3_node_bin_dir)
build_root=$build_parent/paseo-original-$P3_PASEO_COMMIT
marker_expected=$(printf 'commit=%s\nlock=%s\nnode=%s\n' \
  "$P3_PASEO_COMMIT" "$P3_PASEO_LOCK_SHA256" "$P3_NODE_BINARY_SHA256")

if [ -f "$build_root/.spocky-build" ] &&
  [ "$(cat "$build_root/.spocky-build")" = "$marker_expected" ] &&
  [ -f "$build_root/packages/cli/dist/index.js" ]; then
  printf '%s\n' "$build_root"
  exit 0
fi

mkdir -p "$build_parent"
staging=$(mktemp -d "$build_parent/paseo-original-staging.XXXXXX")
cleanup() {
  case "$staging" in
    "$build_parent"/paseo-original-staging.*) rm -rf "$staging" ;;
    *) printf 'refusing to remove unexpected staging directory: %s\n' "$staging" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline" archive "$P3_PASEO_COMMIT" | tar -x -C "$staging"
[ "$(p3_sha256 "$staging/package-lock.json")" = "$P3_PASEO_LOCK_SHA256" ] ||
  p3_fail "archived package-lock.json digest mismatch"

log=$staging/.spocky-build.log
(
  cd "$staging"
  PATH=$node_bin:$PATH
  export PATH
  "$build_gate" gtimeout --kill-after=30 1800 \
    npm ci --ignore-scripts --no-audit --no-fund
  PATH="$staging/node_modules/.bin:$PATH" node scripts/postinstall-patches.mjs
  "$build_gate" gtimeout --kill-after=30 1800 npm run build:server
) >"$log" 2>&1 || {
  tail -n 40 "$log" >&2
  p3_fail "pinned Paseo build failed; full log above was in $log"
}
[ -f "$staging/packages/cli/dist/index.js" ] ||
  p3_fail "pinned Paseo build produced no packages/cli/dist/index.js"
printf '%s\n' "$marker_expected" >"$staging/.spocky-build"

case "$build_root" in
  "$build_parent"/paseo-original-"$P3_PASEO_COMMIT") rm -rf "$build_root" ;;
  *) p3_fail "refusing to replace unexpected build root: $build_root" ;;
esac
mv "$staging" "$build_root"
trap - EXIT HUP INT TERM
printf '%s\n' "$build_root"
