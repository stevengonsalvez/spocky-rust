#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-legacy-handoff.sh"
output=$(gtimeout 30 mktemp "${TMPDIR:-/tmp}/spocky-hub-legacy-handoff-test.XXXXXX")
cleanup() {
  gtimeout 30 rm -f "$output"
}
trap cleanup EXIT HUP INT TERM

plan=$("$runner" --print-plan)
printf '%s\n' "$plan" | grep -F 'baseline: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'storage: disposable copy only; baseline and .baselines stay read-only'
printf '%s\n' "$plan" | grep -F 'forward: pinned baseline writes, retained candidate opens same directory'
printf '%s\n' "$plan" | grep -F 'reverse: pinned baseline reopens candidate-mutated directory, bounded to 300s'
printf '%s\n' "$plan" | grep -F 'acceptance: compatibility exception remains required-not-accepted'
grep -F '.baseline.sourceTreeMutated == false' "$runner" >/dev/null
grep -F '.schemaMigration.beforeJournalRows == 49' "$runner" >/dev/null
grep -F '.schemaMigration.afterJournalRows == 49' "$runner" >/dev/null

gtimeout --kill-after=30 1200 "$runner" >"$output"

jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.sourceTreeMutated == false
  and .storage.disposableCopy == true
  and .forwardHandoff.status == "supported"
  and .forwardHandoff.candidateOpenedBaselineDirectory == true
  and .schemaMigration.status == "no-op-current-schema"
  and .schemaMigration.beforeJournalRows == 49
  and .schemaMigration.applied == 0
  and .schemaMigration.afterJournalRows == 49
  and .dataPreservation.status == "preserved"
  and .dataPreservation.baselineMarkerPayload == "baseline-data-preserved"
  and .dataPreservation.candidateMarkerPayload == "candidate-data-preserved"
  and (.reverseRollback.status == "supported" or .reverseRollback.status == "unsupported")
  and (
    if .reverseRollback.status == "supported"
    then .reverseRollback.baselineOpenedCandidateDirectory == true
      and .reverseRollback.baselineSawCandidateMarker == true
      and .reverseRollback.baselineSawBaselineMarker == true
      and .reverseRollback.journalPreserved == true
    else .reverseRollback.observation.exitStatus != 0
    end
  )
  and .compatibilityException.status == "required-not-accepted"
  and (.rawEvidence | sort == [
    "hub-legacy-handoff-baseline-produce.json",
    "hub-legacy-handoff-baseline-reverse.json",
    "hub-legacy-handoff-candidate-forward.json"
  ])
' "$output" >/dev/null

gtimeout 30 cmp "$output" "$repository_root/evidence/phase2/hub-legacy-handoff-report.json"
