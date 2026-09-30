#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline=5de45e208690b0efc51c59a585ae9729325a9204

print_plan() {
  printf '%s\n' "baseline: paseo@$baseline"
  printf '%s\n' 'package: unsigned macOS Paseo.app bundle'
  printf '%s\n' 'lifecycle: install, launch, reject corrupt update, upgrade, rollback, uninstall'
  printf '%s\n' 'safety: disposable root only; /Applications excluded'
  printf '%s\n' 'safety: signing, notarization, publish, deploy excluded'
}

if [ "${1:-}" = "--print-plan" ]; then
  [ "$#" -eq 1 ] || {
    printf 'usage: %s [--print-plan|--output DISPOSABLE_ROOT]\n' "$0" >&2
    exit 2
  }
  print_plan
  exit 0
fi

if [ "${1:-}" = "--output" ]; then
  [ "$#" -eq 2 ] || {
    printf 'usage: %s [--print-plan|--output DISPOSABLE_ROOT]\n' "$0" >&2
    exit 2
  }
  output=$2
elif [ "$#" -eq 0 ]; then
  output=$(mktemp -d /tmp/spocky-delivery-runtime.XXXXXX)
else
  printf 'usage: %s [--print-plan|--output DISPOSABLE_ROOT]\n' "$0" >&2
  exit 2
fi

case "$output" in
  /tmp/spocky-delivery-runtime*|/private/tmp/spocky-delivery-runtime*) ;;
  *)
    printf 'output must be a disposable spocky-delivery-runtime path under /tmp: %s\n' \
      "$output" >&2
    exit 2
    ;;
esac

if [ "$(uname -s)" != "Darwin" ]; then
  printf 'macOS host required for app bundle execution\n' >&2
  exit 1
fi
command -v jq >/dev/null 2>&1 || {
  printf 'jq is required\n' >&2
  exit 1
}

mkdir -p "$output"
log="$output/delivery-runtime.log"
set +e
cargo run \
  --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-audio-delivery-pilot \
  --bin spocky-macos-delivery-runtime \
  -- "$output" >"$log" 2>&1
run_status=$?
set -e
cat "$log"
if [ "$run_status" -ne 0 ]; then
  exit "$run_status"
fi

report="$output/delivery-runtime-report.json"
jq -e '
  .baseline == "paseo@5de45e208690b0efc51c59a585ae9729325a9204" and
  .contractId == "P2-DELIVERY-01" and
  .corruptUpdatePreservedExecutable == true and
  .corruptUpdatePreservedState == true and
  .steps[-1].activeVersion == null
' "$report" >/dev/null

printf 'artifacts:\n'
shasum -a 256 "$report" "$log"
