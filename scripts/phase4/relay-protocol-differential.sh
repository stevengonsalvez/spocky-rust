#!/usr/bin/env bash
# Differential of spocky-relay-protocol against the pinned relay (Docker + Elixir 1.20 / OTP 29).
#   relay-protocol-differential.sh              compare the committed fixtures with a fresh baseline run
#   relay-protocol-differential.sh regenerate   regenerate corpus and fixtures, then compare again
# Every comparison is raw text with diff. Masked: ts (wall_clock) and a generated v2 connection
# id (generated_id). Each docker run has a time limit and is removed on exit.
set -euo pipefail
export GIT_OPTIONAL_LOCKS=0

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
limit=${SPOCKY_RELAY_DOCKER_TIMEOUT:-900}
mode=${1:-compare}
container="spocky-p4-relay-$$"

actual=$(git -C "$baseline_source" rev-parse HEAD)
if [[ "$actual" != "$expected_commit" ]]; then
  echo "relay baseline mismatch: expected $expected_commit, got $actual" >&2
  exit 1
fi
if [[ -n "$(git -C "$baseline_source" status --porcelain)" ]]; then
  echo "relay baseline is dirty" >&2
  exit 1
fi

cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

# The Docker VM mounts $HOME only. The pinned source is copied, never edited.
mkdir -p "$work/io" "$work/mix" "$fixtures"
rsync -a --delete --exclude .git --exclude _build --exclude deps "$baseline_source/" "$work/baseline/"
cp "$repo_root/scripts/phase4/relay-protocol-baseline.exs" "$work/io/baseline.exs"
cp "$repo_root/scripts/phase4/relay-protocol-live.exs" "$work/io/live.exs"

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

# One gated build of every test binary; they then run outside the gate. A prebuilt list
# of binaries (one path per line in SPOCKY_RELAY_BINARIES) skips it.
build_binaries() {
  CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR="$target" "$gate" \
    cargo test -p spocky-relay-protocol --no-run --message-format=json 2>/dev/null |
    grep -o '"executable":"[^"]*"' | sed 's/"executable":"//; s/"$//'
}
binaries=${SPOCKY_RELAY_BINARIES:-$(build_binaries)}
binary() { printf '%s\n' "$binaries" | grep "/$1-" | head -1; }
differential=$(binary baseline_differential)
live_wire=$(binary live_wire_baseline)

capture() { # corpus-file baseline-out live-out generated-out
  cp "$1" "$work/io/corpus.tsv"
  docker_run 'mix run --no-start /io/baseline.exs /io/corpus.tsv /io/baseline.tsv'
  cp "$fixtures/relay-protocol-extra-corpus.tsv" "$work/io/extra-corpus.tsv" 2>/dev/null || true
  docker_run 'mix run --no-start /io/baseline.exs /io/extra-corpus.tsv /io/extra-baseline.tsv'
  docker_run 'mix run /io/live.exs /io/live.tsv /io/generated.tsv' >"$work/live.log" 2>&1
}

if [[ "$mode" == "regenerate" ]]; then
  SPOCKY_RELAY_CORPUS_OUT="$fixtures/relay-protocol-corpus.tsv" "$differential" --ignored write_corpus
  SPOCKY_RELAY_CORPUS_OUT="$fixtures/relay-protocol-extra-corpus.tsv" "$differential" --ignored write_extra_corpus
  capture "$fixtures/relay-protocol-corpus.tsv"
  cp "$work/io/baseline.tsv" "$fixtures/relay-protocol-baseline.tsv"
  cp "$work/io/extra-baseline.tsv" "$fixtures/relay-protocol-extra-baseline.tsv"
  cp "$work/io/live.tsv" "$fixtures/relay-protocol-live-wire.tsv"
  cp "$work/io/generated.tsv" "$fixtures/relay-protocol-live-generated.tsv"
fi

# Compare pass: a fresh capture against the committed fixtures, then the Rust crate against
# the fresh capture. After regenerate this is a second run, which also shows determinism.
capture "$fixtures/relay-protocol-corpus.tsv"
diff "$fixtures/relay-protocol-baseline.tsv" "$work/io/baseline.tsv"
diff "$fixtures/relay-protocol-extra-baseline.tsv" "$work/io/extra-baseline.tsv"
diff "$fixtures/relay-protocol-live-wire.tsv" "$work/io/live.tsv"

for pair in "relay-protocol-corpus:baseline" "relay-protocol-extra-corpus:extra-baseline"; do
  corpus=${pair%%:*}
  baseline=${pair##*:}
  SPOCKY_RELAY_CORPUS_IN="$fixtures/$corpus.tsv" SPOCKY_RELAY_RENDER_OUT="$work/rust-$baseline.tsv" \
    "$differential" --ignored render_corpus_file
  diff "$work/io/$baseline.tsv" "$work/rust-$baseline.tsv"
done

SPOCKY_RELAY_LIVE_FIXTURE="$work/io/live.tsv" SPOCKY_RELAY_GENERATED_FIXTURE="$work/io/generated.tsv" \
  "$live_wire" 2>&1 | tee "$work/live-wire-test.log"
grep -q "test result: ok" "$work/live-wire-test.log"

echo "relay protocol differential: $(($(wc -l <"$work/io/baseline.tsv") + $(wc -l <"$work/io/extra-baseline.tsv"))) corpus cases and $(wc -l <"$work/io/live.tsv") live wire cases: pinned relay identical to the fixtures, Rust identical to the pinned relay"
