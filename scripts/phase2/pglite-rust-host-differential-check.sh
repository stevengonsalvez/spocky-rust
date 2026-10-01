#!/bin/sh
# Gate of the PGlite Rust host differential. Exits 1, naming each failed
# check, unless catalog and journal parity hold with the expected counts
# (50 tables, 537 constraint and index names, 49 journal rows), every
# scenario and recovery check holds, the error payloads of the Rust
# candidate equal those of the Node retained candidate, and the 17 retained
# runtime tests passed.
#
# usage: pglite-rust-host-differential-check.sh <comparison.json>
#          <candidate.json> <reference candidate.json> <tests.log> <tests exit status>
set -eu

if [ "$#" -ne 5 ]; then
  printf 'usage: %s <comparison.json> <candidate.json> <reference candidate.json> <tests.log> <tests exit status>\n' "$0" >&2
  exit 2
fi
comparison=$1
candidate=$2
reference=$3
tests_log=$4
tests_status=$5
failed=0

check() {
  name=$1
  shift
  if ! "$@" >/dev/null 2>&1; then
    printf 'FAILED: %s\n' "$name"
    failed=1
  fi
}

check 'catalog parity' jq -e '.catalogParity == true' "$comparison"
check 'journal parity' jq -e '.journalParity == true' "$comparison"
check 'table count 50' jq -e '.counts.schemaTables == [50, 50]' "$comparison"
check 'constraint and index name count 537' jq -e '.counts.schemaConstraints == [537, 537]' "$comparison"
check 'journal row count 49' jq -e '.counts.migrationJournal == [49, 49]' "$comparison"
check 'scenario parity' jq -e '(.scenarioParity | length) == 5 and (.scenarioParity | all(.[]; . == true))' "$comparison"
check 'committed write crash recovery' jq -e '.candidateRecoveryEvidence.committedWriteCrashRecovery == true' "$comparison"
check 'injected post-close failure recovery' jq -e '.candidateRecoveryEvidence.injectedPostCloseFailureRecovery == true' "$comparison"
check 'historical user row' jq -e '.candidateRecoveryEvidence.historicalUserRow == true' "$comparison"
check 'partial migration rollback' jq -e '.candidateRecoveryEvidence.partialMigrationRollback
  | .journalRows == 0 and .publicTableRows == 0 and .partialProbeRows == 0 and .rolledBackProbeRows == 0' "$comparison"
check 'failure payloads equal the Node retained candidate' \
  jq -e --slurpfile reference "$reference" '.failures == $reference[0].failures' "$candidate"
check 'migration error equals the Node retained candidate' \
  jq -e --slurpfile reference "$reference" \
  '.operations.partialMigrationRollback.migrationError == $reference[0].operations.partialMigrationRollback.migrationError' \
  "$candidate"
check 'retained runtime tests exit status 0' test "$tests_status" -eq 0
check '17 retained runtime tests passed' grep -F 'test result: ok. 17 passed; 0 failed' "$tests_log"

if [ "$failed" -ne 0 ]; then
  exit 1
fi
printf 'differential checks passed\n'
