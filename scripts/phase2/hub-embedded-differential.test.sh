#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/hub-embedded-differential.sh"

plan=$($runner --print-plan)
printf '%s\n' "$plan" | grep -F 'Hub baseline: 28f6c78833065fd282f9064f92a9aa61875dd359'
printf '%s\n' "$plan" | grep -F 'PGlite package: 0.5.4'
printf '%s\n' "$plan" | grep -F 'operations: restart, cross-process rejection, transaction rollback, same-key serialization'
printf '%s\n' "$plan" | grep -F 'expected mismatch: engine, schema, dialect, migrations'

$runner --preflight-only

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/hub-embedded-compare.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
cat >"$fixture_root/pglite.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"]},"boundary":{"engine":"PGlite","schema":"baseline relational","dialect":"PostgreSQL","migrations":"baseline journal"}}
JSON
cat >"$fixture_root/rust.json" <<'JSON'
{"operations":{"restart":true,"crossProcessRejection":true,"transactionRollback":true,"sameKeySerialization":["first:start","first:end","second:start","second:end"]},"boundary":{"engine":"SQLite","schema":"whole-state snapshot","dialect":"SQLite","migrations":"pilot schema only"}}
JSON

$runner --compare-json "$fixture_root/pglite.json" "$fixture_root/rust.json" \
  >"$fixture_root/comparison.json"
jq -e '.observableParity == true' "$fixture_root/comparison.json" >/dev/null
jq -e '.exactDatabaseParity == false' "$fixture_root/comparison.json" >/dev/null
jq -e '.mismatches == ["engine", "schema", "dialect", "migrations"]' \
  "$fixture_root/comparison.json" >/dev/null
