#!/usr/bin/env bash
# Differential of spocky-relay's Capacity against the pinned relay (Docker + Elixir 1.20 / OTP 29).
#   relay-capacity-differential.sh              replay a fresh pinned run of the committed operations
#   relay-capacity-differential.sh regenerate   rewrite the operation script and baseline fixtures first
# The BEAM memory reading is an input the baseline records (`~ memory=`), so two pinned runs
# differ in those numbers: the check is the Rust replay of a fresh capture, not a diff of captures.
set -euo pipefail
export GIT_OPTIONAL_LOCKS=0

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
common_root=$(cd "$(git -C "$repo_root" rev-parse --git-common-dir)/.." && pwd)
baseline_source=${SPOCKY_RELAY_BASELINE:-$common_root/.baselines/relay}
expected_commit=3fc41c96c8c63f3a7109e832899cc57d473c4531
elixir_image="elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79"
work=${SPOCKY_RELAY_WORK:-$HOME/.cache/spocky-p4-relay}
target=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p4_relay}
fixtures="$repo_root/crates/spocky-relay/tests/fixtures"
gate=/private/tmp/spocky-targets/build-gate.sh
# Only the local lane has the build gate and GNU timeout under its g-prefixed name.
[[ -x "$gate" ]] && gate_cmd="$gate" || gate_cmd=
timeout_cmd=${SPOCKY_TIMEOUT:-gtimeout}
limit=${SPOCKY_RELAY_DOCKER_TIMEOUT:-900}
mode=${1:-compare}
container="spocky-p4-relay-capacity-$$"

[[ "$(git -C "$baseline_source" rev-parse HEAD)" == "$expected_commit" ]] || { echo "relay baseline mismatch" >&2; exit 1; }
[[ -z "$(git -C "$baseline_source" status --porcelain)" ]] || { echo "relay baseline is dirty" >&2; exit 1; }

cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

mkdir -p "$work/io" "$work/mix" "$fixtures"
rsync -a --delete --exclude .git --exclude _build --exclude deps "$baseline_source/" "$work/baseline/"
cp "$repo_root/scripts/phase4/relay-capacity-baseline.exs" "$work/io/capacity.exs"

docker_run() {
  $timeout_cmd --kill-after=30 "$limit" docker run --rm --name "$container" -e MIX_ENV=test \
    -v "$work/baseline:/work" -v "$work/io:/io" -v "$work/mix:/root/.mix" \
    -w /work "$elixir_image" sh -c "$1"
}
if [[ ! -d "$work/mix/archives" ]]; then
  docker_run 'mix local.hex --force >/dev/null && mix local.rebar --force >/dev/null'
fi
if [[ ! -d "$work/baseline/_build" ]]; then
  docker_run 'mix deps.get && mix compile'
fi

binary=${SPOCKY_RELAY_CAPACITY_BINARY:-$(CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target" $gate_cmd \
  cargo test -p spocky-relay --no-run --message-format=json |
  grep -o '"executable":"[^"]*capacity_baseline-[^"]*"' | sed 's/"executable":"//; s/"$//' | head -1)}

if [[ "$mode" == "regenerate" ]]; then
  SPOCKY_RELAY_OPS_OUT="$fixtures/relay-capacity-ops.txt" "$binary" --ignored write_ops
fi
cp "$fixtures/relay-capacity-ops.txt" "$work/io/ops.txt"
docker_run 'mix run --no-start /io/capacity.exs /io/ops.txt /io/baseline.txt'
if [[ "$mode" == "regenerate" ]]; then
  cp "$work/io/baseline.txt" "$fixtures/relay-capacity-baseline.txt"
fi
SPOCKY_RELAY_CAPACITY_BASELINE="$work/io/baseline.txt" "$binary" 2>&1 | tee "$work/capacity-test.log"
grep -q "test result: ok" "$work/capacity-test.log"
echo "relay capacity differential: $(grep -c '^> ' "$work/io/baseline.txt") operations: Rust identical to the pinned relay"
