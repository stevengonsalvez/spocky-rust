#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-test.XXXXXX")
signal_output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-signal.XXXXXX")
signal_runner=
owner_pid=
descendant_pid=
cleanup() {
  for pid in "$signal_runner" "$owner_pid" "$descendant_pid"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
  gtimeout 30 rm -f "$output" "$signal_output"
}
trap cleanup EXIT HUP INT TERM

node "$repository_root/scripts/phase2/hub-simultaneous-ownership-orchestrator.mjs" \
  --self-test-signal-cleanup >"$signal_output" &
signal_runner=$!
attempt=0
while [ ! -s "$signal_output" ] && kill -0 "$signal_runner" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    printf 'signal cleanup readiness timed out\n' >&2
    exit 1
  fi
  sleep 0.05
done
owner_pid=$(jq -er '.ownerPid' "$signal_output")
descendant_pid=$(jq -er '.descendantPid' "$signal_output")
kill -TERM "$signal_runner"
attempt=0
while kill -0 "$signal_runner" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 200 ]; then
    printf 'signal cleanup shutdown timed out\n' >&2
    exit 1
  fi
  sleep 0.05
done
wait "$signal_runner" || [ "$?" -eq 143 ]
! kill -0 "$owner_pid" 2>/dev/null
! kill -0 "$descendant_pid" 2>/dev/null
signal_runner=
owner_pid=
descendant_pid=

gtimeout --kill-after=30 900 "$repository_root/scripts/phase2/hub-simultaneous-ownership.sh" >"$output"
jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.sourceSha256 == "cf3b965451bd8cd9203f16118bdda8df80cec096b51d8d7f3e5b0456dc5f92e7"
  and .baseline.sourceHashExact == true
  and .baseline.sourceSpellingsExact == true
  and .baseline.disposableSourceReadOnly == true
  and .baseline.execution == "handwritten-lock-operation-model"
  and .baseline.pinnedDatabaseRuntimeExecuted == false
  and .candidatePausedBeforeOwner.scope == "actual-candidate-process-paused-before-guard"
  and .candidatePausedBeforeOwner.pause.event == "paused-before-owner-open"
  and .candidatePausedBeforeOwner.baselineOpened == true
  and .candidatePausedBeforeOwner.candidateRejected.error == "directory-in-use"
  and .candidatePausedBeforeOwner.rejectedExit == {"code": 0, "signal": null}
  and (.candidatePausedBeforeOwner | has("candidateExit") | not)
  and .candidatePausedBeforeOwner.completedLiveOwnerPreserved == true
  and .actualCandidateAfterGuardUnitTest.test
    == "directory_lock::tests::completed_legacy_owner_wins_while_candidate_is_paused_after_guard"
  and .baselinePausedAfterExclusiveCreate.scope == "handwritten-pinned-operation-model"
  and .baselinePausedAfterExclusiveCreate.candidateReady.opened == true
  and .baselinePausedAfterExclusiveCreate.distinctInodes == true
  and .baselinePausedAfterExclusiveCreate.visibleOwnerProtocol == "os-file-lock-v1"
  and .baselinePausedAfterExclusiveCreate.bothProcessesLive == true
  and .baselinePausedAfterExclusiveCreate.candidateExit == {"code": 0, "signal": null}
  and .completedLiveRecordStaleUnlinkToctou.allInodesDistinct == true
  and .completedLiveRecordStaleUnlinkToctou.baselineOwnerWasLive == true
  and .completedLiveRecordStaleUnlinkToctou.completedLiveRecordDeletedByModeledCandidate == true
  and .completedLiveRecordStaleUnlinkToctou.bothProcessesLive == true
  and .conclusion.parity == "not-claimed"
  and .conclusion.residualExceptions == [
    "paused-incomplete-writer",
    "completed-live-record-stale-unlink-toctou"
  ]
' "$output" >/dev/null
gtimeout 30 cat "$output"
