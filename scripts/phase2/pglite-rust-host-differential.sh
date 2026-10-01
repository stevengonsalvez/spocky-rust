#!/bin/sh
# Original-versus-Rust differential for the PGlite Rust host. Runs the same
# capture as hub-embedded-retained-evidence.sh, with the Rust child binary
# (crates/spocky-pglite-host, spocky-pglite-host-child) started by the
# unchanged retained adapter in place of Node, and runs the 17 retained-host
# runtime tests of spocky-hub-pilot against the same binary. Writes
# evidence/phase2/pglite-rust-host-differential-*. Heavy steps go through
# the shared build gate.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline_root=$(sh "$repository_root/scripts/phase2/hub-embedded-retained.test.sh" --print-baseline-root)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
evidence_root="$repository_root/evidence/phase2"
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/pglite-rust-host}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
  exit 1
fi
if [ -n "$(gtimeout 30 git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-pglite-differential.XXXXXX")
cleanup() {
  gtimeout 600 rm -rf "$fixture_root"
}
trap cleanup EXIT HUP INT TERM

gtimeout 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root"
(cd "$fixture_root" && gtimeout 600 "$gate" npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
package="$fixture_root/node_modules/@electric-sql/pglite"
migrations="$fixture_root/drizzle"

gtimeout 2400 "$gate" cargo build --locked --release --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-pglite-host --bin spocky-pglite-host-child
gtimeout 2400 "$gate" cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-retained-pglite-evidence
child="$CARGO_TARGET_DIR/release/spocky-pglite-host-child"
# One compilation cache for every child the adapter starts.
mkdir "$fixture_root/cache"
export SPOCKY_PGLITE_CACHE_DIR="$fixture_root/cache"

mkdir "$fixture_root/original-db" "$fixture_root/candidate-db"
PASEO_HUB_SOURCE_ROOT="$fixture_root" \
PASEO_HUB_TSX="$fixture_root/node_modules/.bin/tsx" \
  gtimeout 300 "$fixture_root/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-embedded-pglite.mjs" \
    capture "$fixture_root/original-db" >"$fixture_root/original.json"
SPOCKY_NODE="$child" \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$package" \
SPOCKY_HUB_MIGRATIONS="$migrations" \
  gtimeout 1200 "$CARGO_TARGET_DIR/debug/hub-retained-pglite-evidence" \
    capture "$fixture_root/candidate-db" >"$fixture_root/candidate.json"

gtimeout 30 jq -n \
  --arg baseline "$expected_baseline" \
  --slurpfile original "$fixture_root/original.json" \
  --slurpfile candidate "$fixture_root/candidate.json" '
  {
    baseline: $baseline,
    candidate: "spocky-pglite-host-child behind the unchanged retained adapter",
    catalogParity: (
      $original[0].observations.schemaTables == $candidate[0].observations.schemaTables
      and $original[0].observations.schemaConstraints == $candidate[0].observations.schemaConstraints
    ),
    counts: {
      schemaTables: [($original[0].observations.schemaTables | length), ($candidate[0].observations.schemaTables | length)],
      schemaConstraints: [($original[0].observations.schemaConstraints | length), ($candidate[0].observations.schemaConstraints | length)],
      migrationJournal: [($original[0].observations.migrationJournal | length), ($candidate[0].observations.migrationJournal | length)]
    },
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
      partialMigrationRollback: $candidate[0].operations.partialMigrationRollback,
      historicalUserRow: $candidate[0].operations.historicalResume.userRowPreserved
    },
    runtime: $candidate[0].identity,
    failures: $candidate[0].failures
  }' >"$fixture_root/comparison.json"

# The 17 retained-host runtime tests, unchanged, with the Rust child.
set +e
SPOCKY_NODE="$child" \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$package" \
SPOCKY_HUB_MIGRATIONS="$migrations" \
  gtimeout 3000 "$gate" cargo test --locked --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-hub-pilot --test retained_pglite_runtime -- --test-threads=1 \
    >"$fixture_root/tests.log" 2>&1
tests_status=$?
set -e

for name in original candidate comparison; do
  sed "s#$fixture_root#<fixture>#g" "$fixture_root/$name.json" \
    >"$evidence_root/pglite-rust-host-differential-$name.json"
done
sed "s#$fixture_root#<fixture>#g" "$fixture_root/tests.log" \
  >"$evidence_root/pglite-rust-host-differential-tests.log"
(cd "$evidence_root" && gtimeout 30 shasum -a 256 \
  pglite-rust-host-differential-original.json \
  pglite-rust-host-differential-candidate.json \
  pglite-rust-host-differential-comparison.json \
  pglite-rust-host-differential-tests.log)
gtimeout 30 jq -c '{catalogParity, journalParity, counts, scenarioParity, candidateRecoveryEvidence}' \
  "$evidence_root/pglite-rust-host-differential-comparison.json"
grep -E '^test |^test result' "$evidence_root/pglite-rust-host-differential-tests.log" || true
printf 'retained runtime tests exit status: %s\n' "$tests_status"
