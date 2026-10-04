#!/usr/bin/env bash
# Differential of spocky-relay's Writer against the pinned relay (Docker + Elixir 1.20 / OTP 29).
#   relay-writer-differential.sh              replay a fresh pinned run of the committed operations
#   relay-writer-differential.sh regenerate   rewrite the operation script and baseline fixtures first
# The Writer's timers use far deadlines; reservation timeouts and exits are delivered as messages.
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
limit=${SPOCKY_RELAY_DOCKER_TIMEOUT:-900}
mode=${1:-compare}
container="spocky-p4-relay-writer-$$"

[[ "$(git -C "$baseline_source" rev-parse HEAD)" == "$expected_commit" ]] || { echo "relay baseline mismatch" >&2; exit 1; }
[[ -z "$(git -C "$baseline_source" status --porcelain)" ]] || { echo "relay baseline is dirty" >&2; exit 1; }

cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

mkdir -p "$work/io" "$work/mix" "$fixtures"
rsync -a --delete --exclude .git --exclude _build --exclude deps "$baseline_source/" "$work/baseline/"
cp "$repo_root/scripts/phase4/relay-writer-baseline.exs" "$work/io/writer.exs"

docker_run() {
  gtimeout --kill-after=30 "$limit" docker run --rm --name "$container" -e MIX_ENV=test \
    -v "$work/baseline:/work" -v "$work/io:/io" -v "$work/mix:/root/.mix" \
    -w /work "$elixir_image" sh -c "$1"
}
if [[ ! -d "$work/mix/archives" ]]; then
  docker_run 'mix local.hex --force >/dev/null && mix local.rebar --force >/dev/null'
fi
if [[ ! -d "$work/baseline/_build" ]]; then
  docker_run 'mix deps.get && mix compile'
fi

binary=${SPOCKY_RELAY_WRITER_BINARY:-$(CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target" "$gate" \
  cargo test -p spocky-relay --no-run --message-format=json 2>/dev/null |
  grep -o '"executable":"[^"]*writer_baseline-[^"]*"' | sed 's/"executable":"//; s/"$//' | head -1)}

if [[ "$mode" == "regenerate" ]]; then
  SPOCKY_RELAY_OPS_OUT="$fixtures/relay-writer-ops.txt" "$binary" --ignored write_ops
fi
cp "$fixtures/relay-writer-ops.txt" "$work/io/ops.txt"
docker_run 'mix run --no-start /io/writer.exs /io/ops.txt /io/baseline.txt'
if [[ "$mode" == "regenerate" ]]; then
  cp "$work/io/baseline.txt" "$fixtures/relay-writer-baseline.txt"
fi
# The transcript has one clock input: how many milliseconds after the deadline was set the Writer
# granted a reservation (`~ elapsed=` and the `t` timer lines derived from it). Two pinned runs
# therefore differ in those lines and in nothing else; the comparison with the port is the replay
# of a fresh capture, raw.
cp "$work/io/baseline.txt" "$work/io/baseline-first.txt"
docker_run 'mix run --no-start /io/writer.exs /io/ops.txt /io/baseline.txt'
clockless() { grep -v '^~ elapsed=\|^t ' "$1"; }
diff <(clockless "$work/io/baseline-first.txt") <(clockless "$work/io/baseline.txt")
diff <(clockless "$fixtures/relay-writer-baseline.txt") <(clockless "$work/io/baseline.txt")
SPOCKY_RELAY_WRITER_BASELINE="$work/io/baseline.txt" "$binary" 2>&1 | tee "$work/writer-test.log"
grep -q "test result: ok" "$work/writer-test.log"
echo "relay writer differential: $(grep -c '^> ' "$work/io/baseline.txt") operations: Rust identical to the pinned relay"
