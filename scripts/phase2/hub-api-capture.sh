#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
default_baseline="$repository_root/../../paseo/paseo-rust/.baselines/hub"
baseline_root=${PASEO_HUB_BASELINE_ROOT:-"$default_baseline"}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
fixture="$repository_root/scripts/phase2/hub-api-original.integration.test.ts"
generator="$repository_root/scripts/phase2/hub-api-cases.mjs"
cases="$repository_root/scripts/phase2/hub-api-cases.json"
raw_dir="$repository_root/evidence/raw/phase2"
result_file="$raw_dir/hub-api-original.json"
openapi_file="$raw_dir/hub-api-openapi-original.json"
log_file="$raw_dir/hub-api-original.log"
install_log="$raw_dir/hub-api-npm-ci.log"

actual_baseline=$(git -C "$baseline_root" rev-parse HEAD)
if [ "$actual_baseline" != "$expected_baseline" ]; then
  printf 'Hub baseline HEAD mismatch: expected %s, got %s\n' "$expected_baseline" "$actual_baseline" >&2
  exit 1
fi
if [ -n "$(git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' "baseline=$actual_baseline"
  printf '%s\n' 'disposable archive only; offline npm cache and execution; port 6767 excluded'
  printf '%s\n' 'evidence/raw/phase2/hub-api-original.json'
  printf '%s\n' 'evidence/raw/phase2/hub-api-openapi-original.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub API capture\n' >&2
  exit 1
fi

# The committed case list must be exactly what the generator prints.
generated=$(mktemp /private/tmp/spocky-hub-api-cases.XXXXXX)
trap 'rm -f "$generated"' EXIT HUP INT TERM
node "$generator" >"$generated"
if ! cmp -s "$generated" "$cases"; then
  printf 'scripts/phase2/hub-api-cases.json is not the output of hub-api-cases.mjs\n' >&2
  exit 1
fi

capture_dir=$(mktemp -d /private/tmp/spocky-hub-api.XXXXXX)
cleanup() {
  rm -f "$generated"
  case "$capture_dir" in
    /private/tmp/spocky-hub-api.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline_root" archive "$actual_baseline" | tar -x -C "$capture_dir"
cp "$fixture" "$capture_dir/src/hub-api-original.integration.test.ts"
cp "$cases" "$capture_dir/hub-api-cases.json"
mkdir -p "$raw_dir"
gtimeout 900 npm ci --offline --ignore-scripts --no-audit --no-fund --prefix "$capture_dir" >"$install_log" 2>&1

(
  cd "$capture_dir"
  TZ=UTC \
  SPOCKY_HUB_API_OUTPUT="$result_file" \
  SPOCKY_HUB_API_OPENAPI_OUTPUT="$openapi_file" \
  SPOCKY_HUB_API_CASES="$capture_dir/hub-api-cases.json" \
  gtimeout 300 ./node_modules/.bin/vitest run \
    src/hub-api-original.integration.test.ts \
    --bail=1 --reporter=verbose
) >"$log_file" 2>&1

printf 'Hub API original captured: %s\n' "$result_file"
printf 'Hub OpenAPI original captured: %s\n' "$openapi_file"
