#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"

cargo test -p spocky-relay-pilot --test selected_runtime -- --nocapture
cargo test -p spocky-relay-pilot --test network_runtime -- --nocapture
cargo test -p spocky-relay-pilot --test network_process_runtime -- --nocapture
cargo test -p spocky-relay-pilot --test runtime_process -- --nocapture
cargo test -p spocky-relay-pilot --test distributed_failure -- --nocapture
cargo clippy -p spocky-relay-pilot --all-targets -- -D warnings
cargo fmt -p spocky-relay-pilot --check
