#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-mixed-ownership.sh"
output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-mixed-ownership-test.XXXXXX")
cleanup() {
  gtimeout 30 rm -f "$output"
}
trap cleanup EXIT HUP INT TERM

plan=$("$runner" --print-plan)
printf '%s\n' "$plan" | grep -F 'baseline: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'storage: disposable same-schema directory; source trees read-only'
printf '%s\n' "$plan" | grep -F 'forward: live pinned baseline excludes retained candidate'
printf '%s\n' "$plan" | grep -F 'handoff: candidate opens unchanged directory after bounded baseline exit'
printf '%s\n' "$plan" | grep -F 'reverse: live retained candidate excludes newly started pinned baseline'

gtimeout --kill-after=2 10 node \
  "$repository_root/scripts/phase2/hub-mixed-ownership-orchestrator.mjs" \
  --self-test-cleanup >/dev/null

signal_output=$(mktemp "${TMPDIR:-/tmp}/spocky-hub-signal-cleanup.XXXXXX")
signal_runner=
owner_pid=
descendant_pid=
cleanup_signal_test() {
  for pid in "$signal_runner" "$owner_pid" "$descendant_pid"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
  rm -f "$signal_output"
}
trap cleanup_signal_test EXIT HUP INT TERM
node "$repository_root/scripts/phase2/hub-mixed-ownership-orchestrator.mjs" \
  --self-test-signal-cleanup >"$signal_output" &
signal_runner=$!
attempt=0
while [ ! -s "$signal_output" ] && kill -0 "$signal_runner" 2>/dev/null; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    printf 'signal cleanup self-test readiness timed out\n' >&2
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
    printf 'signal cleanup self-test shutdown timed out\n' >&2
    exit 1
  fi
  sleep 0.05
done
wait "$signal_runner" || [ "$?" -eq 143 ]
! kill -0 "$owner_pid" 2>/dev/null
! kill -0 "$descendant_pid" 2>/dev/null
cleanup_signal_test
trap - EXIT HUP INT TERM

gtimeout --kill-after=30 1200 "$runner" >"$output"

jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.sourceTreeMutated == false
  and .storage.disposableCopy == true
  and .storage.baselineJournalRows == 49
  and .storage.candidateJournalRows == 49
  and .storage.candidateMigrationsApplied == 0
  and .storage.journalRowsEqual == true
  and .platform.os == "darwin"
  and .platform.processGroupCleanup == "dedicated-process-group"
  and .forwardExclusion.baselineReady == true
  and .forwardExclusion.candidateExcluded == true
  and .forwardExclusion.candidateError == "directory-in-use"
  and .handoff.baselineExit == "bounded-clean"
  and .handoff.directoryRecreated == false
  and .handoff.candidateOpenedUnchangedDirectory == true
  and .handoff.baselineMarkerPayload == "pinned-baseline-live-owner"
  and .reverseExclusion.candidateReady == true
  and .reverseExclusion.baselineExcluded == true
  and .reverseExclusion.baselineErrorContains == "already in use"
  and .shutdown.candidateExitCode == 0
  and .shutdown.candidateExitSignal == null
  and .shutdown.candidateProcessGroupGone == true
  and .scope == "ordered-live-starts-only"
  and .limitations == [
    "simultaneous_pre_owner_record_race_unqualified",
    "schema_downgrade_unqualified"
  ]
  and .compatibilityMechanism.status == "not-required-for-ordered-starts"
  and .compatibilityMechanism.reason == "shared live PID owner record excludes ordered mixed starts"
  and (.rawEvidence | sort == [
    "hub-mixed-ownership-events.json",
    "hub-mixed-ownership-processes.json"
  ])
' "$output" >/dev/null

gtimeout 30 cmp "$output" "$repository_root/evidence/phase2/hub-mixed-ownership-report.json"
grep -F 'Scope: ordered live starts only.' \
  "$repository_root/evidence/phase2/hub-mixed-ownership.md" >/dev/null
grep -F 'Simultaneous pre-owner-record race is unqualified.' \
  "$repository_root/evidence/phase2/hub-mixed-ownership.md" >/dev/null
grep -F 'Schema downgrade is unqualified.' \
  "$repository_root/evidence/phase2/hub-mixed-ownership.md" >/dev/null
grep -F 'Port 6767 was untouched.' \
  "$repository_root/evidence/phase2/hub-mixed-ownership.md" >/dev/null
