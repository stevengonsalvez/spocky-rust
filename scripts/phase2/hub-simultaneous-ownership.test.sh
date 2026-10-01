#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-test.XXXXXX")
signal_output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-signal.XXXXXX")
signal_stdout=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-simultaneous-signal-stdout.XXXXXX")
signal_runner=
owner_pid=
descendant_pid=
read_fixture_ids() {
  if [ ! -s "$signal_output" ]; then
    return
  fi
  if [ -z "$owner_pid" ]; then
    candidate_owner=$(jq -er '.ownerPid' "$signal_output" 2>/dev/null || true)
    case "$candidate_owner" in
      ''|*[!0-9]*) ;;
      *) if [ "$candidate_owner" -gt 1 ]; then owner_pid=$candidate_owner; fi ;;
    esac
  fi
  if [ -z "$descendant_pid" ]; then
    candidate_descendant=$(jq -er '.descendantPid' "$signal_output" 2>/dev/null || true)
    case "$candidate_descendant" in
      ''|*[!0-9]*) ;;
      *) if [ "$candidate_descendant" -gt 1 ]; then descendant_pid=$candidate_descendant; fi ;;
    esac
  fi
}
require_fixture_ids() {
  read_fixture_ids
  for fixture_pid in "$owner_pid" "$descendant_pid"; do
    case "$fixture_pid" in
      ''|*[!0-9]*|0|1)
        printf 'signal cleanup fixture PID is missing or invalid: %s\n' "$fixture_pid" >&2
        return 1
        ;;
    esac
  done
  if [ "$owner_pid" = "$descendant_pid" ]; then
    printf 'signal cleanup fixture PIDs are not distinct: %s\n' "$owner_pid" >&2
    return 1
  fi
}
terminate_signal_fixture() {
  requested_signal=${1:-TERM}
  expected_status=${2:-143}
  require_graceful=${3:-false}
  fallback_required=false
  cleanup_status=0
  if [ -n "$signal_runner" ] && kill -0 "$signal_runner" 2>/dev/null; then
    kill -"$requested_signal" "$signal_runner" 2>/dev/null || cleanup_status=1
    attempt=0
    while kill -0 "$signal_runner" 2>/dev/null; do
      read_fixture_ids
      attempt=$((attempt + 1))
      if [ "$attempt" -ge 200 ]; then
        break
      fi
      sleep 0.05
    done
    if kill -0 "$signal_runner" 2>/dev/null; then
      fallback_required=true
      kill -KILL "$signal_runner" 2>/dev/null || cleanup_status=1
    fi
  fi
  if [ -n "$signal_runner" ]; then
    if wait "$signal_runner"; then
      runner_status=0
    else
      runner_status=$?
    fi
    if [ "$require_graceful" = true ] && [ "$runner_status" -ne "$expected_status" ]; then
      printf 'signal cleanup runner status %s, expected %s\n' \
        "$runner_status" "$expected_status" >&2
      cleanup_status=1
    elif [ "$require_graceful" != true ]; then
      case "$runner_status" in
        0|129|130|137|143) ;;
        *)
          printf 'signal cleanup runner exited with status %s\n' "$runner_status" >&2
          cleanup_status=1
          ;;
      esac
    fi
  fi
  read_fixture_ids
  if [ -n "$owner_pid" ] && {
    kill -0 "$owner_pid" 2>/dev/null || kill -0 -- "-$owner_pid" 2>/dev/null;
  }; then
    fallback_required=true
    kill -KILL -- "-$owner_pid" 2>/dev/null || cleanup_status=1
  elif [ -n "$descendant_pid" ] && kill -0 "$descendant_pid" 2>/dev/null; then
    fallback_required=true
    kill -KILL "$descendant_pid" 2>/dev/null || cleanup_status=1
  fi
  if [ "$require_graceful" = true ] && [ "$fallback_required" = true ]; then
    printf 'signal cleanup required fallback after %s\n' "$requested_signal" >&2
    cleanup_status=1
  fi
  for pid in "$owner_pid" "$descendant_pid"; do
    attempt=0
    while [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; do
      attempt=$((attempt + 1))
      if [ "$attempt" -ge 100 ]; then
        printf 'signal cleanup survivor: %s\n' "$pid" >&2
        cleanup_status=1
        break
      fi
      sleep 0.05
    done
  done
  if [ "$cleanup_status" -ne 0 ]; then
    return "$cleanup_status"
  fi
  signal_runner=
}
cleanup() {
  cleanup_status=0
  terminate_signal_fixture || cleanup_status=1
  gtimeout 30 rm -f "$output" "$signal_output" "$signal_stdout" || cleanup_status=1
  return "$cleanup_status"
}
on_signal() {
  signal_status=$1
  trap - EXIT HUP INT TERM
  if ! cleanup; then signal_status=1; fi
  exit "$signal_status"
}
trap cleanup EXIT
trap 'on_signal 129' HUP
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

# Exercise HUP before the fixture announces readiness. The sidecar retains exact owned PIDs.
node "$repository_root/scripts/phase2/hub-simultaneous-ownership-orchestrator.mjs" \
  --self-test-signal-cleanup "$signal_output" delay-ready >"$signal_stdout" &
signal_runner=$!
attempt=0
while [ ! -s "$signal_output" ] && kill -0 "$signal_runner" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    printf 'early signal cleanup fixture timed out\n' >&2
    exit 1
  fi
  sleep 0.05
done
require_fixture_ids
terminate_signal_fixture HUP 129 true
if kill -0 "$owner_pid" 2>/dev/null; then
  printf 'early signal cleanup owner survived: %s\n' "$owner_pid" >&2
  exit 1
fi
if kill -0 "$descendant_pid" 2>/dev/null; then
  printf 'early signal cleanup descendant survived: %s\n' "$descendant_pid" >&2
  exit 1
fi
owner_pid=
descendant_pid=
: >"$signal_output"

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
require_fixture_ids
terminate_signal_fixture TERM 143 true
if kill -0 "$owner_pid" 2>/dev/null; then
  printf 'signal cleanup owner survived: %s\n' "$owner_pid" >&2
  exit 1
fi
if kill -0 "$descendant_pid" 2>/dev/null; then
  printf 'signal cleanup descendant survived: %s\n' "$descendant_pid" >&2
  exit 1
fi
owner_pid=
descendant_pid=

# ownership.sh has 900 seconds of summed inner bounds. Keep finite cleanup and escalation headroom.
gtimeout --kill-after=30 1020 "$repository_root/scripts/phase2/hub-simultaneous-ownership.sh" >"$output"
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
