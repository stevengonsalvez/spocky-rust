#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-"$repository_root/../paseo-rewrite"}
execution_root=${PASEO_EXECUTION_ROOT:-$reference_root}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
raw_log="$repository_root/evidence/phase2/plugin-settings-raw.log"

if [[ "$(git -C "$reference_root" rev-parse HEAD)" != "$expected_baseline" ]]; then
  printf 'baseline HEAD mismatch\n' >&2
  exit 1
fi
if ! git -C "$reference_root" diff --quiet || ! git -C "$reference_root" diff --cached --quiet; then
  printf 'baseline tracked files are dirty\n' >&2
  exit 1
fi
if [[ ! -x "$execution_root/node_modules/.bin/vitest" ]]; then
  printf 'missing pinned vitest installation: %s/node_modules/.bin/vitest\n' "$execution_root" >&2
  exit 1
fi
if [[ "$execution_root" == "$reference_root" ]]; then
  printf 'execution root must be a disposable copy, not the read-only baseline\n' >&2
  exit 1
fi

test_target="$execution_root/packages/server/src/server/plugins/settings/schema-differential.test.ts"
cp "$repository_root/scripts/phase2/plugin-settings-baseline.test.ts" "$test_target"

{
  PASEO_REFERENCE_ROOT="$execution_root" \
    "$execution_root/node_modules/.bin/vitest" run \
    packages/server/src/server/plugins/settings/schema-differential.test.ts \
    --maxWorkers=1 --reporter=default \
    --config "$repository_root/scripts/phase2/plugin-settings-vitest.config.mts" \
    --root "$execution_root"
  CARGO_TARGET_DIR="$repository_root/crates/spocky-plugin-pilot/.target-settings" \
    cargo test --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-plugin-pilot --test settings_lifecycle \
    differential_capture_matches_pinned_settings_cases -- --nocapture
} | tee "$raw_log"

baseline=$(sed -n 's/^PLUGIN_SETTINGS_BASELINE //p' "$raw_log")
rust=$(sed -n 's/^PLUGIN_SETTINGS_RUST //p' "$raw_log")
if [[ -z "$baseline" || -z "$rust" || "$baseline" != "$rust" ]]; then
  printf 'settings differential mismatch\n' >&2
  diff -u <(printf '%s\n' "$baseline") <(printf '%s\n' "$rust") || true
  exit 1
fi

printf 'settings differential: 8 matched, 0 mismatched\n'
shasum -a 256 "$raw_log"
