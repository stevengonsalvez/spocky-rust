#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
script="$repository_root/scripts/phase2/delivery-macos-runtime.sh"

plan=$("$script" --print-plan)
printf '%s\n' "$plan" | grep -F \
  'baseline: paseo@5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$plan" | grep -F 'package: unsigned macOS Paseo.app bundle'
printf '%s\n' "$plan" | grep -F 'lifecycle: install, launch, reject corrupt update, upgrade, rollback, uninstall'
printf '%s\n' "$plan" | grep -F 'safety: disposable root only; /Applications excluded'
printf '%s\n' "$plan" | grep -F 'safety: signing, notarization, publish, deploy excluded'

output=$(mktemp -d /tmp/spocky-delivery-runtime-test.XXXXXX)
cleanup() {
  case "$output" in
    /tmp/spocky-delivery-runtime-test.*) rm -rf "$output" ;;
    *) printf 'refusing to remove unexpected test directory: %s\n' "$output" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

"$script" --output "$output"
test -f "$output/delivery-runtime-report.json"
test -f "$output/delivery-runtime.log"
jq -e '
  .contractId == "P2-DELIVERY-01" and
  .packageFormat == "unsigned_macos_app_bundle" and
  .corruptUpdatePreservedExecutable == true and
  .corruptUpdatePreservedState == true and
  [.steps[].operation] == [
    "install_launch",
    "corrupt_update_rejected",
    "valid_update_launch",
    "rollback_launch",
    "uninstall_retain_state"
  ] and
  .steps[-1].activeVersion == null
' "$output/delivery-runtime-report.json" >/dev/null
