#!/bin/sh
# Runs the p3_e2ee_channel acceptance commands and keeps their evidence:
# both raw transcripts (pinned TypeScript and Rust) of every differential
# scenario, each command's log, and the digests of the pinned inputs.
#
# Usage: scripts/phase3/e2ee-differential.sh
#
# Evidence lands in evidence/raw/phase3/e2ee-<utc>/ (untracked) and its
# SHA-256 digests are printed. Exit 0 only when test, clippy, and fmt pass,
# both sides' transcripts of exactly 57 scenarios and 4 interop pairs exist,
# and each is byte-identical. SPOCKY_ALLOW_SKIP is always unset, so the
# differential can never skip.
#
# Env: PASEO_REFERENCE_ROOT (default: the paseo-rewrite sibling of the main
#      checkout), SPOCKY_PASEO_NODE_MODULES (default: the
#      scripts/phase3/build-original.sh install of 5de45e2),
#      CARGO_TARGET_DIR (default /private/tmp/spocky-targets/p3_e2ee_channel),
#      CARGO_BUILD_JOBS (default 2), SPOCKY_BUILD_GATE (default
#      /private/tmp/spocky-targets/build-gate.sh).
set -eu
unset SPOCKY_ALLOW_SKIP

# The scenario count is pinned: a scenario that silently stops running, or
# one added without updating this number, fails the run.
expected_scenarios=57

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
. "$repository_root/scripts/phase3/pins.sh"

common=$(git -C "$repository_root" rev-parse --path-format=absolute --git-common-dir)
reference=${PASEO_REFERENCE_ROOT:-$(dirname "$(dirname "$common")")/paseo-rewrite}
node_modules=${SPOCKY_PASEO_NODE_MODULES:-/private/tmp/spocky-targets/p3_slice_harness/paseo-original-$P3_PASEO_COMMIT/node_modules}
[ "$(git -C "$reference" rev-parse HEAD)" = "$P3_PASEO_COMMIT" ] ||
  p3_fail "reference HEAD is not $P3_PASEO_COMMIT: $reference"
[ -z "$(git -C "$reference" status --porcelain --untracked-files=no)" ] ||
  p3_fail "reference tracked tree is dirty: $reference"
[ -d "$node_modules/tweetnacl" ] ||
  p3_fail "missing pinned dependencies: $node_modules (build them with scripts/phase3/build-original.sh)"
node_bin=$(p3_node_bin_dir)

export SPOCKY_PINNED_NODE="$node_bin/node"
export PASEO_REFERENCE_ROOT="$reference"
export SPOCKY_PASEO_NODE_MODULES="$node_modules"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/spocky-targets/p3_e2ee_channel}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
gate=${SPOCKY_BUILD_GATE:-/private/tmp/spocky-targets/build-gate.sh}

run_id=e2ee-$(date -u +%Y%m%dT%H%M%SZ)
evidence=$repository_root/evidence/raw/phase3/$run_id
mkdir -p "$evidence/transcripts"
export SPOCKY_E2EE_EVIDENCE="$evidence/transcripts"

{
  printf 'commit %s\n' "$(git -C "$repository_root" rev-parse HEAD)"
  printf 'node %s %s\n' "$("$SPOCKY_PINNED_NODE" --version)" "$SPOCKY_PINNED_NODE"
  printf 'reference %s %s\n' "$(git -C "$reference" rev-parse HEAD)" "$reference"
  for file in encrypted-channel.ts crypto.ts base64.ts; do
    printf 'source %s %s\n' "$(p3_sha256 "$reference/packages/relay/src/$file")" "packages/relay/src/$file"
  done
  for file in tweetnacl/nacl-fast.js tweetnacl/package.json base64-js/index.js base64-js/package.json; do
    printf 'module %s %s\n' "$(p3_sha256 "$node_modules/$file")" "$file"
  done
} >"$evidence/inputs.txt"

cd "$repository_root"
status=0
"$gate" cargo test --locked -p spocky-crypto >"$evidence/test.log" 2>&1 || status=1
"$gate" cargo clippy --locked -p spocky-crypto --all-targets -- -D warnings >"$evidence/clippy.log" 2>&1 || status=1
"$gate" cargo fmt --package spocky-crypto -- --check >"$evidence/fmt.log" 2>&1 || status=1

# Both sides must have produced byte-identical transcripts.
for node_transcript in "$evidence"/transcripts/*.node.txt; do
  cmp -s "$node_transcript" "${node_transcript%.node.txt}.rust.txt" || {
    printf 'transcripts differ: %s\n' "$node_transcript" >&2
    status=1
  }
done

for pair_transcript in "$evidence"/transcripts/pair.*.txt; do
  cmp -s "$pair_transcript" "$evidence/transcripts/pair.client-node-daemon-node.txt" || {
    printf 'pair transcripts differ: %s\n' "$pair_transcript" >&2
    status=1
  }
done
[ "$(find "$evidence/transcripts" -name 'pair.*.txt' | wc -l | tr -d ' ')" = 4 ] || {
  printf 'expected four pair transcripts\n' >&2
  status=1
}

scenarios=$(find "$evidence/transcripts" -name '*.node.txt' | wc -l | tr -d ' ')
rust_scenarios=$(find "$evidence/transcripts" -name '*.rust.txt' | wc -l | tr -d ' ')
if [ "$scenarios" != "$expected_scenarios" ] || [ "$rust_scenarios" != "$expected_scenarios" ]; then
  printf 'expected %s scenarios per side, found node %s and rust %s\n' \
    "$expected_scenarios" "$scenarios" "$rust_scenarios" >&2
  status=1
fi

grep -E '^test result' "$evidence/test.log" || true
printf 'scenarios %s\n' "$scenarios"
printf 'evidence %s\n' "$evidence"
for file in "$evidence"/*.txt "$evidence"/*.log; do
  printf '%s  %s\n' "$(p3_sha256 "$file")" "${file#"$repository_root"/}"
done
(cd "$evidence/transcripts" && cat ./*.txt) | shasum -a 256 | awk '{print $1 "  transcripts (concatenated, sorted by name)"}'
[ "$status" -eq 0 ] || p3_fail "$run_id failed; see $evidence"
printf '%s passed\n' "$run_id"
