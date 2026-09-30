#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
baseline="$repo_root/.baselines/relay"
expected_relay_commit=3fc41c96c8c63f3a7109e832899cc57d473c4531
raw_dir="$repo_root/evidence/raw/phase2"
raw_log="$raw_dir/relay-runtime.log"
baseline_log="$raw_dir/relay-baseline-runtime.log"
elixir_image="elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79"

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

docker run --rm \
  --name "paseo-relay-baseline-$$" \
  -e MIX_ENV=test \
  -e PASEO_OWNERSHIP_SURGE_COUNT=30 \
  -v "$baseline:/baseline:ro" \
  "$elixir_image" \
  sh -lc 'cp -a /baseline /work && cd /work && mix local.hex --force && mix local.rebar --force && mix deps.get && mix test test/paseo_relay_test.exs --seed 1' \
  2>&1 | tee "$baseline_log"

{
  cargo test -p spocky-relay-pilot --test runtime_process -- --test-threads=1 --nocapture
  cargo test -p spocky-relay-pilot --test network_runtime -- --test-threads=1 --nocapture
  cargo test -p spocky-relay-pilot --test network_process_runtime -- --test-threads=1 --nocapture
} 2>&1 | tee "$raw_log"

shasum -a 256 "$baseline_log" "$raw_log"
