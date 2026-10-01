#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline=5de45e208690b0efc51c59a585ae9729325a9204
evidence_dir="$repository_root/evidence/phase2"
windows_target=x86_64-pc-windows-msvc
xwin_version=0.23.1

usage() {
  printf 'usage: %s --print-plan|--macos|--windows\n' "$0" >&2
}

print_plan() {
  printf '%s\n' "baseline: paseo@$baseline"
  printf '%s\n' 'macOS: targeted audio runtime, 10 tests, bound 600 seconds'
  printf '%s\n' 'Linux: crate all-target compilation retained; audio runtime unqualified'
  printf '%s\n' 'Windows: locked MSVC all-target compile only, bound 1200 seconds'
  printf '%s\n' 'Android: historical OS-command evidence only; selected-app audio unqualified'
  printf '%s\n' 'iOS: SDK and simctl unavailable; runtime unqualified'
  printf '%s\n' 'browser: contract model only; capture and playback runtime unqualified'
  printf '%s\n' 'safety: no device, emulator, signing, deployment, publication, or port 6767'
}

resolve_main_root() {
  common_git_dir=$(git -C "$repository_root" rev-parse --git-common-dir)
  case "$common_git_dir" in
    /*) ;;
    *) common_git_dir="$repository_root/$common_git_dir" ;;
  esac
  CDPATH='' cd -- "$(dirname "$common_git_dir")" && pwd
}

require_gtimeout() {
  command -v gtimeout >/dev/null 2>&1 || {
    printf '%s\n' 'gtimeout is required for bounded platform qualification' >&2
    exit 1
  }
}

assert_test_count() {
  log_file=$1
  expected=$2
  actual=$(grep -Ec '^test .* \.\.\. ok$' "$log_file" || true)
  if [ "$actual" -ne "$expected" ]; then
    printf 'audio qualification executed %s passing tests, expected %s: %s\n' \
      "$actual" "$expected" "$log_file" >&2
    exit 1
  fi
}

run_macos() {
  [ "$(uname -s)" = Darwin ] || {
    printf '%s\n' 'macOS audio qualification requires a Darwin host' >&2
    exit 1
  }
  require_gtimeout
  mkdir -p "$evidence_dir"
  log_file="$evidence_dir/audio-platform-macos.log"
  : >"$log_file"
  {
    uname -a
    rustc --version
    cargo --version
  } >>"$log_file" 2>&1
  set +e
  gtimeout 600 cargo test --locked -p spocky-audio-delivery-pilot \
    --test audio_contract --test local_runtime \
    --test macos_audio_runtime --test runtime_report \
    -- --test-threads=1 >>"$log_file" 2>&1
  run_status=$?
  set -e
  if [ "$run_status" -ne 0 ]; then
    printf 'macOS audio qualification failed with status %s: %s\n' \
      "$run_status" "$log_file" >&2
    exit "$run_status"
  fi
  assert_test_count "$log_file" 10
  printf '%s\n' AUDIO_PLATFORM_MACOS_OK >>"$log_file"
  printf 'macOS audio qualification passed: %s\n' "$log_file"
  shasum -a 256 "$log_file"
}

run_windows() {
  require_gtimeout
  main_root=$(resolve_main_root)
  xwin="$main_root/.tools/bin/cargo-xwin"
  xwin_cache="$main_root/.tools/xwin-cache"
  target_dir=/tmp/spocky-audio-delivery-windows-target
  [ -x "$xwin" ] || {
    printf 'repository-local cargo-xwin is unavailable: %s\n' "$xwin" >&2
    exit 1
  }
  [ "$("$xwin" --version)" = "cargo-xwin $xwin_version" ] || {
    printf 'cargo-xwin %s is required: %s\n' "$xwin_version" "$xwin" >&2
    exit 1
  }
  rustup target list --installed | grep -Fx "$windows_target" >/dev/null || {
    printf 'Rust target is not installed: %s\n' "$windows_target" >&2
    exit 1
  }
  mkdir -p "$evidence_dir" "$xwin_cache" "$target_dir"
  log_file="$evidence_dir/audio-platform-windows.log"
  : >"$log_file"
  {
    rustc --version
    cargo --version
    "$xwin" --version
    printf 'target: %s\n' "$windows_target"
    printf '%s\n' 'qualification: compile only, no Windows runtime claim'
  } >>"$log_file" 2>&1
  set +e
  gtimeout 1200 env XWIN_CACHE_DIR="$xwin_cache" \
    "$xwin" xwin check --locked --target "$windows_target" \
    -p spocky-audio-delivery-pilot --all-targets \
    --target-dir "$target_dir" >>"$log_file" 2>&1
  run_status=$?
  set -e
  if [ "$run_status" -ne 0 ]; then
    printf 'Windows compile qualification failed with status %s: %s\n' \
      "$run_status" "$log_file" >&2
    exit "$run_status"
  fi
  printf '%s\n' AUDIO_PLATFORM_WINDOWS_COMPILE_OK >>"$log_file"
  printf 'Windows audio and delivery compile qualification passed: %s\n' "$log_file"
  shasum -a 256 "$log_file"
}

case "${1:-}" in
  --print-plan)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    print_plan
    ;;
  --macos)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    run_macos
    ;;
  --windows)
    [ "$#" -eq 1 ] || { usage; exit 2; }
    run_windows
    ;;
  *)
    usage
    exit 2
    ;;
esac

