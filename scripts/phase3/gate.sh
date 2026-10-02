#!/bin/sh
# Runs a Phase 3 slice gate with the unchanged pinned Paseo CLI and the real
# pinned codex against a scripted loopback Responses stub.
#
# Usage: scripts/phase3/gate.sh <gate> [--self-check-only]
#
# Always first runs the gate with the original daemon on both sides, which
# must pass with zero mismatch (proof the harness reports no false mismatch).
# Then, unless --self-check-only, runs original against Spocky
# (target/debug/spocky-daemon), which must also pass. Exit 0 only when every
# run passes. Evidence lands in evidence/raw/phase3/<gate>-<utc>/ (untracked)
# and its digests are printed.
#
# Env: CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_slice_harness),
#      CARGO_BUILD_JOBS (default 2), SPOCKY_BUILD_GATE (default
#      /private/tmp/spocky-targets/build-gate.sh).
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

[ $# -ge 1 ] || p3_fail "usage: scripts/phase3/gate.sh <gate> [--self-check-only]"
gate=$1
shift
self_check_only=false
case "${1:-}" in
  '') ;;
  --self-check-only) self_check_only=true; shift ;;
  *) p3_fail "unknown option: $1" ;;
esac
[ $# -eq 0 ] || p3_fail "unexpected arguments: $*"
case "$gate" in
  g1 | g2) ;;
  *) p3_fail "unsupported gate: $gate (defined gates: g1, g2)" ;;
esac

CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_slice_harness}
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_TARGET_DIR CARGO_BUILD_JOBS
build_gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}
[ -x "$build_gate" ] || p3_fail "missing heavy-command gate: $build_gate"

# Strip every inherited PASEO_* variable so nothing ambient steers the client.
for name in $(env | sed -n 's/^\(PASEO_[A-Za-z0-9_]*\)=.*/\1/p'); do
  unset "$name"
done

node_bin=$(p3_node_bin_dir)
codex=$(p3_codex_binary)
p3_paseo_baseline "$repository_root" >/dev/null
paseo_root=$("$repository_root/scripts/phase3/build-original.sh")

(cd "$repository_root" &&
  "$build_gate" gtimeout --kill-after=30 900 \
    cargo build --locked -p spocky-slice-harness --bins) >&2
stub=$CARGO_TARGET_DIR/debug/spocky-responses-stub
runner=$CARGO_TARGET_DIR/debug/spocky-slice-gate
[ -x "$stub" ] && [ -x "$runner" ] || p3_fail "harness binaries missing under $CARGO_TARGET_DIR/debug"

spocky_daemon=

run_id=$gate-$(date -u +%Y%m%dT%H%M%SZ)
evidence=$repository_root/evidence/raw/phase3/$run_id
mkdir -p "$evidence"
printf 'gate %s evidence: %s\n' "$gate" "$evidence"

run_pair() {
  pair_name=$1
  right=$2
  set -- "$runner" "$gate" --left original --right "$right" \
    --paseo-root "$paseo_root" --node-bin "$node_bin" --codex "$codex" \
    --stub "$stub" --evidence "$evidence/$pair_name"
  if [ "$right" = spocky ]; then
    set -- "$@" --spocky-daemon "$spocky_daemon"
  fi
  pair_status=0
  "$@" || pair_status=$?
  printf '%s %s exit %s\n' "$gate" "$pair_name" "$pair_status"
  for file in verdict.json manifest.json rules.json transforms.json; do
    if [ -f "$evidence/$pair_name/$file" ]; then
      printf '  sha256 %s  %s\n' "$(p3_sha256 "$evidence/$pair_name/$file")" "$pair_name/$file"
    fi
  done
  return "$pair_status"
}

overall=0
run_pair self-check original || overall=1
if [ "$self_check_only" = false ]; then
  if [ "$overall" -eq 0 ]; then
    if (cd "$repository_root" &&
      "$build_gate" gtimeout --kill-after=30 900 \
        cargo build --locked -p spocky-daemon-app --bin spocky-daemon) >&2 &&
      [ -x "$CARGO_TARGET_DIR/debug/spocky-daemon" ]; then
      spocky_daemon=$CARGO_TARGET_DIR/debug/spocky-daemon
      run_pair parity spocky || overall=1
    else
      printf '%s parity blocked: package spocky-daemon-app has no buildable bin target spocky-daemon\n' "$gate" >&2
      overall=1
    fi
  else
    printf '%s parity skipped: self-check failed, so a parity verdict would be meaningless\n' "$gate" >&2
  fi
fi
exit "$overall"
