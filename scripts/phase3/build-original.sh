#!/bin/sh
# Builds the pinned Paseo CLI and daemon (git archive of .baselines/paseo-runtime
# at the pinned commit) with Node 22.20.0 into a disposable build root outside
# every git worktree. Never writes into the baseline checkout. Prints the build
# root on success.
#
# `npm ci` runs with lifecycle scripts, as a real checkout install does, so
# native and binary packages (node-pty, sharp, esbuild, workerd) are installed
# and the original daemon is not degraded. The staging copy is git-initialized
# because the root `prepare` script installs lefthook hooks into it.
#
# A completed root carries .spocky-build with the commit, lock, Node digests,
# and install mode; a root whose marker differs is rebuilt from scratch. One
# build at a time per root, guarded by a lock directory holding the owner PID.
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
lock=$build_root.lock
marker_expected=$(printf 'commit=%s\nlock=%s\nnode=%s\ninstall=lifecycle-scripts\n' \
  "$P3_PASEO_COMMIT" "$P3_PASEO_LOCK_SHA256" "$P3_NODE_BINARY_SHA256")

complete() {
  [ -f "$build_root/.spocky-build" ] &&
    [ "$(cat "$build_root/.spocky-build")" = "$marker_expected" ] &&
    [ -f "$build_root/packages/cli/dist/index.js" ]
}

if complete; then
  printf '%s\n' "$build_root"
  exit 0
fi

mkdir -p "$build_parent"
staging=
held_lock=false
cleanup() {
  if [ -n "$staging" ]; then
    case "$staging" in
      "$build_parent"/paseo-original-staging.*) rm -rf "$staging" ;;
      *) printf 'refusing to remove unexpected staging directory: %s\n' "$staging" >&2 ;;
    esac
  fi
  if [ "$held_lock" = true ]; then
    rm -rf "$lock"
  fi
}
trap cleanup EXIT
trap 'cleanup; trap - EXIT; exit 130' INT
trap 'cleanup; trap - EXIT; exit 143' TERM HUP

# Take the per-root lock; a lock whose owner PID is gone is stale.
waited=0
until mkdir "$lock" 2>/dev/null; do
  owner=$(cat "$lock/pid" 2>/dev/null || true)
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    rm -rf "$lock"
    continue
  fi
  [ "$waited" -lt 3600 ] || p3_fail "build lock $lock still held after 3600 s by PID ${owner:-unknown}"
  sleep 5
  waited=$((waited + 5))
done
held_lock=true
printf '%s\n' "$$" >"$lock/pid"

# Another process may have finished the build while this one waited.
if complete; then
  printf '%s\n' "$build_root"
  exit 0
fi

staging=$(mktemp -d "$build_parent/paseo-original-staging.XXXXXX")
archive=$staging.tar
git -C "$baseline" archive --format=tar -o "$archive" "$P3_PASEO_COMMIT" ||
  p3_fail "git archive of $P3_PASEO_COMMIT failed"
tar -x -f "$archive" -C "$staging" || p3_fail "extracting $archive failed"
rm -f "$archive"
[ "$(p3_sha256 "$staging/package-lock.json")" = "$P3_PASEO_LOCK_SHA256" ] ||
  p3_fail "archived package-lock.json digest mismatch"

log=$staging/.spocky-build.log
(
  cd "$staging"
  PATH=$node_bin:$PATH
  export PATH
  git init -q .
  "$build_gate" gtimeout --kill-after=30 2400 npm ci --no-audit --no-fund
  "$build_gate" gtimeout --kill-after=30 1800 npm run build:server
) >"$log" 2>&1 || {
  tail -n 40 "$log" >&2
  p3_fail "pinned Paseo build failed; log was $log"
}
[ -f "$staging/packages/cli/dist/index.js" ] ||
  p3_fail "pinned Paseo build produced no packages/cli/dist/index.js"
printf '%s\n' "$marker_expected" >"$staging/.spocky-build"

case "$build_root" in
  "$build_parent"/paseo-original-"$P3_PASEO_COMMIT") rm -rf "$build_root" ;;
  *) p3_fail "refusing to replace unexpected build root: $build_root" ;;
esac
mv "$staging" "$build_root"
staging=
printf '%s\n' "$build_root"
