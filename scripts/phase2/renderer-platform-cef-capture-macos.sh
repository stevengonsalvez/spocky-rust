#!/bin/sh
# Host the Dioxus web bundle in the pinned CEF on macOS, capture it with the shared
# CDP driver, and compare it with a baseline capture (the shipped desktop app).
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
scripts="$repository_root/scripts/phase2"
cef_archive=cef_binary_152.0.7+g83ffcba+chromium-152.0.7977.83_macosx64_minimal.tar.bz2
cef_sha256=c4c07276991f64004201282bc2237c8679444b6a38788253f10e77d72911ddd5
expected_chrome=152.0.7977.83
cef_pins=${SPOCKY_CEF_PINS:-/private/tmp/spocky-targets/cef-pins}
host_work=/private/tmp/spocky-targets/cef-host-mac
bundle=${SPOCKY_DIOXUS_BUNDLE:-/private/tmp/spocky-targets/renderer-linux/dx/spocky-ui-web/release/web/public}
baseline_dir=${SPOCKY_CEF_BASELINE_DIR:-$repository_root/evidence/phase2/renderer-platform-desktop-macos}
evidence_dir=${SPOCKY_CEF_EVIDENCE_DIR:-$repository_root/evidence/phase2/renderer-platform-cef-macos}
electron_work=/private/tmp/spocky-targets/electron-capture-work

if [ "${1:-}" = "--print-plan" ]; then
  printf 'cef: 152.0.7+g83ffcba, chromium %s, macosx64 minimal, SHA-256 %s\n' "$expected_chrome" "$cef_sha256"
  printf '%s\n' 'host: frameless 1280x800 Views window, Alloy style, no browser chrome, DevTools port on loopback'
  printf '%s\n' 'bundle: Dioxus 0.7.0 web release served on a random loopback port, never 6767'
  printf '%s\n' 'storage: no seeded storage, the same as the desktop baseline'
  printf '%s\n' 'captures: the bundle in CEF (host A) and in Electron 44.2.0 (host B), twice each, fresh host per capture'
  printf '%s\n' 'compare: each host against the shipped desktop app, and CEF against Electron on the same bundle'
  printf '%s\n' 'measured: exact full-PNG SHA-256 membership, unmasked RMSE, focus walk, activation, accessibility tree'
  printf '%s\n' 'evidence/phase2/renderer-platform-cef-macos/'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'macOS CEF capture must run on macOS' >&2
  exit 1
fi
actual_sha=$(shasum -a 256 "$cef_pins/$cef_archive" | awk '{print $1}')
if [ "$actual_sha" != "$cef_sha256" ]; then
  printf 'CEF archive SHA-256 mismatch: expected %s, got %s\n' "$cef_sha256" "$actual_sha" >&2
  exit 1
fi
app="$host_work/build/spocky-cef-host.app"
for need in "$app/Contents/MacOS/spocky-cef-host" "$bundle/index.html" "$baseline_dir/original-desktop.json" \
  "$electron_work/electron/node_modules/playwright-core"; do
  if [ ! -e "$need" ]; then
    printf 'missing prerequisite: %s\n' "$need" >&2
    exit 1
  fi
done

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
refuse_6767() {
  if [ "$1" = 6767 ]; then
    printf '%s\n' 'port allocator selected forbidden port 6767' >&2
    exit 1
  fi
}

host_pid=
server_pid=
stop_host() {
  # Exact PID: ask politely, wait a bounded time, then force the same PID.
  if [ -n "$host_pid" ] && kill -0 "$host_pid" 2>/dev/null; then
    kill "$host_pid" 2>/dev/null || true
    count=0
    while kill -0 "$host_pid" 2>/dev/null && [ "$count" -lt 30 ]; do
      sleep 1
      count=$((count + 1))
    done
    if kill -0 "$host_pid" 2>/dev/null; then kill -9 "$host_pid" 2>/dev/null || true; fi
    wait "$host_pid" 2>/dev/null || true
  fi
  host_pid=
}
cleanup() {
  status=$?
  stop_host
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

mkdir -p "$evidence_dir"
http_port=$(free_port)
refuse_6767 "$http_port"
python3 -m http.server "$http_port" --bind 127.0.0.1 --directory "$bundle" >"$host_work/http-server.log" 2>&1 &
server_pid=$!
count=0
until curl --silent --fail --max-time 2 "http://127.0.0.1:$http_port/" >/dev/null 2>&1; do
  count=$((count + 1))
  if [ "$count" -ge 30 ]; then printf '%s\n' 'bundle server did not start' >&2; exit 1; fi
  sleep 1
done

capture() {
  name=$1
  cdp_port=$(free_port)
  refuse_6767 "$cdp_port"
  cache=$(mktemp -d "$host_work/cache.XXXXXX")
  "$app/Contents/MacOS/spocky-cef-host" --spocky-cdp-port="$cdp_port" --spocky-cache="$cache" \
    --spocky-bound-ms=600000 "--spocky-url=http://127.0.0.1:$http_port/" >"$host_work/host-$name.log" 2>&1 &
  host_pid=$!
  count=0
  until curl --silent --fail --max-time 2 "http://127.0.0.1:$cdp_port/json/version" >/dev/null 2>&1; do
    count=$((count + 1))
    if [ "$count" -ge 90 ]; then printf 'CEF DevTools did not become ready for %s\n' "$name" >&2; return 1; fi
    sleep 1
  done
  NODE_PATH="$electron_work/electron/node_modules" gtimeout 300 node "$scripts/renderer-platform-cdp-capture.cjs" \
    "$cdp_port" "http://127.0.0.1:$http_port/" "$evidence_dir" "$name" candidate
  stop_host
}
capture candidate-desktop
capture candidate-repeat-desktop

# Host B: the same bundle in the pinned Electron 44.2.0, to separate the engine from
# the pilot content. Fresh Electron process per capture, as for the original.
electron_bin="$electron_work/electron/node_modules/.bin/electron"
cp "$scripts/renderer-platform-electron-host.cjs" "$electron_work/electron/"
capture_electron() {
  name=$1
  cdp_port=$(free_port)
  refuse_6767 "$cdp_port"
  (cd "$electron_work/electron" && CDP_PORT=$cdp_port HOST_BOUND_MS=600000 "$electron_bin" renderer-platform-electron-host.cjs) \
    >"$host_work/electron-host-$name.log" 2>&1 &
  host_pid=$!
  count=0
  until curl --silent --fail --max-time 2 "http://127.0.0.1:$cdp_port/json/version" >/dev/null 2>&1; do
    count=$((count + 1))
    if [ "$count" -ge 90 ]; then printf 'Electron DevTools did not become ready for %s\n' "$name" >&2; return 1; fi
    sleep 1
  done
  NODE_PATH="$electron_work/electron/node_modules" gtimeout 300 node "$scripts/renderer-platform-cdp-capture.cjs" \
    "$cdp_port" "http://127.0.0.1:$http_port/" "$evidence_dir" "$name" candidate
  stop_host
}
capture_electron candidate-electron-desktop
capture_electron candidate-electron-repeat-desktop

observed=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["browser"]["product"])' "$evidence_dir/candidate-desktop.json")
if [ "$observed" != "Chrome/$expected_chrome" ]; then
  printf 'CEF Chromium mismatch: expected Chrome/%s, got %s\n' "$expected_chrome" "$observed" >&2
  exit 1
fi
python3 "$scripts/renderer-platform-runtime-compare.py" "$baseline_dir/original-desktop.json" \
  "$evidence_dir/candidate-desktop.json" "$evidence_dir/compare-first.json"
python3 "$scripts/renderer-platform-runtime-compare.py" "$baseline_dir/original-desktop.json" \
  "$evidence_dir/candidate-repeat-desktop.json" "$evidence_dir/compare-repeat.json"
python3 "$scripts/renderer-platform-runtime-compare.py" "$baseline_dir/original-desktop.json" \
  "$evidence_dir/candidate-electron-desktop.json" "$evidence_dir/compare-electron-first.json"
python3 "$scripts/renderer-platform-runtime-compare.py" "$evidence_dir/candidate-desktop.json" \
  "$evidence_dir/candidate-repeat-desktop.json" "$evidence_dir/candidate-stability.json"
# Engine equivalence: Electron 44.2.0 (baseline side) against CEF, same bundle.
python3 "$scripts/renderer-platform-runtime-compare.py" "$evidence_dir/candidate-electron-desktop.json" \
  "$evidence_dir/candidate-desktop.json" "$evidence_dir/engine-electron-vs-cef.json"
python3 "$scripts/renderer-platform-runtime-compare.py" "$evidence_dir/candidate-electron-desktop.json" \
  "$evidence_dir/candidate-electron-repeat-desktop.json" "$evidence_dir/electron-stability.json"
python3 "$scripts/renderer-platform-runtime-compare.py" "$baseline_dir/original-desktop.json" \
  "$baseline_dir/original-repeat-desktop.json" "$evidence_dir/original-stability.json"
# What produced this run: commit, OS, tool versions, CEF archive and host binary, bundle tree.
SPOCKY_INPUTS_OUT="$evidence_dir/inputs.json" SPOCKY_BUNDLE="$bundle" SPOCKY_CEF_HOST="$app/Contents/MacOS/spocky-cef-host" \
  SPOCKY_HOSTB="$electron_work/electron" CEF_ARCHIVE="$cef_archive" CEF_SHA256="$cef_sha256" \
  node "$scripts/renderer-platform-inputs.cjs"
printf 'CEF macOS evidence: %s\n' "$evidence_dir"
find "$evidence_dir" -maxdepth 1 -type f -exec shasum -a 256 {} +
