#!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
report=$(gtimeout --kill-after=30 1200 "$root/scripts/phase2/hub-schema-downgrade.sh")
printf '%s\n' "$report" | jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.clean == true
  and .candidateNewer.legacyApplied == 0
  and .candidateNewer.journal == 50
  and .candidateNewer.dataPreserved == true
  and .legacyNewer.candidateApplied == 0
  and .legacyNewer.journal == 50
  and .legacyNewer.dataPreserved == true
  and .partialFailure.errorObserved == true
  and .partialFailure.journalRolledBack == true
  and .partialFailure.schemaRolledBack == true
  and .scope == "bounded-additive-only-no-full-parity-claim"
' >/dev/null
