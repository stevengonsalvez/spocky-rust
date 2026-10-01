#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
expected_baseline=3fc41c96c8c63f3a7109e832899cc57d473c4531
elixir_image='elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79'
baseline_root=${PASEO_RELAY_BASELINE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rust/.baselines/relay}

preflight() {
  actual=$(git -C "$baseline_root" rev-parse HEAD)
  if [ "$actual" != "$expected_baseline" ]; then
    printf 'Relay baseline mismatch: expected %s, got %s\n' "$expected_baseline" "$actual" >&2
    exit 1
  fi
  if [ -n "$(git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
    printf 'Relay baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
    exit 1
  fi
}

compare_files() {
  baseline_output=$1
  rust_output=$2
  if ! cmp -s "$baseline_output" "$rust_output"; then
    diff -u "$baseline_output" "$rust_output" >&2 || true
    return 1
  fi
}

case "${1:-}" in
  --print-plan)
    printf '%s\n' \
      "Relay baseline: $expected_baseline" \
      'handshakes: escaped keys and values, accepted and rejected' \
      'fragments: 33554417, 33554418, 33554419 assembled bytes' \
      'comparison: byte-for-byte raw output'
    exit 0
    ;;
  --preflight-only)
    preflight
    printf 'Relay baseline preflight passed: %s\n' "$expected_baseline"
    exit 0
    ;;
  --compare-files)
    if [ "$#" -ne 3 ]; then
      printf 'usage: %s --compare-files BASELINE_OUTPUT RUST_OUTPUT\n' "$0" >&2
      exit 2
    fi
    compare_files "$2" "$3"
    exit
    ;;
  '') ;;
  *)
    printf 'usage: %s [--print-plan|--preflight-only|--compare-files BASELINE_OUTPUT RUST_OUTPUT]\n' "$0" >&2
    exit 2
    ;;
esac

preflight
command -v docker >/dev/null
command -v tmux >/dev/null

if [ -n "${PASEO_RELAY_RESIDUAL_OUTPUT:-}" ]; then
  output_root=$PASEO_RELAY_RESIDUAL_OUTPUT
  mkdir "$output_root"
else
  mkdir -p "$repository_root/target/phase2"
  output_root=$(mktemp -d "$repository_root/target/phase2/relay-residual-differential.XXXXXX")
fi
build_root=$(mktemp -d "$repository_root/target/phase2/spocky-relay-residual-original.XXXXXX")
baseline_session="relay-baseline-residual-$$"
rust_session="relay-rust-residual-$$"
baseline_container="relay-baseline-residual-$$"
baseline_server_log="$output_root/baseline-server.log"
rust_server_log="$output_root/rust-server.log"

cleanup() {
  if tmux has-session -t "$rust_session" 2>/dev/null; then
    tmux kill-session -t "$rust_session"
  fi
  if tmux has-session -t "$baseline_session" 2>/dev/null; then
    tmux kill-session -t "$baseline_session"
  fi
  if docker container inspect "$baseline_container" >/dev/null 2>&1; then
    docker rm -f "$baseline_container" >/dev/null
  fi
  rm -rf "$build_root"
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline_root" archive "$expected_baseline" | tar -x -C "$build_root"
docker run --rm \
  -e MIX_ENV=prod \
  -e MIX_HOME=/work/.mix \
  -v "$build_root:/work" \
  -w /work \
  "$elixir_image" \
  sh -lc 'mix local.hex --force >/dev/null && mix local.rebar --force >/dev/null && mix deps.get --only prod && mix compile'

target_dir=${CARGO_TARGET_DIR:-$repository_root/target/relay-residuals}
CARGO_TARGET_DIR="$target_dir" cargo build -p spocky-relay-pilot \
  --bin relay-residual-probe --bin spocky-relay-network-node

tmux new-session -d -s "$baseline_session" \
  "docker run --rm --name '$baseline_container' -p 127.0.0.1::4000 -e MIX_ENV=prod -e MIX_HOME=/work/.mix -e PASEO_RELAY_HOST=0.0.0.0 -e PASEO_RELAY_PORT=4000 -v '$build_root:/work' -w /work '$elixir_image' sh -lc 'exec mix run --no-halt' 2>&1 | tee '$baseline_server_log'"

baseline_port=''
attempt=0
while [ "$attempt" -lt 60 ]; do
  baseline_port=$(docker port "$baseline_container" 4000/tcp 2>/dev/null | sed -n 's/.*://p' | head -1)
  [ -n "$baseline_port" ] && break
  attempt=$((attempt + 1))
  sleep 1
done
if [ -z "$baseline_port" ]; then
  printf 'Baseline relay failed to publish a port\n' >&2
  sed -n '1,160p' "$baseline_server_log" >&2 || true
  exit 1
fi
if [ "$baseline_port" = 6767 ]; then
  printf 'Docker selected forbidden production port 6767\n' >&2
  exit 1
fi

attempt=0
until curl --silent --fail "http://127.0.0.1:$baseline_port/health" >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 60 ]; then
    printf 'Baseline relay failed health check\n' >&2
    sed -n '1,160p' "$baseline_server_log" >&2 || true
    exit 1
  fi
  sleep 1
done

tmux new-session -d -s "$rust_session" \
  "tail -f /dev/null | '$target_dir/debug/spocky-relay-network-node' residual 2>&1 | tee '$rust_server_log'"
attempt=0
while [ "$attempt" -lt 60 ]; do
  if [ -f "$rust_server_log" ]; then
    rust_address=$(awk -F '\t' '$1 == "READY" {print $5; exit}' "$rust_server_log")
  fi
  [ -n "${rust_address:-}" ] && break
  attempt=$((attempt + 1))
  sleep 1
done
if [ -z "${rust_address:-}" ]; then
  printf 'Rust relay failed to report readiness\n' >&2
  sed -n '1,160p' "$rust_server_log" >&2 || true
  exit 1
fi
if [ "${rust_address##*:}" = 6767 ]; then
  printf 'Rust selected forbidden production port 6767\n' >&2
  exit 1
fi

"$target_dir/debug/relay-residual-probe" "127.0.0.1:$baseline_port" \
  >"$output_root/baseline.tsv"
"$target_dir/debug/relay-residual-probe" "$rust_address" \
  >"$output_root/rust.tsv"
compare_files "$output_root/baseline.tsv" "$output_root/rust.tsv"

cp "$output_root/baseline.tsv" "$output_root/comparison.tsv"
printf 'relay residual parity: 7/7\n'
shasum -a 256 \
  "$output_root/baseline.tsv" \
  "$output_root/rust.tsv" \
  "$output_root/comparison.tsv"
