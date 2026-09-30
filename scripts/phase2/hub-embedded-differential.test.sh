#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-embedded-differential.sh"

plan=$($runner --print-plan)
printf '%s\n' "$plan" | grep -F 'Hub baseline: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'PGlite package: 0.5.4'
printf '%s\n' "$plan" | grep -F 'operations: restart, cross-process rejection, transaction rollback, same-key serialization, stale-owner recovery'
printf '%s\n' "$plan" | grep -F 'observations: tables, constraints, migration journal, lock owner record'
printf '%s\n' "$plan" | grep -F 'expected mismatch: engine, schema, dialect, migrations'

$runner --preflight-only

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/hub-embedded-compare.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
cat >"$fixture_root/pglite.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"],"staleOwnerRecovery":true},"observations":{"tables":["drizzle.__drizzle_migrations","public.user"],"constraints":["user_email_unique"],"migrationJournal":[{"hash":"baseline-hash","createdAt":1}],"migrationReopenStable":true,"lockOwnerKeys":["pid","token"]},"boundary":{"engine":"PGlite","schema":"baseline relational","dialect":"PostgreSQL","migrations":"baseline journal"}}
JSON
cat >"$fixture_root/rust.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"],"staleOwnerRecovery":true},"observations":{"tables":["account","hub_state","user"],"constraints":["sqlite_autoindex_user_1"],"migrationJournal":[{"version":1,"name":"0001_snapshot_state"},{"version":2,"name":"0002_modeled_relational_state"}],"migrationReopenStable":true,"lockOwnerKeys":["pid","token"]},"boundary":{"engine":"SQLite","schema":"modeled relational subset plus snapshot","dialect":"SQLite","migrations":"pilot version journal"}}
JSON

$runner --compare-json "$fixture_root/pglite.json" "$fixture_root/rust.json" \
  >"$fixture_root/comparison.json"
jq -e '.observableParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.exactDatabaseParity == false' "$fixture_root/comparison.json" >/dev/null
jq -e '.mismatches == ["engine", "schema", "dialect", "migrations"]' \
  "$fixture_root/comparison.json" >/dev/null
jq -e '.observations.pglite.lockOwnerKeys == .observations.rust.lockOwnerKeys' \
  "$fixture_root/comparison.json" >/dev/null
jq -e '.operations.pglite.staleOwnerRecovery == true' \
  "$fixture_root/comparison.json" >/dev/null
