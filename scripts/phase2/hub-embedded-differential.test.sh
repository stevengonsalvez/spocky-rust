#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-embedded-differential.sh"

plan=$($runner --print-plan)
printf '%s\n' "$plan" | grep -F 'Hub baseline: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'PGlite package: 0.5.4'
printf '%s\n' "$plan" | grep -F 'operations: restart, cross-process rejection, transaction rollback, same-key serialization, stale-owner recovery'
printf '%s\n' "$plan" | grep -F 'observations: tables, constraints, canonical schema inventory, migration journal, lock owner record'
printf '%s\n' "$plan" | grep -F 'expected mismatch: engine, schema, dialect, migrations'

$runner --preflight-only

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/hub-embedded-compare.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
cat >"$fixture_root/pglite.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"],"staleOwnerRecovery":true},"observations":{"tables":["drizzle.__drizzle_migrations","public.user"],"constraints":["user_email_unique"],"canonicalTables":["public.user"],"canonicalConstraints":["user_email_unique"],"migrationJournal":[{"hash":"baseline-hash","createdAt":1}],"migrationReopenStable":true,"lockOwnerKeys":["pid","token"]},"boundary":{"engine":"PGlite","schema":"baseline relational","dialect":"PostgreSQL","migrations":"baseline journal"}}
JSON
cat >"$fixture_root/rust.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"],"staleOwnerRecovery":true},"observations":{"tables":["hub_state","user"],"constraints":["user_email_unique"],"canonicalTables":["public.user"],"canonicalConstraints":["user_email_unique"],"migrationJournal":[{"version":0,"name":"0000_phase_0_spine"}],"migrationReopenStable":true,"lockOwnerKeys":["pid","token"]},"boundary":{"engine":"SQLite","schema":"baseline-owned relational schema plus snapshot compatibility shim","dialect":"SQLite","migrations":"baseline journal representation over idempotent final schema"}}
JSON

$runner --compare-json "$fixture_root/pglite.json" "$fixture_root/rust.json" \
  >"$fixture_root/comparison.json"
jq -e '.observableParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.exactDatabaseParity == false' "$fixture_root/comparison.json" >/dev/null
jq -e '.schemaInventoryParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.tableInventoryParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.constraintInventoryParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.schemaInventoryMismatches == {tablesMissingInPglite: [], tablesMissingInRust: [], constraintsMissingInPglite: [], constraintsMissingInRust: []}' "$fixture_root/comparison.json" >/dev/null
jq -e '.mismatches == ["engine", "schema", "dialect", "migrations"]' \
  "$fixture_root/comparison.json" >/dev/null
jq -e '.observations.pglite.lockOwnerKeys == .observations.rust.lockOwnerKeys' \
  "$fixture_root/comparison.json" >/dev/null
jq -e '.operations.pglite.staleOwnerRecovery == true' \
  "$fixture_root/comparison.json" >/dev/null
