#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
if [ -n "${PASEO_HUB_BASELINE_ROOT:-}" ]; then
  baseline_root=$PASEO_HUB_BASELINE_ROOT
elif [ -d "$repository_root/.baselines/hub" ]; then
  baseline_root="$repository_root/.baselines/hub"
else
  baseline_root="$repository_root/../../paseo-rust/.baselines/hub"
fi
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
evidence_root="$repository_root/evidence/phase2"

if [ "${1:-}" = "--print-baseline-root" ]; then
  printf '%s\n' "$baseline_root"
  exit 0
fi
fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-retained-evidence.XXXXXX")
cleanup() {
  gtimeout 30 rm -rf "$fixture_root"
}
trap cleanup EXIT HUP INT TERM

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
  exit 1
fi
if [ -n "$(gtimeout 30 git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

gtimeout 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root"
(cd "$fixture_root" && gtimeout 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
(cd "$fixture_root" && \
  gtimeout 60 npm ls @electric-sql/pglite --all --json) \
  >"$fixture_root/dependency-graph.json"
(cd "$fixture_root/node_modules/@electric-sql/pglite" && \
  gtimeout 120 find . -type f -exec shasum -a 256 {} +) \
  >"$fixture_root/package-sha256.unsorted.txt"
gtimeout 30 sort "$fixture_root/package-sha256.unsorted.txt" \
  >"$fixture_root/package-sha256.txt"
(cd "$fixture_root/drizzle" && \
  gtimeout 120 find . -type f -exec shasum -a 256 {} +) \
  >"$fixture_root/migrations-sha256.unsorted.txt"
gtimeout 30 sort "$fixture_root/migrations-sha256.unsorted.txt" \
  >"$fixture_root/migrations-sha256.txt"
gtimeout 300 cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-retained-pglite-evidence >/dev/null

mkdir "$fixture_root/original-db" "$fixture_root/candidate-db"
PASEO_HUB_SOURCE_ROOT="$fixture_root" \
PASEO_HUB_TSX="$fixture_root/node_modules/.bin/tsx" \
  gtimeout 300 "$fixture_root/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-embedded-pglite.mjs" \
    capture "$fixture_root/original-db" >"$fixture_root/original.json"
SPOCKY_NODE=$(gtimeout 30 command -v node) \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$fixture_root/node_modules/@electric-sql/pglite" \
SPOCKY_HUB_MIGRATIONS="$fixture_root/drizzle" \
  gtimeout 300 "$repository_root/target/debug/hub-retained-pglite-evidence" \
    capture "$fixture_root/candidate-db" >"$fixture_root/candidate.json"

gtimeout 30 jq -n \
  --arg baseline "$expected_baseline" \
  --slurpfile original "$fixture_root/original.json" \
  --slurpfile candidate "$fixture_root/candidate.json" \
  --slurpfile dependencyGraph "$fixture_root/dependency-graph.json" '
  {
    baseline: $baseline,
    exactEngineRuntime: true,
    selectionStatus: "retained-host-candidate",
    catalogParity: (
      $original[0].observations.schemaTables == $candidate[0].observations.schemaTables
      and $original[0].observations.schemaConstraints == $candidate[0].observations.schemaConstraints
    ),
    journalParity: (
      $original[0].observations.migrationJournal == $candidate[0].observations.migrationJournal
    ),
    scenarioParity: {
      restart: ($original[0].operations.restart and $candidate[0].operations.restart),
      crossProcessRejection: (
        $original[0].operations.crossProcessRejection
        and $candidate[0].operations.crossProcessRejection
      ),
      transactionRollback: (
        $original[0].operations.transactionRollback
        and $candidate[0].operations.transactionRollback
      ),
      staleOwnerRecovery: (
        $original[0].operations.staleOwnerRecovery
        and $candidate[0].operations.staleOwnerRecovery
      ),
      historicalResume: (
        $candidate[0].operations.historicalResume.prefixJournalRows == 1
        and $candidate[0].operations.historicalResume.suffix.applied == 48
        and $candidate[0].operations.historicalResume.suffix.journalRows == 49
      )
    },
    candidateRecoveryEvidence: {
      committedWriteCrashRecovery: $candidate[0].operations.committedWriteCrashRecovery,
      injectedPostCloseFailureRecovery: $candidate[0].operations.injectedPostCloseFailureRecovery,
      partialMigrationRollback: {
        verified: (
          $candidate[0].operations.partialMigrationRollback.journalRows == 0
          and $candidate[0].operations.partialMigrationRollback.publicTableRows == 0
          and $candidate[0].operations.partialMigrationRollback.partialProbeRows == 0
          and $candidate[0].operations.partialMigrationRollback.rolledBackProbeRows == 0
        ),
        observed: $candidate[0].operations.partialMigrationRollback
      },
      historicalUserRow: $candidate[0].operations.historicalResume.userRowPreserved
    },
    runtime: $candidate[0].identity,
    dependencyGraph: $dependencyGraph[0],
    failures: $candidate[0].failures,
    retainedFiles: [
      "scripts/phase2/hub-embedded-retained-host.mjs",
      "crates/spocky-hub-pilot/src/retained_pglite.rs"
    ],
    retainedPackageInventory: "hub-embedded-retained-package-sha256.txt",
    retainedMigrationInventory: "hub-embedded-retained-migrations-sha256.txt",
    unqualifiedResiduals: [
      "Node executable availability and platform packaging are not qualified",
      "Framed IPC throughput and latency are not qualified",
      "Retained JavaScript delivery, updates, and support ownership are not qualified",
      "Callback transactions and keyed application locks are not ported or qualified",
      "Catalog parity does not prove full schema-definition provenance parity"
    ],
    compatibilityException: {
      status: "required-not-accepted",
      capability: "Retain pinned PGlite JavaScript host behind bounded Rust IPC",
      owner: "Hub storage",
      scope: "Vendor PGlite glue plus named retained_pglite adapter only",
      removalCondition: "Native Rust host reproduces distributed PGlite runtime contract",
      reviewDate: "2026-10-01"
    }
  }' >"$fixture_root/comparison.json"

gtimeout 30 jq -e '
  .catalogParity == true
  and .journalParity == true
  and (.scenarioParity | all(.[]; . == true))
  and .candidateRecoveryEvidence.committedWriteCrashRecovery == true
  and .candidateRecoveryEvidence.injectedPostCloseFailureRecovery == true
  and .candidateRecoveryEvidence.partialMigrationRollback.verified == true
  and .candidateRecoveryEvidence.historicalUserRow == true
  and .compatibilityException.status == "required-not-accepted"
' "$fixture_root/comparison.json" >/dev/null

gtimeout 30 cp "$fixture_root/original.json" "$evidence_root/hub-embedded-retained-original.json"
gtimeout 30 cp "$fixture_root/candidate.json" "$evidence_root/hub-embedded-retained-candidate.json"
gtimeout 30 cp "$fixture_root/comparison.json" "$evidence_root/hub-embedded-retained-comparison.json"
gtimeout 30 cp "$fixture_root/dependency-graph.json" \
  "$evidence_root/hub-embedded-retained-dependency-graph.json"
gtimeout 30 cp "$fixture_root/package-sha256.txt" \
  "$evidence_root/hub-embedded-retained-package-sha256.txt"
gtimeout 30 cp "$fixture_root/migrations-sha256.txt" \
  "$evidence_root/hub-embedded-retained-migrations-sha256.txt"
(cd "$evidence_root" && gtimeout 30 shasum -a 256 \
  hub-embedded-retained-original.json \
  hub-embedded-retained-candidate.json \
  hub-embedded-retained-comparison.json \
  hub-embedded-retained-dependency-graph.json \
  hub-embedded-retained-package-sha256.txt \
  hub-embedded-retained-migrations-sha256.txt) \
  >"$evidence_root/hub-embedded-retained-sha256.txt"
gtimeout 30 cat "$evidence_root/hub-embedded-retained-comparison.json"
gtimeout 30 cat "$evidence_root/hub-embedded-retained-sha256.txt"
