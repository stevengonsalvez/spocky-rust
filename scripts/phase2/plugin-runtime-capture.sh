#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline_root="$repository_root/.baselines/paseo-runtime"
import_root="$repository_root/.baselines/import"
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
expected_import=8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5
raw_dir="$repository_root/evidence/raw/phase2"
raw_log="$raw_dir/plugin-runtime.log"
raw_traffic="$raw_dir/plugin-runtime-traffic.json"

mkdir -p "$raw_dir"
exec > >(tee "$raw_log") 2>&1

assert_clean_pinned_checkout() {
  local checkout=$1
  local expected=$2
  local actual
  actual=$(git -C "$checkout" rev-parse HEAD)
  if [[ "$actual" != "$expected" ]]; then
    printf 'checkout HEAD mismatch: %s expected %s, got %s\n' "$checkout" "$expected" "$actual" >&2
    exit 1
  fi
  if [[ -n "$(git -C "$checkout" status --porcelain)" ]]; then
    printf 'checkout is dirty: %s\n' "$checkout" >&2
    exit 1
  fi
}

assert_clean_pinned_checkout "$baseline_root" "$expected_baseline"
assert_clean_pinned_checkout "$import_root" "$expected_import"

printf 'baseline=%s\n' "$expected_baseline"
printf 'import=%s\n' "$expected_import"
rustc --version
cargo --version
node --version
npm --version
git --version

timeout 180 "$baseline_root/node_modules/.bin/vitest" run \
  packages/server/src/server/plugins/managed-source.posix.test.ts \
  packages/server/src/server/plugins/settings/index.test.ts \
  --maxWorkers=1 --reporter=default \
  --config "$repository_root/scripts/phase2/plugin-vitest.config.mts" \
  --root "$baseline_root"

timeout 120 cargo test \
  --manifest-path "$repository_root/Cargo.toml" \
  -p paseo-plugin-pilot --test plugin_lifecycle
timeout 120 cargo test \
  --manifest-path "$repository_root/Cargo.toml" \
  -p paseo-plugin-pilot --test runtime_acquisition \
  -- --nocapture --test-threads=1

assert_clean_pinned_checkout "$baseline_root" "$expected_baseline"
assert_clean_pinned_checkout "$import_root" "$expected_import"

sed -n 's/^.*PLUGIN_RUNTIME_EVIDENCE //p' "$raw_log" | jq -s \
  --arg baseline "$expected_baseline" \
  --arg import "$expected_import" \
  '{baseline: $baseline, import: $import, cases: .}' > "$raw_traffic"

if [[ "$(jq '.cases | length' "$raw_traffic")" != "3" ]]; then
  printf 'expected three runtime evidence cases\n' >&2
  exit 1
fi

shasum -a 256 "$raw_traffic"
