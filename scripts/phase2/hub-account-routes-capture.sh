#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
default_baseline="$repository_root/../../paseo/paseo-rust/.baselines/hub"
baseline_root=${PASEO_HUB_BASELINE_ROOT:-"$default_baseline"}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
fixture="$repository_root/scripts/phase2/fixtures/hub-account-routes-original.integration.test.ts"
generator="$repository_root/scripts/phase2/hub-account-routes-cases.mjs"
cases="$repository_root/scripts/phase2/hub-account-routes-cases.json"
raw_dir="$repository_root/evidence/raw/phase2"
result_file="$raw_dir/hub-account-routes-original.json"
log_file="$raw_dir/hub-account-routes-original.log"
install_log="$raw_dir/hub-account-routes-npm-ci.log"

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
  printf '%s\n' 'disposable archive only; offline npm cache; embedded PGlite, no container; port 6767 excluded'
  printf '%s\n' 'evidence/raw/phase2/hub-account-routes-original.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub account route capture\n' >&2
  exit 1
fi

# The committed case list must be exactly what the generator prints.
generated=$(mktemp /private/tmp/spocky-hub-account-cases.XXXXXX)
capture_dir=$(mktemp -d /private/tmp/spocky-hub-account-routes.XXXXXX)
cleanup() {
  rm -f "$generated"
  case "$capture_dir" in
    /private/tmp/spocky-hub-account-routes.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM
node "$generator" >"$generated"
if ! cmp -s "$generated" "$cases"; then
  printf 'scripts/phase2/hub-account-routes-cases.json is not the output of hub-account-routes-cases.mjs\n' >&2
  exit 1
fi

git -C "$baseline_root" archive "$actual_baseline" | tar -x -C "$capture_dir"
cp "$fixture" "$capture_dir/src/hub-account-routes-original.integration.test.ts"
cp "$cases" "$capture_dir/hub-account-routes-cases.json"
mkdir -p "$raw_dir"
gtimeout 900 npm ci --offline --ignore-scripts --no-audit --no-fund --prefix "$capture_dir" >"$install_log" 2>&1

(
  cd "$capture_dir"
  TZ=UTC \
  PASEO_HUB_BASELINE="$actual_baseline" \
  SPOCKY_HUB_ACCOUNT_CASES="$capture_dir/hub-account-routes-cases.json" \
  SPOCKY_HUB_ACCOUNT_OUTPUT="$result_file" \
  gtimeout 600 ./node_modules/.bin/vitest run \
    src/hub-account-routes-original.integration.test.ts \
    --bail=1 --reporter=verbose
) >"$log_file" 2>&1

printf 'Hub account routes original captured: %s\n' "$result_file"
