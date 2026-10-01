#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
pglite_version=0.5.4
pglite_integrity='sha512-yYZUyyXrHU7tPlCjwZQJ6hIG9DscdCCn7Uk0mYKwC1FeHX286AbcmFveMiRBEak8e9iPupjsoVImN3yJZVed2g=='

baseline_root=${PASEO_HUB_BASELINE_ROOT:-}
if [ -z "$baseline_root" ]; then
  if [ -d "$repository_root/.baselines/hub" ]; then
    baseline_root="$repository_root/.baselines/hub"
  else
    baseline_root="$repository_root/../../paseo-rust/.baselines/hub"
  fi
fi

actual=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
if [ "$actual" != "$expected_baseline" ]; then
  printf 'Hub baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
  exit 1
fi
if [ -n "$(gtimeout 30 git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi
locked_version=$(gtimeout 30 jq -r '.packages["node_modules/@electric-sql/pglite"].version' \
  "$baseline_root/package-lock.json")
locked_integrity=$(gtimeout 30 jq -r '.packages["node_modules/@electric-sql/pglite"].integrity' \
  "$baseline_root/package-lock.json")
if [ "$locked_version" != "$pglite_version" ] || [ "$locked_integrity" != "$pglite_integrity" ]; then
  printf 'PGlite package lock mismatch in pinned Hub baseline\n' >&2
  exit 1
fi

probe_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-engine-selection.XXXXXX")
cleanup() {
  gtimeout 30 rm -rf "$probe_root"
}
trap cleanup EXIT HUP INT TERM

gtimeout 300 git -C "$baseline_root" archive -o "$probe_root/baseline.tar" "$expected_baseline"
gtimeout 60 tar -xf "$probe_root/baseline.tar" -C "$probe_root"
(cd "$probe_root" && gtimeout 600 npm ci --ignore-scripts --no-audit --no-fund >/dev/null 2>&1)

package_root="$probe_root/node_modules/@electric-sql/pglite"
probe=$(gtimeout 60 node "$repository_root/scripts/phase2/hub-embedded-engine-probe.mjs" \
  "$package_root")
PASEO_HUB_SOURCE_ROOT="$probe_root" \
PASEO_HUB_TSX="$probe_root/node_modules/.bin/tsx" \
  gtimeout 300 "$probe_root/node_modules/.bin/tsx" \
    "$repository_root/scripts/phase2/hub-embedded-pglite.mjs" \
    capture "$probe_root/runtime" >"$probe_root/runtime.json"
migration_count=$(gtimeout 30 jq '.observations.migrationJournal | length' \
  "$probe_root/runtime.json")
runtime_operations=$(gtimeout 30 jq -c '.operations' "$probe_root/runtime.json")

gtimeout 30 jq -n \
  --arg commit "$expected_baseline" \
  --arg version "$pglite_version" \
  --arg integrity "$pglite_integrity" \
  --argjson probe "$probe" \
  --argjson migrationCount "$migration_count" \
  --argjson runtimeOperations "$runtime_operations" \
  '{
    baseline: {
      commit: $commit,
      package: $probe.package,
      version: $version,
      integrity: $integrity
    },
    probe: ($probe + {
      historicalMigrationCount: $migrationCount,
      runtimeOperations: $runtimeOperations
    }),
    decision: {
      selection: "blocked",
      sqliteExactEngine: false,
      requiresRetainedJavaScriptOrRootDependencies: true,
      reason: "Pinned PGlite exposes JavaScript entry points and Wasm modules with custom env imports. Current Rust manifests contain SQLite only. Exact hosting therefore retains JavaScript glue or adds root Cargo dependencies and a compatible host implementation."
    },
    compatibilityException: {
      status: "required-not-accepted",
      capability: "Embedded Hub PostgreSQL-compatible storage",
      owner: "Hub storage",
      exactRuntimeVersion: ("@electric-sql/pglite " + $version + " with its distributed JavaScript glue; Node.js v26.7.0 is the only qualified host"),
      boundary: "Rust Hub process to retained JavaScript PGlite host over a new bounded IPC boundary",
      exchangedData: "PostgreSQL SQL, bind parameters, result rows, transaction commands, migration commands, errors, and shutdown state",
      pilotEvidence: ("Pinned baseline replays " + ($migrationCount | tostring) + " historical migrations and passes restart, rollback, locking, and stale-owner probes; no Rust-hosted PGlite candidate exists"),
      securityPackagingRisk: "Adds Node.js and npm artifacts, a 25 MB PGlite package, subprocess IPC, filesystem permissions, code-signing scope, updater scope, and supply-chain review",
      performanceSupportRisk: "Adds process startup, IPC serialization, crash supervision, memory duplication, and support for 121 custom main-module env imports plus 40 initdb env imports",
      originalVsCandidateTests: "Original PGlite runtime is executable; SQLite remains engine, schema, dialect, migration, constraint, mutation, and crash-process non-parity; retained-host candidate has not been implemented",
      platforms: "macOS arm64 baseline probe only; iOS, Android, browser, macOS packaging, Windows, and Linux retained-runtime qualification is absent",
      removalCondition: "Remove retained JavaScript after a Rust-hosted PGlite-compatible engine replays all historical migrations, reopens old state, survives process crashes, and matches embedded and PostgreSQL mutation traces on required platforms",
      reviewDate: "2026-10-01"
    }
  }'
