#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
common_git_dir=$(git -C "$repository_root" rev-parse --path-format=absolute --git-common-dir)
main_root=$(dirname "$common_git_dir")
reference_root=${PASEO_REFERENCE_ROOT:-"$main_root/.baselines/paseo-runtime"}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
raw_log="$repository_root/evidence/phase2/plugin-daemon-rpc-raw.log"
target_dir="$repository_root/crates/spocky-plugin-pilot/.target-daemon-rpc"

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
  PASEO_REFERENCE_ROOT="$reference_root" SPOCKY_REPOSITORY_ROOT="$repository_root" \
    "$reference_root/node_modules/.bin/vitest" run \
    scripts/phase2/plugin-daemon-rpc-baseline.test.ts \
    --maxWorkers=1 --reporter=default \
    --config scripts/phase2/plugin-daemon-rpc-vitest.config.mts \
    --root "$repository_root"
  CARGO_TARGET_DIR="$target_dir" cargo test \
    --manifest-path "$repository_root/Cargo.toml" \
    -p spocky-plugin-pilot --test plugin_daemon_rpc \
    all_plugin_daemon_messages_roundtrip_with_exact_shapes -- --nocapture
} | tee "$raw_log"

baseline=$(sed -n 's/^.*PLUGIN_DAEMON_RPC_BASELINE //p' "$raw_log")
rust=$(sed -n 's/^.*PLUGIN_DAEMON_RPC_RUST //p' "$raw_log")
if [[ -z "$baseline" || -z "$rust" || "$baseline" != "$rust" ]]; then
  printf 'plugin daemon RPC differential mismatch\n' >&2
  diff -u <(printf '%s\n' "$baseline") <(printf '%s\n' "$rust") || true
  exit 1
fi

if ! git -C "$reference_root" diff --quiet || ! git -C "$reference_root" diff --cached --quiet; then
  printf 'baseline tracked files changed during capture\n' >&2
  exit 1
fi

printf 'plugin daemon RPC differential: 42 matched, 0 mismatched\n'
shasum -a 256 "$raw_log"
