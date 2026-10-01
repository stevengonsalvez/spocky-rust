#!/bin/sh
# Proves scripts/phase3/receipts-differential.sh exits nonzero on every
# failure and zero only on a clean match:
#
# - A stub build gate fakes the cargo commands to cover a matching run (with
#   SPOCKY_ALLOW_SKIP=1 exported, which the script must unset), mismatched
#   normalized outputs, missing outputs, a failing test with matching
#   outputs, failing clippy, failing fmt, and an untracked file in the tree.
# - A real run through the build gate against a dist whose index.js differs
#   from the pinned build must fail on the pinned digest.
#
# Every case runs the committed script from a throwaway detached worktree at
# HEAD, so the clean-tree check sees a clean tree whatever this checkout
# holds. The worktree is removed on exit.
#
# Usage: scripts/phase3/receipts-differential.test.sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"
pinned_dist=/private/tmp/spocky-targets/p3_slice_harness/paseo-original-$P3_PASEO_COMMIT/packages/server/dist/server

work=$(mktemp -d /tmp/spocky-receipts-differential-test.XXXXXX)
cleanup() {
  if [ -d "$work/tree" ]; then
    git -C "$repository_root" worktree remove --force "$work/tree" || true
  fi
  case "$work" in
    /tmp/spocky-receipts-differential-test.*) rm -rf "$work" ;;
    *) printf 'refusing to remove unexpected test directory: %s\n' "$work" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$repository_root" worktree add --quiet --detach "$work/tree" HEAD
script="$work/tree/scripts/phase3/receipts-differential.sh"

cat >"$work/gate" <<'EOF'
#!/bin/sh
# Stub build gate: fakes the lane's cargo commands from STUB_TEST,
# STUB_CLIPPY, and STUB_FMT. Exits 97 if the script under test leaked
# SPOCKY_ALLOW_SKIP.
[ -z "${SPOCKY_ALLOW_SKIP+set}" ] || exit 97
case "$*" in
  "cargo test "*)
    case "$STUB_TEST" in
      match)
        printf '[1]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-node-normalized.json"
        printf '[1]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-rust-normalized.json"
        ;;
      mismatch)
        printf '[1]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-node-normalized.json"
        printf '[2]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-rust-normalized.json"
        ;;
      missing) ;;
      fail)
        # Matching outputs, so only the failing exit status can fail the run.
        printf '[1]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-node-normalized.json"
        printf '[1]' >"$SPOCKY_RECEIPTS_EVIDENCE/receipts-rust-normalized.json"
        exit 1
        ;;
      *) exit 2 ;;
    esac
    ;;
  "cargo clippy "*) [ "${STUB_CLIPPY:-pass}" = pass ] ;;
  "cargo fmt "*) [ "${STUB_FMT:-pass}" = pass ] ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/gate"

# run_case <label> <expected exit: 0 or nonzero> <env assignment>...
run_case() {
  label=$1
  expected=$2
  shift 2
  if env "$@" SPOCKY_RECEIPTS_EVIDENCE_ROOT="$work/$label" "$script" >"$work/$label.log" 2>&1; then
    actual=0
  else
    actual=nonzero
  fi
  if [ "$actual" != "$expected" ]; then
    cat "$work/$label.log" >&2
    p3_fail "case $label: expected exit $expected, got $actual"
  fi
  printf 'ok %s (exit %s)\n' "$label" "$actual"
}

stub="SPOCKY_BUILD_GATE=$work/gate"
run_case match 0 "$stub" STUB_TEST=match SPOCKY_ALLOW_SKIP=1
grep -F 'passed' "$work/match.log" >/dev/null
run_case mismatch nonzero "$stub" STUB_TEST=mismatch
grep -F 'normalized differential outputs differ' "$work/mismatch.log" >/dev/null
run_case missing nonzero "$stub" STUB_TEST=missing
grep -F 'missing differential output' "$work/missing.log" >/dev/null
run_case test-fails nonzero "$stub" STUB_TEST=fail
run_case clippy-fails nonzero "$stub" STUB_TEST=match STUB_CLIPPY=fail
run_case fmt-fails nonzero "$stub" STUB_TEST=match STUB_FMT=fail
printf 'probe\n' >"$work/tree/receipts-dirty-probe"
run_case dirty-tree nonzero "$stub" STUB_TEST=match
grep -F 'worktree is not clean' "$work/dirty-tree.log" >/dev/null
grep -F '?? receipts-dirty-probe' "$work"/dirty-tree/receipts-*/git-status.txt >/dev/null
rm "$work/tree/receipts-dirty-probe"

# A real differential run against a dist that is not the pinned build.
tampered=$work/dist
mkdir -p "$tampered/server/message-receipts"
cp "$pinned_dist/server/atomic-file.js" "$tampered/server/atomic-file.js"
sed 's/agent_request_key_conflict/agent_request_key_conflict_tampered/' \
  "$pinned_dist/server/message-receipts/index.js" >"$tampered/server/message-receipts/index.js"
! cmp -s "$pinned_dist/server/message-receipts/index.js" "$tampered/server/message-receipts/index.js" ||
  p3_fail "tampered index.js is identical to the pinned build"
run_case tampered-dist nonzero SPOCKY_PASEO_DIST="$tampered"
grep -F 'is not the pinned build' "$work"/tampered-dist/receipts-*/test.log >/dev/null

printf 'receipts-differential.test.sh: 8 cases passed\n'
