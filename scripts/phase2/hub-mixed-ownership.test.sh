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

gtimeout --kill-after=30 1200 "$runner" >"$output"

jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.sourceTreeMutated == false
  and .storage.disposableCopy == true
  and .storage.sameSchemaJournalRows == 49
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
  and .compatibilityMechanism.status == "not-required"
  and .compatibilityMechanism.reason == "shared live PID owner record excludes ordered mixed starts"
  and (.rawEvidence | sort == [
    "hub-mixed-ownership-events.json",
    "hub-mixed-ownership-processes.json"
  ])
' "$output" >/dev/null

gtimeout 30 cmp "$output" "$repository_root/evidence/phase2/hub-mixed-ownership-report.json"
