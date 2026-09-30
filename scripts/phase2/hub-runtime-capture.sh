#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
baseline_root=${PASEO_HUB_BASELINE_ROOT:-"$repository_root/.baselines/hub"}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
raw_dir="$repository_root/evidence/raw/phase2"
result_file="$raw_dir/hub-runtime-original.json"
log_file="$raw_dir/hub-runtime-original.log"
install_log="$raw_dir/hub-runtime-npm-ci.log"
runtime_tests='src/db/runtime/embedded-persistence.integration.test.ts
src/index.embedded.integration.test.ts
src/instance-setup/environment-bootstrap.integration.test.ts'

actual_baseline=$(git -C "$baseline_root" rev-parse HEAD)
if [ "$actual_baseline" != "$expected_baseline" ]; then
  printf 'Hub baseline HEAD mismatch: expected %s, got %s\n' \
    "$expected_baseline" "$actual_baseline" >&2
  exit 1
fi
if [ -n "$(git -C "$baseline_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Hub baseline tracked tree is dirty: %s\n' "$baseline_root" >&2
  exit 1
fi

if [ "${1:-}" = "--preflight-only" ]; then
  printf 'Hub baseline preflight passed: %s\n' "$actual_baseline"
  printf 'safety boundary: disposable local runtimes only; port 6767 excluded\n'
  exit 0
fi

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' "$runtime_tests"
  printf '%s\n' "evidence/raw/phase2/hub-runtime-original.json"
  printf '%s\n' 'Ryuk disabled; suite stops exact PostgreSQL container'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--preflight-only|--print-plan]\n' "$0" >&2
  exit 2
fi

if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub runtime capture\n' >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  printf 'Docker is required for disposable PostgreSQL parity evidence\n' >&2
  exit 1
fi

capture_dir=$(mktemp -d /private/tmp/paseo-hub-runtime.XXXXXX)
cleanup() {
  case "$capture_dir" in
    /private/tmp/paseo-hub-runtime.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing to remove unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline_root" archive "$actual_baseline" | tar -x -C "$capture_dir"
mkdir -p "$raw_dir"

set +e
gtimeout 600 npm ci \
  --prefix "$capture_dir" \
  --ignore-scripts \
  --no-audit \
  --no-fund >"$install_log" 2>&1
install_status=$?
set -e
if [ "$install_status" -ne 0 ]; then
  printf 'Hub dependency install failed with status %s; see %s\n' \
    "$install_status" "$install_log" >&2
  exit "$install_status"
fi

set +e
(
  cd "$capture_dir"
  PORT=38941 TESTCONTAINERS_RYUK_DISABLED=true gtimeout 900 ./node_modules/.bin/vitest run \
    src/db/runtime/embedded-persistence.integration.test.ts \
    src/index.embedded.integration.test.ts \
    src/instance-setup/environment-bootstrap.integration.test.ts \
    --bail=1 \
    --reporter=verbose \
    --reporter=json \
    --outputFile.json="$result_file"
) >"$log_file" 2>&1
test_status=$?
set -e

if [ "$test_status" -ne 0 ]; then
  printf 'Hub runtime evidence failed with status %s; see %s\n' \
    "$test_status" "$log_file" >&2
  exit "$test_status"
fi

printf 'Hub runtime evidence captured: %s\n' "$result_file"
printf 'Hub runtime log captured: %s\n' "$log_file"
