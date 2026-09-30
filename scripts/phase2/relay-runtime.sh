#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
baseline="$repo_root/.baselines/relay"
expected_relay_commit=3fc41c96c8c63f3a7109e832899cc57d473c4531
raw_dir="$repo_root/evidence/raw/phase2"
raw_log="$raw_dir/relay-runtime.log"

actual_relay_commit=$(git -C "$baseline" rev-parse HEAD)
if [[ "$actual_relay_commit" != "$expected_relay_commit" ]]; then
  echo "relay baseline mismatch: expected $expected_relay_commit, got $actual_relay_commit" >&2
  exit 1
fi
if [[ -n "$(git -C "$baseline" status --porcelain)" ]]; then
  echo "relay baseline is dirty" >&2
  exit 1
fi

mkdir -p "$raw_dir"
cd "$repo_root"
cargo test -p paseo-relay-pilot --test runtime_process -- --test-threads=1 --nocapture \
  2>&1 | tee "$raw_log"
