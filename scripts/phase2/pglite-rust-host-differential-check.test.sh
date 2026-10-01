#!/bin/sh
# Proves the PGlite Rust host differential gate passes only a full match:
# a valid fixture passes, and each single mismatch, count drop or error
# fails with exit 1 and names the failed check.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
check="$repository_root/scripts/phase2/pglite-rust-host-differential-check.sh"
reference="$repository_root/evidence/phase2/hub-embedded-retained-candidate.json"

work=$(mktemp -d "${TMPDIR:-/tmp}/pglite-rust-host-differential-check.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM

jq -n '{
  catalogParity: true,
  journalParity: true,
  counts: {schemaTables: [50, 50], schemaConstraints: [537, 537], migrationJournal: [49, 49]},
  scenarioParity: {restart: true, crossProcessRejection: true, transactionRollback: true,
    staleOwnerRecovery: true, historicalResume: true},
  candidateRecoveryEvidence: {
    committedWriteCrashRecovery: true,
    injectedPostCloseFailureRecovery: true,
    partialMigrationRollback: {journalRows: 0, publicTableRows: 0, partialProbeRows: 0, rolledBackProbeRows: 0},
    historicalUserRow: true
  }
}' >"$work/comparison.json"
cp "$reference" "$work/candidate.json"
printf 'test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n' >"$work/tests.log"

"$check" "$work/comparison.json" "$work/candidate.json" "$reference" "$work/tests.log" 0 \
  | grep -F 'differential checks passed' >/dev/null

# expect_failure <check name> <comparison jq edit> <candidate jq edit> <tests log> <status>
expect_failure() {
  name=$1
  jq "$2" "$work/comparison.json" >"$work/bad-comparison.json"
  jq "$3" "$work/candidate.json" >"$work/bad-candidate.json"
  printf '%s\n' "$4" >"$work/bad-tests.log"
  set +e
  "$check" "$work/bad-comparison.json" "$work/bad-candidate.json" "$reference" \
    "$work/bad-tests.log" "$5" >"$work/output.txt" 2>&1
  status=$?
  set -e
  if [ "$status" -ne 1 ]; then
    printf 'expected exit 1 for %s, got %s\n' "$name" "$status" >&2
    exit 1
  fi
  if ! grep -F "FAILED: $name" "$work/output.txt" >/dev/null; then
    printf 'expected "FAILED: %s" in:\n' "$name" >&2
    cat "$work/output.txt" >&2
    exit 1
  fi
}

passed='test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'
expect_failure 'catalog parity' '.catalogParity = false' '.' "$passed" 0
expect_failure 'journal parity' '.journalParity = false' '.' "$passed" 0
expect_failure 'table count 50' '.counts.schemaTables = [50, 49]' '.' "$passed" 0
expect_failure 'constraint and index name count 537' '.counts.schemaConstraints = [536, 536]' '.' "$passed" 0
expect_failure 'journal row count 49' '.counts.migrationJournal = [48, 48]' '.' "$passed" 0
expect_failure 'scenario parity' '.scenarioParity.restart = false' '.' "$passed" 0
expect_failure 'scenario parity' 'del(.scenarioParity.historicalResume)' '.' "$passed" 0
expect_failure 'committed write crash recovery' \
  '.candidateRecoveryEvidence.committedWriteCrashRecovery = false' '.' "$passed" 0
expect_failure 'partial migration rollback' \
  '.candidateRecoveryEvidence.partialMigrationRollback.journalRows = 1' '.' "$passed" 0
expect_failure 'failure payloads equal the Node retained candidate' \
  '.' '.failures.rollback.code = "23502"' "$passed" 0
expect_failure 'migration error equals the Node retained candidate' \
  '.' '.operations.partialMigrationRollback.migrationError.message = "other"' "$passed" 0
expect_failure 'retained runtime tests exit status 0' '.' '.' "$passed" 101
expect_failure '17 retained runtime tests passed' '.' '.' \
  'test result: FAILED. 16 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out' 0

# A missing input is an error, not a pass.
set +e
"$check" "$work/missing.json" "$work/candidate.json" "$reference" "$work/tests.log" 0 \
  >"$work/output.txt" 2>&1
status=$?
set -e
if [ "$status" -ne 1 ] || ! grep -F 'FAILED: catalog parity' "$work/output.txt" >/dev/null; then
  printf 'missing comparison did not fail the gate\n' >&2
  exit 1
fi

printf 'differential gate rejects every mismatch\n'
