#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
default_baseline="$repository_root/../../paseo/paseo-rust/.baselines/hub"
baseline_root=${PASEO_HUB_BASELINE_ROOT:-"$default_baseline"}
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
fixture="$repository_root/scripts/phase2/hub-triggers-original.integration.test.ts"
raw_dir="$repository_root/evidence/raw/phase2"
install_log="$raw_dir/hub-triggers-npm-ci.log"
# One capture per host time zone: the UTC capture has a zero local offset, the Europe/London capture
# has offset 0 in January and 60 in August, so the baseline's daylight saving rules are on record.
captures="UTC:hub-triggers-original.json Europe/London:hub-triggers-original-europe-london.json"

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
  printf '%s\n' 'disposable archive only; offline execution; port 6767 excluded'
  printf '%s\n' 'npm ci --offline against the pinned package-lock.json (no network)'
  for capture in $captures; do
    printf '%s\n' "TZ=${capture%%:*} evidence/raw/phase2/${capture#*:}"
  done
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Hub trigger capture\n' >&2
  exit 1
fi

capture_dir=$(mktemp -d /private/tmp/spocky-hub-triggers.XXXXXX)
cleanup() {
  case "$capture_dir" in
    /private/tmp/spocky-hub-triggers.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

git -C "$baseline_root" archive "$actual_baseline" | tar -x -C "$capture_dir"
cp "$fixture" "$capture_dir/src/hub-triggers-original.integration.test.ts"
mkdir -p "$raw_dir"
# The install is offline: every package comes from the local npm cache and is checked against the
# integrity hashes in the pinned package-lock.json. An online install is an explicit opt-in and is
# written to the install log so the evidence states it.
install_mode=--offline
if [ "${SPOCKY_HUB_TRIGGERS_ALLOW_ONLINE_INSTALL:-}" = 1 ]; then
  install_mode=--online
  printf 'ONLINE INSTALL: SPOCKY_HUB_TRIGGERS_ALLOW_ONLINE_INSTALL=1 permitted network access\n' >"$install_log"
else
  : >"$install_log"
fi
if [ "$install_mode" = --offline ]; then
  gtimeout 600 npm ci --offline --prefix "$capture_dir" --ignore-scripts --no-audit --no-fund >>"$install_log" 2>&1 || {
    printf 'offline npm ci failed (see %s); fill the npm cache or set SPOCKY_HUB_TRIGGERS_ALLOW_ONLINE_INSTALL=1\n' "$install_log" >&2
    exit 1
  }
else
  gtimeout 600 npm ci --prefix "$capture_dir" --ignore-scripts --no-audit --no-fund >>"$install_log" 2>&1
fi

for capture in $captures; do
  zone=${capture%%:*}
  result_file="$raw_dir/${capture#*:}"
  log_file="${result_file%.json}.log"
  (
    cd "$capture_dir"
    TZ="$zone" \
    SPOCKY_HUB_TRIGGERS_OUTPUT="$result_file" \
    gtimeout 120 ./node_modules/.bin/vitest run \
      src/hub-triggers-original.integration.test.ts \
      --bail=1 --reporter=verbose
  ) >"$log_file" 2>&1
  printf 'Hub trigger original captured (TZ=%s): %s\n' "$zone" "$result_file"
done
