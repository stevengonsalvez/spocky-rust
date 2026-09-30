#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
baseline_root=${PASEO_HUB_BASELINE_ROOT:-"$repository_root/.baselines/hub"}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
fixture="$repository_root/scripts/phase2/fixtures/hub-account-recovery-trace.integration.test.ts"
raw_dir="$repository_root/evidence/raw/phase2"
result_file="$raw_dir/hub-account-recovery-original.json"
log_file="$raw_dir/hub-account-recovery-original.log"
install_log="$raw_dir/hub-account-recovery-npm-ci.log"

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
  printf '%s\n' 'disposable archive and PostgreSQL only; port 6767 excluded'
  printf '%s\n' 'evidence/raw/phase2/hub-account-recovery-original.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub recovery capture\n' >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  printf 'Docker is required for disposable PostgreSQL recovery evidence\n' >&2
  exit 1
fi

capture_dir=$(mktemp -d /private/tmp/paseo-hub-recovery.XXXXXX)
cleanup() {
  case "$capture_dir" in
    /private/tmp/paseo-hub-recovery.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline_root" archive "$actual_baseline" | tar -x -C "$capture_dir"
cp "$fixture" "$capture_dir/src/account-recovery-trace.integration.test.ts"
mkdir -p "$raw_dir"
gtimeout 600 npm ci --prefix "$capture_dir" --ignore-scripts --no-audit --no-fund >"$install_log" 2>&1

(
  cd "$capture_dir"
  PASEO_ACCOUNT_RECOVERY_OUTPUT="$result_file" \
  PASEO_ACCOUNT_RECOVERY_DATA="$capture_dir/recovery-data" \
  PASEO_HUB_BASELINE="$actual_baseline" \
  TESTCONTAINERS_RYUK_DISABLED=true \
  gtimeout 300 ./node_modules/.bin/vitest run \
    src/account-recovery-trace.integration.test.ts \
    --bail=1 --reporter=verbose
) >"$log_file" 2>&1

printf 'Hub recovery original captured: %s\n' "$result_file"
