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
#   - the probe's stdout is the outcomes line, then three labelled wire
#     blocks, "# recording client", "# retry-other connection" and "# race
#     second connection", every frame in arrival order. Bare heartbeat pong
#     lines may appear anywhere in a block; the gate strips them before its
#     compare, and this runner ignores them. Each block starts with exactly
#     one server_info frame, and the two sides' frames are byte-identical in
#     key order after masking generated values (as g2-differential.sh does),
#     with no exemption: a difference in features.workspaceLabels (open gap
#     DWLABEL-001) fails the run;
#   - both sides hold exactly two send receipts, both `completed`, with the
#     same fingerprints.
# The probe is scripts/phase3/receipts-retry-probe.mjs. Everything is compared
# as raw bytes after the masks; jq only answers yes or no and never writes
# back a re-encoded value. Receipt files are joined into one line each and
# sorted, since their names are generated.
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
# Generated values the frames hold, masked like g2-differential.sh does.
mask() {
  sed -E -e 's#/private/tmp/spocky-p3-[A-Za-z0-9-]+#<ROOT>#g' \
    -e 's/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/<UUID>/g' \
    -e 's/wks_[0-9a-f]+/<WKS>/g' \
    -e 's/prj_[0-9a-f]+/<PRJ>/g' \
    -e 's/srv_[A-Za-z0-9_-]{12}/<SRV>/g' \
    -e 's/20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g' "$1"
}
# jq is only a predicate here: is this line a server_info status frame?
server_info='if .message then .message else . end'
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

    # The probe's stdout, decoded from its byte array.
    python3 -c '
import json, sys
steps = json.load(open(sys.argv[1]))["steps"]
sys.stdout.buffer.write(bytes([step for s in steps if s["name"] == "probe" for step in s["stdout"]]))
' "$side" >"$evidence/$side_dir-probe-stdout.txt" ||
      fail "$side_dir probe stdout could not be decoded"

    # Line 1 is the outcomes summary; the labelled blocks follow.
    base=$evidence/$side_dir
    : >"$base-client-wire.txt"
    : >"$base-other-wire.txt"
    : >"$base-race-wire.txt"
    awk -v client="$base-client-wire.txt" -v other="$base-other-wire.txt" -v race="$base-race-wire.txt" '
      NR == 1 { next }
      $0 == "# recording client" { out = client; seen_client = NR; next }
      $0 == "# retry-other connection" { out = other; seen_other = NR; next }
      $0 == "# race second connection" { out = race; seen_race = NR; next }
      out != "" { print >> out }
      END { if (!seen_client || !seen_other || !seen_race || seen_client > seen_other || seen_other > seen_race) exit 3 }
    ' "$base-probe-stdout.txt" 2>/dev/null ||
      fail "$side_dir probe stdout has no outcomes line then the recording client, retry-other connection and race second connection blocks, in that order"
    sed -n 1p "$base-probe-stdout.txt" >"$base-summary.txt"
    sed -E 's/^\{"outcomes":(.*),"workspaceId":"[^"]*"\}$/\1/' "$base-summary.txt" >"$base-outcomes.txt"
    [ "$(cat "$base-outcomes.txt")" = "$expected_outcomes" ] ||
      fail "$side_dir outcomes differ from the expected outcomes; see $base-outcomes.txt"

    # Each block starts with its handshake's server_info frame, and only one.
    for block in client other race; do
      wire=$base-$block-wire.txt
      sed -n 1p "$wire" >"$base-$block-server-info.txt"
      jq -e "$server_info | .type == \"status\" and .payload.status == \"server_info\"" "$base-$block-server-info.txt" >/dev/null 2>&1 ||
        fail "$side_dir $block block does not start with the server_info frame"
      infos=$(grep -c '"status":"server_info"' "$wire" || true)
      [ "$infos" = 1 ] || fail "$side_dir $block block holds $infos server_info frames, expected 1"
    done

    # Send receipts: two, both completed. Each file becomes one masked line;
    # the lines are sorted because the file names are generated.
    receipts=$parity/$side_dir/files/paseo-home/agent-requests
    : >"$base-receipts.txt"
    count=0
    if [ -d "$receipts" ]; then
      for receipt in "$receipts"/*.json; do
        [ -f "$receipt" ] || continue
        count=$((count + 1))
        mask "$receipt" | tr '\n' ' ' >>"$base-receipts.txt"
        printf '\n' >>"$base-receipts.txt"
        grep -q '"state": "completed"' "$receipt" ||
          fail "$side_dir has a send receipt that is not completed: $receipt"
      done
    fi
    sort -o "$base-receipts.txt" "$base-receipts.txt"
    [ "$count" -eq "$expected_receipts" ] ||
      fail "$side_dir holds $count send receipts, expected $expected_receipts"
  done
  if [ -f "$evidence/left-original-receipts.txt" ] && [ -f "$evidence/right-spocky-receipts.txt" ]; then
    cmp -s "$evidence/left-original-receipts.txt" "$evidence/right-spocky-receipts.txt" ||
      fail "send receipts differ between the original and spocky daemons"
    for block in client other race; do
      for side_dir in left-original right-spocky; do
        mask "$evidence/$side_dir-$block-server-info.txt" >"$evidence/$side_dir-$block-server-info-compared.txt"
      done
      cmp -s "$evidence/left-original-$block-server-info-compared.txt" "$evidence/right-spocky-$block-server-info-compared.txt" ||
        fail "$block block server_info frames differ between the original and spocky daemons"
    done
  fi
fi

for file in "$evidence"/*; do
  [ -f "$file" ] || continue
  printf '%s  %s\n' "$(p3_sha256 "$file")" "${file#"$repository_root"/}"
done
[ "$status" -eq 0 ] || p3_fail "$run_id failed; see $evidence"
printf '%s passed\n' "$run_id"
