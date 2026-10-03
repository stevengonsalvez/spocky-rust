#!/bin/sh
# Hunts for a rare failure of the spocky-message-receipts suite: runs
# `cargo test --locked -p spocky-message-receipts` repeatedly under CPU load and
# KEEPS the full output of every run, so a failing run names its test and its
# panic. Nothing is ever sent to /dev/null.
#
# Usage: scripts/phase3/receipts-flake-hunt.sh [runs] [load-loops]
#   runs        iterations, default 50 (each runs the lib, ported, differential
#               and doc tests)
#   load-loops  busy shell loops to run alongside, default the CPU count
#
# Output lands in evidence/raw/phase3/receipts-hunt-<utc>/ (untracked):
#   run-<n>.log       the whole output of run n, kept for every run
#   failures.txt      one line per failing run: its number, failing tests, and
#                     the first panic message
#   summary.txt       runs, failures, load loops, commit
# The busy loops are started here and their PIDs recorded in load.pids; only
# those PIDs are signalled, on exit. Exit 0 only when every run passed.
#
# Env: SPOCKY_PASEO_DIST (default: the p3_slice_harness build of 5de45e2),
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_message_receipts),
#      SPOCKY_BUILD_GATE (default /private/tmp/spocky-targets/build-gate.sh),
#      SPOCKY_HUNT_RUN_TIMEOUT (seconds per run, default 900).
set -eu
unset SPOCKY_ALLOW_SKIP

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

runs=${1:-50}
loops=${2:-$(sysctl -n hw.ncpu 2>/dev/null || nproc)}
case "$runs$loops" in
  '' | *[!0-9]*) p3_fail "usage: receipts-flake-hunt.sh [runs] [load-loops], both numbers" ;;
esac

dist=${SPOCKY_PASEO_DIST:-/private/tmp/spocky-targets/p3_slice_harness/paseo-original-$P3_PASEO_COMMIT/packages/server/dist/server}
[ -f "$dist/server/message-receipts/index.js" ] || p3_fail "missing pinned dist: $dist"
node_bin=$(p3_node_bin_dir)
export SPOCKY_PINNED_NODE="$node_bin/node"
export SPOCKY_PASEO_DIST="$dist"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_message_receipts}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}

out=$repository_root/evidence/raw/phase3/receipts-hunt-$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$(dirname "$out")"
mkdir "$out" || p3_fail "output directory already exists: $out"
: >"$out/failures.txt"
: >"$out/load.pids"

cd "$repository_root"
# Build once through the gate, so the loop below starts no compile.
"$gate" cargo test --locked -p spocky-message-receipts --no-run >"$out/build.log" 2>&1 ||
  p3_fail "build failed; see $out/build.log"

stop_load() {
  while read -r pid; do
    kill "$pid" 2>/dev/null || true
  done <"$out/load.pids"
}
trap stop_load EXIT HUP INT TERM
n=0
while [ "$n" -lt "$loops" ]; do
  sh -c 'while :; do :; done' &
  echo $! >>"$out/load.pids"
  n=$((n + 1))
done

failed=0
run=1
while [ "$run" -le "$runs" ]; do
  log=$out/run-$run.log
  if ! gtimeout --kill-after=30 "${SPOCKY_HUNT_RUN_TIMEOUT:-900}" cargo test --locked -p spocky-message-receipts >"$log" 2>&1; then
    failed=$((failed + 1))
    tests=$(sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$log" | tr '\n' ' ')
    panic=$(grep -m1 'panicked at' "$log" || true)
    printf 'run %s: tests [%s] %s\n' "$run" "$tests" "$panic" >>"$out/failures.txt"
    printf 'run %s FAILED, kept %s\n' "$run" "$log" >&2
  fi
  run=$((run + 1))
done

{
  printf 'commit %s\n' "$(git rev-parse HEAD)"
  printf 'runs %s\nfailures %s\nload-loops %s\n' "$runs" "$failed" "$loops"
} >"$out/summary.txt"
cat "$out/summary.txt"
printf 'output %s\n' "$out"
[ "$failed" -eq 0 ] || p3_fail "$failed of $runs runs failed; see $out/failures.txt"
