#!/usr/bin/env bash
# Differential of spocky-relay-protocol against the pinned relay (Docker + Elixir 1.20 / OTP 29).
#   relay-protocol-differential.sh              compare the committed fixtures with a fresh baseline run
#   relay-protocol-differential.sh regenerate   regenerate the corpus and both baseline fixtures first
# Raw text comparison with diff. Masked: the random connection id (generated_id) and ts (wall_clock).
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# Baselines are excluded from Git, so a linked worktree reads them from the main checkout.
common_root=$(cd "$(git -C "$repo_root" rev-parse --git-common-dir)/.." && pwd)
baseline_source=${SPOCKY_RELAY_BASELINE:-$common_root/.baselines/relay}
expected_commit=3fc41c96c8c63f3a7109e832899cc57d473c4531
elixir_image="elixir@sha256:c915d900894e1d664cd8ed72fd2c38fce72b612cc1757d57f86a0cc62e62dd79"
work=${SPOCKY_RELAY_WORK:-$HOME/.cache/spocky-p4-relay}
target=${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p4_relay}
fixtures="$repo_root/crates/spocky-relay-protocol/tests/fixtures"
gate=/private/tmp/spocky-targets/build-gate.sh
mode=${1:-compare}

actual=$(git -C "$baseline_source" rev-parse HEAD)
if [[ "$actual" != "$expected_commit" ]]; then
  echo "relay baseline mismatch: expected $expected_commit, got $actual" >&2
  exit 1
fi
if [[ -n "$(git -C "$baseline_source" status --porcelain)" ]]; then
  echo "relay baseline is dirty" >&2
  exit 1
fi

# The Docker VM mounts $HOME only. The pinned source is copied, never edited.
mkdir -p "$work/io" "$work/mix" "$fixtures"
rsync -a --delete --exclude .git --exclude _build --exclude deps "$baseline_source/" "$work/baseline/"
cp "$repo_root/scripts/phase4/relay-protocol-baseline.exs" "$work/io/baseline.exs"
cp "$repo_root/scripts/phase4/relay-protocol-live.exs" "$work/io/live.exs"

docker_run() {
  docker run --rm --name "spocky-p4-relay-$$" -e MIX_ENV=test \
    -v "$work/baseline:/work" -v "$work/io:/io" -v "$work/mix:/root/.mix" \
    -w /work "$elixir_image" sh -c "$1"
}

if [[ ! -d "$work/mix/archives" ]]; then
  docker_run 'mix local.hex --force >/dev/null && mix local.rebar --force >/dev/null'
fi
if [[ ! -d "$work/baseline/_build" ]]; then
  docker_run 'mix deps.get && mix compile'
fi

# One gated build; the test binary then runs outside the gate. A prebuilt binary skips it.
test_binary=${SPOCKY_RELAY_TEST_BINARY:-$(
  CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target" "$gate" \
    cargo test -p spocky-relay-protocol --test baseline_differential --no-run --message-format=json 2>/dev/null |
    grep -o '"executable":"[^"]*"' | sed 's/"executable":"//; s/"$//' | head -1
)}

if [[ "$mode" == "regenerate" ]]; then
  SPOCKY_RELAY_CORPUS_OUT="$fixtures/relay-protocol-corpus.tsv" "$test_binary" --ignored write_corpus
fi

cp "$fixtures/relay-protocol-corpus.tsv" "$work/io/corpus.tsv"
docker_run 'mix run --no-start /io/baseline.exs /io/corpus.tsv /io/baseline.tsv'
docker_run 'mix run /io/live.exs /io/live.tsv' >"$work/live.log" 2>&1

if [[ "$mode" == "regenerate" ]]; then
  cp "$work/io/baseline.tsv" "$fixtures/relay-protocol-baseline.tsv"
  cp "$work/io/live.tsv" "$fixtures/relay-protocol-live-baseline.tsv"
fi

SPOCKY_RELAY_CORPUS_IN="$fixtures/relay-protocol-corpus.tsv" \
  SPOCKY_RELAY_RENDER_OUT="$work/rust.tsv" "$test_binary" --ignored render_corpus_file
diff "$work/io/baseline.tsv" "$work/rust.tsv"
diff "$fixtures/relay-protocol-baseline.tsv" "$work/io/baseline.tsv"
diff "$fixtures/relay-protocol-live-baseline.tsv" "$work/io/live.tsv"
echo "relay protocol differential: $(wc -l <"$work/io/baseline.tsv" | tr -d ' ') corpus cases and $(wc -l <"$work/io/live.tsv" | tr -d ' ') live wire cases, raw text identical"
