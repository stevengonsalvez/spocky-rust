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
storage: disposable same-schema directory; source trees read-only
forward: live pinned baseline excludes retained candidate
handoff: candidate opens unchanged directory after bounded baseline exit
reverse: live retained candidate excludes newly started pinned baseline
EOF
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi

fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-mixed-ownership.XXXXXX")
cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/spocky-hub-mixed-ownership.*) gtimeout 30 rm -rf "$fixture_root" ;;
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

gtimeout --kill-after=30 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 30 mkdir "$fixture_root/source" "$fixture_root/database"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root/source"
(cd "$fixture_root/source" && gtimeout --kill-after=30 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)
gtimeout --kill-after=30 300 cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-mixed-ownership-evidence >/dev/null

PASEO_HUB_SOURCE_ROOT="$fixture_root/source" \
SPOCKY_PGLITE_ADAPTER="$repository_root/scripts/phase2/hub-embedded-retained-host.mjs" \
SPOCKY_PGLITE_PACKAGE="$fixture_root/source/node_modules/@electric-sql/pglite" \
SPOCKY_HUB_MIGRATIONS="$fixture_root/source/drizzle" \
  gtimeout --kill-after=30 300 node "$repository_root/scripts/phase2/hub-mixed-ownership-orchestrator.mjs" \
    "$fixture_root/source/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-mixed-ownership-baseline.mjs" \
    "$repository_root/target/debug/hub-mixed-ownership-evidence" \
    "$fixture_root/database" \
    "$fixture_root/events.json" \
    "$fixture_root/processes.json" >"$fixture_root/qualification.json"

baseline_after=$(gtimeout 30 git -C "$baseline_root" status --porcelain)
source_tree_mutated=true
if [ -z "$baseline_after" ] && [ "$actual_baseline" = "$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)" ]; then
  source_tree_mutated=false
fi

gtimeout 30 jq -n \
  --arg baseline "$expected_baseline" \
  --argjson sourceTreeMutated "$source_tree_mutated" \
  --slurpfile qualification "$fixture_root/qualification.json" '
  {
    baseline: { commit: $baseline, sourceTreeMutated: $sourceTreeMutated },
    scope: $qualification[0].scope,
    limitations: $qualification[0].limitations,
    platform: $qualification[0].platform,
    storage: {
      disposableCopy: true,
      baselineJournalRows: $qualification[0].storageObservation.baselineJournalRows,
      candidateJournalRows: $qualification[0].storageObservation.candidateJournalRows,
      candidateMigrationsApplied: $qualification[0].storageObservation.candidateMigrationsApplied,
      journalRowsEqual: (
        $qualification[0].storageObservation.baselineJournalRows
        == $qualification[0].storageObservation.candidateJournalRows
      )
    },
    forwardExclusion: $qualification[0].forwardExclusion,
    handoff: $qualification[0].handoff,
    reverseExclusion: $qualification[0].reverseExclusion,
    compatibilityMechanism: $qualification[0].compatibilityMechanism,
    shutdown: $qualification[0].shutdown,
    rawEvidence: [
      "hub-mixed-ownership-events.json",
      "hub-mixed-ownership-processes.json"
    ]
  }' >"$fixture_root/report.json"

gtimeout 30 jq -e '
  .baseline.sourceTreeMutated == false
  and .scope == "ordered-live-starts-only"
  and .limitations == [
    "simultaneous_pre_owner_record_race_unqualified",
    "schema_downgrade_unqualified"
  ]
  and .platform.os == "darwin"
  and .platform.processGroupCleanup == "dedicated-process-group"
  and .storage.baselineJournalRows == 49
  and .storage.candidateJournalRows == 49
  and .storage.candidateMigrationsApplied == 0
  and .storage.journalRowsEqual == true
  and .forwardExclusion.baselineReady == true
  and .forwardExclusion.candidateExcluded == true
  and .handoff.baselineExit == "bounded-clean"
  and .handoff.directoryRecreated == false
  and .handoff.candidateOpenedUnchangedDirectory == true
  and .handoff.baselineMarkerPayload == "pinned-baseline-live-owner"
  and .reverseExclusion.candidateReady == true
  and .reverseExclusion.baselineExcluded == true
  and .shutdown.candidateExitCode == 0
  and .shutdown.candidateExitSignal == null
  and .shutdown.candidateProcessGroupGone == true
  and .compatibilityMechanism.status == "not-required-for-ordered-starts"
' "$fixture_root/report.json" >/dev/null

gtimeout 30 cp "$fixture_root/events.json" "$evidence_root/hub-mixed-ownership-events.json"
gtimeout 30 cp "$fixture_root/processes.json" "$evidence_root/hub-mixed-ownership-processes.json"
gtimeout 30 cp "$fixture_root/report.json" "$evidence_root/hub-mixed-ownership-report.json"
events_hash=$(gtimeout 30 shasum -a 256 "$evidence_root/hub-mixed-ownership-events.json" | awk '{print $1}')
processes_hash=$(gtimeout 30 shasum -a 256 "$evidence_root/hub-mixed-ownership-processes.json" | awk '{print $1}')
report_hash=$(gtimeout 30 shasum -a 256 "$evidence_root/hub-mixed-ownership-report.json" | awk '{print $1}')
gtimeout 30 jq -jnr \
  --arg eventsHash "$events_hash" \
  --arg processesHash "$processes_hash" \
  --arg reportHash "$report_hash" '
  "# Hub mixed ownership qualification\n\n" +
  "Run:\n\n```sh\n" +
  "gtimeout --kill-after=30 1200 scripts/phase2/hub-mixed-ownership.test.sh\n" +
  "```\n\n" +
  "Observed sequence: pinned baseline owns the disposable 49-row same-schema directory; " +
  "candidate is excluded; baseline exits cleanly; candidate opens the unchanged directory; " +
  "new pinned baseline is excluded.\n\n" +
  "Observed storage: baseline 49 journal rows; candidate 49 journal rows; " +
  "candidate applied zero migrations.\n\n" +
  "Shutdown: both owners exited cleanly and their dedicated process groups were gone.\n\n" +
  "Platform: macOS x64.\n\n" +
  "Scope: ordered live starts only.\n\n" +
  "Limitations:\n\n" +
  "- Simultaneous pre-owner-record race is unqualified.\n" +
  "- Schema downgrade is unqualified.\n\n" +
  "Port 6767 was untouched.\n\n" +
  "| Artifact | SHA-256 |\n" +
  "| --- | --- |\n" +
  "| `hub-mixed-ownership-events.json` | `\($eventsHash)` |\n" +
  "| `hub-mixed-ownership-processes.json` | `\($processesHash)` |\n" +
  "| `hub-mixed-ownership-report.json` | `\($reportHash)` |\n"
  ' >"$evidence_root/hub-mixed-ownership.md"
(cd "$evidence_root" && gtimeout 30 shasum -a 256 \
  hub-mixed-ownership-events.json \
  hub-mixed-ownership-processes.json \
  hub-mixed-ownership-report.json \
  hub-mixed-ownership.md >hub-mixed-ownership-sha256.txt)
gtimeout 30 cat "$fixture_root/report.json"
