#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
raw_dir="$repository_root/evidence/raw/phase2"
log_file="$raw_dir/dioxus-windows-check.log"
tools_root="$repository_root/.tools"
xwin="$tools_root/bin/cargo-xwin"
xwin_cache="$tools_root/xwin-cache"
target_dir="$tools_root/windows-target"
xwin_version=0.23.1
windows_target=x86_64-pc-windows-msvc

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' "cargo-xwin: $xwin_version"
  printf '%s\n' "target: $windows_target"
  printf '%s\n' 'cache and target output: repository-local ignored paths'
  printf '%s\n' 'bound: 1200 seconds'
  printf '%s\n' 'qualification: compile check only, no Windows launch claim'
  printf '%s\n' 'evidence/raw/phase2/dioxus-windows-check.log'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if ! command -v gtimeout >/dev/null 2>&1; then
  printf 'gtimeout is required for bounded Windows qualification\n' >&2
  exit 1
fi

mkdir -p "$raw_dir" "$tools_root" "$xwin_cache" "$target_dir"
if [ ! -x "$xwin" ] || [ "$("$xwin" --version 2>/dev/null || true)" != "cargo-xwin $xwin_version" ]; then
  gtimeout 600 cargo install --root "$tools_root" --version "$xwin_version" --locked cargo-xwin
fi
if ! rustup target list --installed | grep -Fx "$windows_target" >/dev/null; then
  gtimeout 300 rustup target add "$windows_target"
fi

: >"$log_file"
{
  rustc --version
  cargo --version
  "$xwin" --version
  printf 'target: %s\n' "$windows_target"
  printf '%s\n' 'XWIN_CACHE_DIR: .tools/xwin-cache'
  printf '%s\n' 'CARGO_TARGET_DIR: .tools/windows-target'
} >>"$log_file" 2>&1

set +e
gtimeout 1200 env XWIN_CACHE_DIR="$xwin_cache" \
  "$xwin" xwin check --locked --target "$windows_target" \
  -p spocky-ui-renderer-pilot --bin spocky-ui-desktop \
  --no-default-features --features desktop --target-dir "$target_dir" \
  >>"$log_file" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
  printf 'Windows compile qualification failed with status %s; see %s\n' "$status" "$log_file" >&2
  exit "$status"
fi

printf '%s\n' WINDOWS_DESKTOP_CHECK_OK >>"$log_file"
printf 'Windows desktop compile qualification passed: %s\n' "$log_file"
shasum -a 256 "$log_file"
