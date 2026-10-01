#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

if command -v gtimeout >/dev/null 2>&1; then
  timeout_command=gtimeout
elif command -v timeout >/dev/null 2>&1; then
  timeout_command=timeout
else
  echo "relay selected runtime requires gtimeout or timeout" >&2
  exit 1
fi

run_bounded() {
  duration=$1
  shift
  "$timeout_command" "$duration" "$@"
}

run_bounded 60s cargo test -p spocky-relay-pilot --test selected_runtime -- --nocapture
run_bounded 30s scripts/phase2/relay-residual-differential.test.sh
run_bounded 180s scripts/phase2/relay-residual-differential.sh
run_bounded 30s cargo test -p spocky-relay-pilot --test network_runtime -- --nocapture
run_bounded 30s cargo test -p spocky-relay-pilot --test network_process_runtime -- --nocapture
run_bounded 30s cargo test -p spocky-relay-pilot --test runtime_process -- --nocapture
run_bounded 30s cargo test -p spocky-relay-pilot --test distributed_failure -- --nocapture
run_bounded 60s cargo clippy -p spocky-relay-pilot --all-targets -- -D warnings
cargo fmt -p spocky-relay-pilot --check
