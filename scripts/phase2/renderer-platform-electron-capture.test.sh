#!/bin/sh
set -eu

scripts="$(CDPATH='' cd -- "$(dirname "$0")" && pwd)"
runner="$scripts/renderer-platform-electron-capture.sh"
package_dir="$scripts/renderer-platform-electron"
plan=$("$runner" --print-plan)
count=0

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}
expect_plan() {
  printf '%s\n' "$plan" | grep -F -q -- "$1" || fail "plan missing: $1"
  count=$((count + 1))
}
expect_in() {
  grep -F -q -- "$2" "$1" || fail "$1 missing: $2"
  count=$((count + 1))
}

expect_plan 'electron: 44.2.0, chromium 152.0.7977.76'
expect_plan 'host: frameless 1280x800 window, no application menu, scale factor 1'
expect_plan 'captures: original-desktop and original-repeat-desktop, each in a fresh Electron process'
expect_plan 'evidence/phase2/renderer-platform-electron-macos/'

# Electron and Playwright are pinned exactly, in the manifest and in the lockfile.
expect_in "$package_dir/package.json" '"electron": "44.2.0"'
expect_in "$package_dir/package.json" '"playwright-core": "1.58.2"'
expect_in "$package_dir/package-lock.json" '"node_modules/electron"'
python3 - "$package_dir/package-lock.json" <<'PY'
import json
import sys

packages = json.load(open(sys.argv[1]))["packages"]
assert packages["node_modules/electron"]["version"] == "44.2.0"
assert packages["node_modules/playwright-core"]["version"] == "1.58.2"
PY
count=$((count + 2))

# Port 6767 is never used, processes are stopped by exact PID or exact session name.
expect_in "$runner" 'port allocator selected forbidden port 6767'
expect_in "$runner" 'kill "$host_pid"'
expect_in "$runner" 'tmux -L "$tmux_socket" kill-session -t "$session"'
expect_in "$scripts/renderer-platform-electron-host.cjs" 'port === "6767"'
expect_in "$scripts/renderer-platform-electron-host.cjs" 'Menu.setApplicationMenu(null)'
expect_in "$scripts/renderer-platform-cdp-capture.cjs" 'refusing port 6767'

for script in "$scripts/renderer-platform-electron-host.cjs" "$scripts/renderer-platform-cdp-capture.cjs"; do
  node --check "$script"
  count=$((count + 1))
done
sh -n "$runner"
count=$((count + 1))

if rg -n '\x{2014}' "$scripts"/renderer-platform-electron* "$scripts"/renderer-platform-cdp-capture.cjs; then
  fail 'renderer Electron scripts contain forbidden em dash'
fi
count=$((count + 1))

printf '%s renderer Electron capture script assertions passed\n' "$count"
