#!/bin/sh
# Runs the G4 send-retry gate (original daemon against spocky-daemon) and then
# re-checks its evidence on its own, so a gate bug cannot pass the lane alone.
#
# Usage: scripts/phase3/receipts-retry-parity.sh
#
# Runs `scripts/phase3/gate.sh g4-retry` (self-check, then parity). Exit 0 only
# when all of these hold:
#   - the worktree is clean outside logs/ and the gate exits 0;
#   - both verdicts (self-check and parity) pass with no differences,
#     discovery or comparison error, check failure, survivor, or harness error;
#   - the parity sides are the original and spocky daemons, each probe step
#     exited 0 and the stub recorded exactly 3 turns (a retry that started a
#     second turn makes it 4);
#   - both sides' probe outcomes equal the expected outcomes below, in order;
#   - both sides hold exactly two send receipts, both `completed`, with the
#     same fingerprints.
# The probe is scripts/phase3/receipts-retry-probe.mjs. Outcomes, receipts and
# verdicts are compared as raw text; nothing is re-sorted.
# scripts/phase3/receipts-retry-parity.test.sh proves each failure exits
# nonzero.
#
# Evidence lands in evidence/raw/phase3/receipts-retry-<utc>/ (untracked); its
# SHA-256 digests are printed.
#
# Env: SPOCKY_RETRY_GATE (default scripts/phase3/gate.sh),
#      SPOCKY_RETRY_EVIDENCE_ROOT (default evidence/raw/phase3),
#      SPOCKY_RETRY_TIMEOUT (default 3300 seconds), plus the gate's own
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_message_receipts)
#      and CARGO_BUILD_JOBS (default 2).
set -eu
unset SPOCKY_ALLOW_SKIP

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

gate_script=${SPOCKY_RETRY_GATE:-$repository_root/scripts/phase3/gate.sh}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_message_receipts}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# The probe's outcomes, in order: a retry and a second connection's retry are
# accepted, the same messageId with other text is a key conflict, and the
# concurrent pair is accepted.
expected_outcomes='[{"step":"created","ok":true,"error":null},{"step":"first","ok":true,"error":null},{"step":"retry","ok":true,"error":null},{"step":"retry-other","ok":true,"error":null},{"step":"conflict","ok":false,"error":"agent_request_key_conflict"},{"step":"race","ok":true,"error":null}]'
expected_turns=3
expected_receipts=2

run_id=receipts-retry-$(date -u +%Y%m%dT%H%M%SZ)
evidence=${SPOCKY_RETRY_EVIDENCE_ROOT:-$repository_root/evidence/raw/phase3}/$run_id
mkdir -p "$(dirname "$evidence")"
# A fresh directory, so outputs of an earlier run can never pass this one.
mkdir "$evidence" || p3_fail "evidence directory already exists: $evidence"

cd "$repository_root"
status=0
fail() {
  printf 'FAIL %s\n' "$*" >&2
  status=1
}

git status --porcelain --untracked-files=all >"$evidence/git-status.txt"
if [ -n "$(git status --porcelain --untracked-files=all -- . ':(exclude)logs')" ]; then
  fail "worktree is not clean; see $evidence/git-status.txt"
fi
{
  printf 'commit %s\n' "$(git rev-parse HEAD)"
  printf 'gate %s\n' "$gate_script"
  printf 'probe %s %s\n' "$(p3_sha256 "$repository_root/scripts/phase3/receipts-retry-probe.mjs")" "scripts/phase3/receipts-retry-probe.mjs"
} >"$evidence/inputs.txt"

gate_status=0
gtimeout --kill-after=30 "${SPOCKY_RETRY_TIMEOUT:-3300}" "$gate_script" g4-retry >"$evidence/gate.log" 2>&1 || gate_status=$?
[ "$gate_status" -eq 0 ] || fail "gate.sh g4-retry exited $gate_status; see $evidence/gate.log"

gate_evidence=$(sed -n 's/^gate g4-retry evidence: //p' "$evidence/gate.log" | tail -n 1)
if [ -z "$gate_evidence" ] || [ ! -d "$gate_evidence" ]; then
  fail "no gate evidence directory in $evidence/gate.log"
else
  printf 'gate-evidence %s\n' "$gate_evidence" >>"$evidence/inputs.txt"

  # Both verdicts must pass cleanly.
  for half in self-check parity; do
    verdict=$gate_evidence/$half/verdict.json
    if [ ! -f "$verdict" ]; then
      fail "missing $half verdict: $verdict"
      continue
    fi
    cp "$verdict" "$evidence/$half-verdict.json"
    jq -e '
      .pass == true and
      (.differences | length) == 0 and
      .discoveryError == null and
      .comparisonError == null and
      (.checkFailures | length) == 0 and
      (.survivors | length) == 0 and
      (.harnessErrors | length) == 0
    ' "$verdict" >/dev/null || fail "$half verdict is not a clean pass: $verdict"
  done

  # Per side facts, read from the raw side records.
  parity=$gate_evidence/parity
  for side_dir in left-original right-spocky; do
    side=$parity/$side_dir/side.json
    if [ ! -f "$side" ]; then
      fail "missing parity side record: $side"
      continue
    fi
    kind=${side_dir#*-}
    jq -e --arg kind "$kind" '.kind == $kind' "$side" >/dev/null ||
      fail "$side_dir is not the $kind daemon"
    jq -e '[.steps[] | select(.name == "probe")] | length == 1 and .[0].exit == {"kind":"code","value":0}' "$side" >/dev/null ||
      fail "$side_dir probe step did not exit 0"
    turns=$(jq '.stub_records | length' "$side")
    [ "$turns" = "$expected_turns" ] ||
      fail "$side_dir stub recorded $turns turns, expected $expected_turns (a retry started a turn, or a send never did)"

    # First stdout line of the probe, decoded from its byte array.
    python3 -c '
import json, sys
steps = json.load(open(sys.argv[1]))["steps"]
sys.stdout.buffer.write(bytes([step for s in steps if s["name"] == "probe" for step in s["stdout"]]))
' "$side" >"$evidence/$side_dir-probe-stdout.txt" ||
      fail "$side_dir probe stdout could not be decoded"
    head -n 1 "$evidence/$side_dir-probe-stdout.txt" | jq -c '.outcomes' >"$evidence/$side_dir-outcomes.json" ||
      fail "$side_dir probe printed no outcomes line"
    [ "$(cat "$evidence/$side_dir-outcomes.json")" = "$expected_outcomes" ] ||
      fail "$side_dir outcomes differ from the expected outcomes; see $evidence/$side_dir-outcomes.json"

    # Send receipts: two, both completed; fingerprints kept in file-name order
    # of nothing (names are generated), so they are listed sorted as text.
    receipts=$parity/$side_dir/files/paseo-home/agent-requests
    : >"$evidence/$side_dir-receipts.txt"
    count=0
    if [ -d "$receipts" ]; then
      for receipt in "$receipts"/*.json; do
        [ -f "$receipt" ] || continue
        count=$((count + 1))
        jq -c '[.fingerprint, .state]' "$receipt" >>"$evidence/$side_dir-receipts.txt" ||
          fail "$side_dir receipt $receipt is not valid JSON"
      done
    fi
    sort -o "$evidence/$side_dir-receipts.txt" "$evidence/$side_dir-receipts.txt"
    [ "$count" -eq "$expected_receipts" ] ||
      fail "$side_dir holds $count send receipts, expected $expected_receipts"
    if grep -v '"completed"\]$' "$evidence/$side_dir-receipts.txt" >/dev/null; then
      fail "$side_dir has a send receipt that is not completed"
    fi
  done
  if [ -f "$evidence/left-original-receipts.txt" ] && [ -f "$evidence/right-spocky-receipts.txt" ]; then
    cmp -s "$evidence/left-original-receipts.txt" "$evidence/right-spocky-receipts.txt" ||
      fail "send receipt fingerprints differ between the original and spocky daemons"
    cmp -s "$evidence/left-original-outcomes.json" "$evidence/right-spocky-outcomes.json" ||
      fail "probe outcomes differ between the original and spocky daemons"
  fi
fi

for file in "$evidence"/*; do
  [ -f "$file" ] || continue
  printf '%s  %s\n' "$(p3_sha256 "$file")" "${file#"$repository_root"/}"
done
[ "$status" -eq 0 ] || p3_fail "$run_id failed; see $evidence"
printf '%s passed\n' "$run_id"
