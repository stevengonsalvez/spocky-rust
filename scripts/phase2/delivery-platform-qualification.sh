#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline=5de45e208690b0efc51c59a585ae9729325a9204
evidence_dir="$repository_root/evidence/phase2"
linux_dir="$evidence_dir/delivery-linux-runtime"
expected_log_digest=ca80b66d7f25e2faf58587a311abfa46e37e0814c1414c2507a03eb3e02f94b7
expected_metadata_digest=31b4371386bbbd0d0d0004f657f8619716670170b5fed343482e324d9c592f53
expected_report_digest=e81f3644a06089dbed8835d8f6b0745914b0c517b67b0b407c4d8a691e7a5179

usage() {
  printf 'usage: %s --print-plan|--verify-linux|--macos\n' "$0" >&2
}

print_plan() {
  printf '%s\n' "baseline: paseo@$baseline"
  printf '%s\n' 'macOS: unsigned app lifecycle runtime, bound 600 seconds'
  printf '%s\n' 'Linux: retained AppImage and deb runtime, 8+1 tests, digest verified'
  printf '%s\n' 'Windows: crate compile only; packaging and runtime unqualified'
  printf '%s\n' 'Android: package install and update lifecycle unqualified'
  printf '%s\n' 'iOS: SDK and simctl unavailable; package lifecycle unqualified'
  printf '%s\n' 'browser: packaging and update lifecycle unqualified'
  printf '%s\n' 'signing: no signed artifact, notarization, Gatekeeper, or production key use'
  printf '%s\n' 'safety: no host install, deployment, publication, signing, or port 6767'
}

assert_digest() {
  file=$1
  expected=$2
  actual=$(shasum -a 256 "$file" | awk '{print $1}')
  if [ "$actual" != "$expected" ]; then
    printf 'digest mismatch for %s: expected %s, got %s\n' \
      "$file" "$expected" "$actual" >&2
    exit 1
  fi
}

verify_linux() {
  log_file="$linux_dir/container.log"
  metadata_file="$linux_dir/run-metadata.json"
  report_file="$linux_dir/run/linux-delivery-report.json"
  assert_digest "$log_file" "$expected_log_digest"
  assert_digest "$metadata_file" "$expected_metadata_digest"
  assert_digest "$report_file" "$expected_report_digest"
  grep -Fx 'running 8 tests' "$log_file" >/dev/null
  grep -Fx 'running 1 test' "$log_file" >/dev/null
  grep -Fx 'LINUX_DELIVERY_OK' "$log_file" >/dev/null
  jq -e --arg baseline "paseo@$baseline" '
    .baseline == $baseline and
    .exitStatus == 0 and
    .timeoutSeconds == 1500
  ' "$metadata_file" >/dev/null
  jq -e --arg baseline "paseo@$baseline" '
    .baseline == $baseline and
    .contractId == "P2-DELIVERY-01" and
    .appimageCorruptUpdatePreservedInstall == true and
    .debCorruptUpdatePreservedInstall == true and
    (.steps | length) == 15
  ' "$report_file" >/dev/null
  printf '%s\n' DELIVERY_PLATFORM_LINUX_EVIDENCE_OK
  shasum -a 256 "$log_file" "$metadata_file" "$report_file"
}

run_macos() {
  [ "$(uname -s)" = Darwin ] || {
    printf '%s\n' 'macOS delivery qualification requires a Darwin host' >&2
    exit 1
  }
  command -v gtimeout >/dev/null 2>&1 || {
    printf '%s\n' 'gtimeout is required for bounded macOS delivery qualification' >&2
    exit 1
  }
  mkdir -p "$evidence_dir"
  log_file="$evidence_dir/delivery-platform-macos.log"
  : >"$log_file"
  set +e
  gtimeout 600 sh "$repository_root/scripts/phase2/delivery-macos-runtime.test.sh" \
    >"$log_file" 2>&1
  run_status=$?
  set -e
  if [ "$run_status" -ne 0 ]; then
    printf 'macOS delivery qualification failed with status %s: %s\n' \
      "$run_status" "$log_file" >&2
    exit "$run_status"
  fi
  printf '%s\n' DELIVERY_PLATFORM_MACOS_OK >>"$log_file"
  printf 'macOS delivery qualification passed: %s\n' "$log_file"
  shasum -a 256 "$log_file"
}

case "${1:-}" in
  --print-plan)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    print_plan
    ;;
  --verify-linux)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    verify_linux
    ;;
  --macos)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    run_macos
    ;;
  *)
    usage
    exit 2
    ;;
esac

