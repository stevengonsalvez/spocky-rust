#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
common_git_dir=$(git -C "$repository_root" rev-parse --path-format=absolute --git-common-dir)
main_root=$(dirname "$common_git_dir")
reference_root=${PASEO_REFERENCE_ROOT:-"$main_root/.baselines/paseo-runtime"}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
raw_log="$repository_root/evidence/phase2/plugin-hook-runtime-raw.log"
target_dir="$repository_root/crates/spocky-plugin-pilot/.target-hook-runtime"

if [[ "$(git -C "$reference_root" rev-parse HEAD)" != "$expected_baseline" ]]; then
  printf 'baseline HEAD mismatch\n' >&2
  exit 1
fi
if ! git -C "$reference_root" diff --quiet || ! git -C "$reference_root" diff --cached --quiet; then
  printf 'baseline tracked files are dirty\n' >&2
  exit 1
fi
if [[ ! -x "$reference_root/node_modules/.bin/vitest" ]]; then
  printf 'missing pinned vitest installation: %s/node_modules/.bin/vitest\n' "$reference_root" >&2
  exit 1
fi

{
  PASEO_REFERENCE_ROOT="$reference_root" \
    "$reference_root/node_modules/.bin/vitest" run \
    scripts/phase2/plugin-hook-baseline.test.ts \
    --maxWorkers=1 --reporter=default \
    --config scripts/phase2/plugin-hook-vitest.config.mts \
    --root "$repository_root"

  PASEO_REFERENCE_ROOT="$reference_root" NODE_OPTIONS=--conditions=source \
    "$reference_root/node_modules/.bin/vitest" run \
    packages/server/src/server/plugins/lifecycle/handlers.test.ts \
    -t "teardown aborts an active callback" \
    --maxWorkers=1 --reporter=default --root "$reference_root"
  CARGO_TARGET_DIR="$target_dir" cargo test \
    --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-plugin-pilot --test hook_usage_runtime -- --nocapture --test-threads=1
} | tee "$raw_log"

baseline=$(sed -n 's/^.*PLUGIN_HOOK_BASELINE //p' "$raw_log")
rust=$(sed -n 's/^.*PLUGIN_HOOK_RUST //p' "$raw_log")
baseline_failures=$(sed -n 's/^.*PLUGIN_HOOK_BASELINE_FAILURES //p' "$raw_log")
rust_failures=$(sed -n 's/^.*PLUGIN_HOOK_RUST_FAILURES //p' "$raw_log")
if [[ -z "$baseline" || -z "$rust" || "$baseline" != "$rust" ]]; then
  printf 'hook, usage, and provider differential mismatch\n' >&2
  diff -u <(printf '%s\n' "$baseline") <(printf '%s\n' "$rust") || true
  exit 1
fi
if [[ -z "$baseline_failures" || "$baseline_failures" != "$rust_failures" ]]; then
  printf 'timeout and process-death differential mismatch\n' >&2
  diff -u <(printf '%s\n' "$baseline_failures") <(printf '%s\n' "$rust_failures") || true
  exit 1
fi

if ! git -C "$reference_root" diff --quiet || ! git -C "$reference_root" diff --cached --quiet; then
  printf 'baseline tracked files changed during capture\n' >&2
  exit 1
fi

printf 'plugin hook differential: 18 matched, 0 mismatched\n'
shasum -a 256 "$raw_log"
