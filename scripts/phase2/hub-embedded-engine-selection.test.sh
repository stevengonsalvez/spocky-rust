#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-embedded-engine-selection.sh"
output=$(mktemp "${TMPDIR:-/tmp}/spocky-hub-engine-selection.XXXXXX")
trap 'rm -f "$output"' EXIT HUP INT TERM

gtimeout 900 "$runner" >"$output"

jq -e '
  .baseline.commit == "28f6c78833065fd282f9064f92a9aa61875dd359"
  and .baseline.package == "@electric-sql/pglite"
  and .baseline.version == "0.5.4"
  and .probe.mainWasm.imports.byModule.env > 0
  and .probe.mainWasm.imports.byModule.wasi_snapshot_preview1 > 0
  and .probe.initdbWasm.imports.byModule.env > 0
  and .probe.packageBytes > 17000000
  and .probe.packageFileCount > 100
  and .decision.selection == "blocked"
  and .decision.sqliteExactEngine == false
  and .decision.requiresRetainedJavaScriptOrRootDependencies == true
  and .compatibilityException.status == "required-not-accepted"
  and ([
    "capability", "owner", "exactRuntimeVersion", "boundary", "exchangedData",
    "pilotEvidence", "securityPackagingRisk", "performanceSupportRisk",
    "originalVsCandidateTests", "platforms", "removalCondition", "reviewDate"
  ] - (.compatibilityException | keys) | length == 0)
  and (.compatibilityException | to_entries | all(.[]; .value != null and .value != "" and .value != []))
' "$output" >/dev/null

gtimeout 30 cmp "$output" \
  "$repository_root/evidence/phase2/hub-embedded-engine-selection.json"
