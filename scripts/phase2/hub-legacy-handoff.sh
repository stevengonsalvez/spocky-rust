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

if [ "${1:-}" = "--print-plan" ]; then
  cat <<EOF
baseline: $expected_baseline
storage: disposable copy only; baseline and .baselines stay read-only
forward: pinned baseline writes, retained candidate opens same directory
reverse: pinned baseline reopens candidate-mutated directory, bounded to 300s
acceptance: compatibility exception remains required-not-accepted
EOF
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi

fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-legacy-handoff.XXXXXX")
cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/spocky-hub-legacy-handoff.*) gtimeout 30 rm -rf "$fixture_root" ;;
    *) printf 'refusing to remove unexpected fixture: %s\n' "$fixture_root" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

actual_baseline=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual_baseline" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual_baseline" >&2
  exit 1
fi
baseline_before=$(gtimeout 30 git -C "$baseline_root" status --porcelain)
if [ -n "$baseline_before" ]; then
  printf 'Hub baseline tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

gtimeout 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 60 mkdir "$fixture_root/source"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root/source"
(cd "$fixture_root/source" && gtimeout 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
gtimeout 30 mkdir "$fixture_root/database"

PASEO_HUB_SOURCE_ROOT="$fixture_root/source" \
  gtimeout 300 "$fixture_root/source/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-legacy-handoff-baseline.mjs" \
    produce "$fixture_root/database" >"$fixture_root/baseline-produce.json"

gtimeout 300 cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-legacy-handoff-evidence >/dev/null
SPOCKY_NODE=$(gtimeout 30 command -v node) \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$fixture_root/source/node_modules/@electric-sql/pglite" \
SPOCKY_HUB_MIGRATIONS="$fixture_root/source/drizzle" \
  gtimeout 300 "$repository_root/target/debug/hub-legacy-handoff-evidence" \
    "$fixture_root/database" >"$fixture_root/candidate-forward.json"

set +e
PASEO_HUB_SOURCE_ROOT="$fixture_root/source" \
  gtimeout 300 "$fixture_root/source/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-legacy-handoff-baseline.mjs" \
    reverse-observe "$fixture_root/database" >"$fixture_root/baseline-reverse.stdout" \
    2>"$fixture_root/baseline-reverse.stderr"
reverse_exit=$?
set -e
if [ "$reverse_exit" -eq 0 ]; then
  gtimeout 30 cp "$fixture_root/baseline-reverse.stdout" "$fixture_root/baseline-reverse.json"
  reverse_status=supported
else
  gtimeout 30 jq -n \
    --arg status unsupported \
    --argjson exitStatus "$reverse_exit" \
    --rawfile stdout "$fixture_root/baseline-reverse.stdout" \
    --rawfile stderr "$fixture_root/baseline-reverse.stderr" \
    '{operation: "baseline-reverse-observe", status: $status, exitStatus: $exitStatus, stdout: $stdout, stderr: $stderr}' \
    >"$fixture_root/baseline-reverse.json"
  reverse_status=unsupported
fi

baseline_after=$(gtimeout 30 git -C "$baseline_root" status --porcelain)
source_tree_mutated=true
if [ -z "$baseline_after" ] && [ "$actual_baseline" = "$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)" ]; then
  source_tree_mutated=false
fi

gtimeout 30 jq -n \
  --arg baseline "$expected_baseline" \
  --argjson sourceTreeMutated "$source_tree_mutated" \
  --arg reverseStatus "$reverse_status" \
  --slurpfile produced "$fixture_root/baseline-produce.json" \
  --slurpfile forward "$fixture_root/candidate-forward.json" \
  --slurpfile reverse "$fixture_root/baseline-reverse.json" '
  {
    baseline: {
      commit: $baseline,
      sourceTreeMutated: $sourceTreeMutated
    },
    storage: {
      disposableCopy: true,
      baselineDirectoryReusedAcrossProcesses: true
    },
    forwardHandoff: {
      status: (if $forward[0].opened then "supported" else "unsupported" end),
      candidateOpenedBaselineDirectory: $forward[0].opened
    },
    schemaMigration: {
      status: (
        if $forward[0].migration.applied == 0
          and $forward[0].beforeJournalRows == $forward[0].afterJournalRows
        then "no-op-current-schema"
        else "migration-applied"
        end
      ),
      beforeJournalRows: $forward[0].beforeJournalRows,
      applied: $forward[0].migration.applied,
      afterJournalRows: $forward[0].afterJournalRows
    },
    dataPreservation: {
      status: (
        if $forward[0].baselineMarker and $forward[0].candidateMarker
        then "preserved"
        else "not-preserved"
        end
      ),
      baselineMarker: $forward[0].baselineMarker,
      candidateMarker: $forward[0].candidateMarker
    },
    reverseRollback: {
      status: $reverseStatus,
      baselineOpenedCandidateDirectory: ($reverseStatus == "supported" and $reverse[0].opened == true),
      baselineSawCandidateMarker: (
        $reverseStatus == "supported"
        and any($reverse[0].probeRows[]?; .producer == "retained-candidate")
      ),
      observation: $reverse[0]
    },
    rawEvidence: [
      "hub-legacy-handoff-baseline-produce.json",
      "hub-legacy-handoff-candidate-forward.json",
      "hub-legacy-handoff-baseline-reverse.json"
    ],
    compatibilityException: {
      status: "required-not-accepted",
      capability: "Retain pinned PGlite JavaScript host behind bounded Rust IPC"
    }
  }' >"$fixture_root/report.json"

gtimeout 30 cp "$fixture_root/baseline-produce.json" \
  "$evidence_root/hub-legacy-handoff-baseline-produce.json"
gtimeout 30 cp "$fixture_root/candidate-forward.json" \
  "$evidence_root/hub-legacy-handoff-candidate-forward.json"
gtimeout 30 cp "$fixture_root/baseline-reverse.json" \
  "$evidence_root/hub-legacy-handoff-baseline-reverse.json"
gtimeout 30 cp "$fixture_root/report.json" \
  "$evidence_root/hub-legacy-handoff-report.json"
gtimeout 30 cat "$fixture_root/report.json"
