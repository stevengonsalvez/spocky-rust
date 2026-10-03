#!/bin/sh
# Capture the ORIGINAL Paseo empty-project screen inside the pinned Electron
# 44.2.0 on macOS, as the baseline the CEF host must match.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
expected_reference=5de45e208690b0efc51c59a585ae9729325a9204
expected_reference_lock_sha=844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6
expected_electron=44.2.0
expected_chrome=152.0.7977.76
scripts="$repository_root/scripts/phase2"
evidence_dir="$repository_root/evidence/phase2/renderer-platform-electron-macos"
build_gate=/private/tmp/spocky-targets/build-gate.sh
tmux_socket=spocky

if [ "${1:-}" = "--print-plan" ]; then
  printf 'electron: %s, chromium %s\n' "$expected_electron" "$expected_chrome"
  printf '%s\n' 'host: frameless 1280x800 window, no application menu, scale factor 1'
  printf '%s\n' 'original: pinned Paseo reference served by Metro with an isolated disposable daemon'
  printf '%s\n' 'captures: original-desktop and original-repeat-desktop, each in a fresh Electron process'
  printf '%s\n' 'measured: full-PNG SHA-256, focus walk, Plus and Add project activation, accessibility tree'
  printf '%s\n' 'evidence/phase2/renderer-platform-electron-macos/'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'macOS baseline capture must run on macOS' >&2
  exit 1
fi
for command in gtimeout tmux python3 npm node curl shasum git; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf '%s is required for the Electron baseline capture\n' "$command" >&2
    exit 1
  fi
done
actual_reference=$(git -C "$reference_root" rev-parse HEAD)
if [ "$actual_reference" != "$expected_reference" ]; then
  printf 'Paseo reference mismatch: expected %s, got %s\n' "$expected_reference" "$actual_reference" >&2
  exit 1
fi

gate=
if [ -x "$build_gate" ]; then gate=$build_gate; fi
free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
metro_port=$(free_port)
daemon_port=$(free_port)
for port in "$metro_port" "$daemon_port"; do
  if [ "$port" = 6767 ]; then
    printf '%s\n' 'port allocator selected forbidden port 6767' >&2
    exit 1
  fi
done

# Prepared once and reused (npm ci and the original builds are slow). It lives under
# spocky-targets, which Spotlight and the file watchers skip. Delete it by hand to
# force a fresh preparation.
work=${SPOCKY_ELECTRON_CAPTURE_WORK:-/private/tmp/spocky-targets/electron-capture-work}
case "$work" in
  /private/tmp/spocky-targets/*) ;;
  *) printf 'work directory must be under /private/tmp/spocky-targets: %s\n' "$work" >&2; exit 1 ;;
esac
prepared="$work/.prepared-$expected_reference"
daemon_session="spocky-p2-electron-daemon-$$"
metro_session="spocky-p2-electron-metro-$$"
host_pid=
stop_host() {
  # Exact PID only. The host also exits on its own after its bound.
  if [ -n "$host_pid" ] && kill -0 "$host_pid" 2>/dev/null; then
    kill "$host_pid" 2>/dev/null || true
    wait "$host_pid" 2>/dev/null || true
  fi
  host_pid=
}
cleanup() {
  status=$?
  stop_host
  for session in "$metro_session" "$daemon_session"; do
    if tmux -L "$tmux_socket" has-session -t "$session" 2>/dev/null; then
      tmux -L "$tmux_socket" kill-session -t "$session"
    fi
  done
  if [ "$status" -ne 0 ]; then
    # Keep the logs of a failed run next to the evidence for diagnosis.
    mkdir -p "$evidence_dir/failed-run"
    cp "$work"/*.log "$evidence_dir/failed-run/" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

mkdir -p "$evidence_dir" "$work/electron"
if [ ! -f "$prepared" ]; then
  mkdir -p "$work/reference"
  git -C "$reference_root" archive "$actual_reference" | tar -x -C "$work/reference"
fi
reference_lock_sha=$(shasum -a 256 "$work/reference/package-lock.json" | awk '{print $1}')
if [ "$reference_lock_sha" != "$expected_reference_lock_sha" ]; then
  printf 'reference package-lock hash is outside the pinned contract: %s\n' "$reference_lock_sha" >&2
  exit 1
fi

# Pinned Electron and Playwright, from the committed lockfile.
cp "$scripts/renderer-platform-electron/package.json" "$scripts/renderer-platform-electron/package-lock.json" \
  "$scripts/renderer-platform-electron-host.cjs" "$scripts/renderer-platform-cdp-capture.cjs" "$work/electron/"
$gate gtimeout 900 npm ci --prefix "$work/electron" --no-audit --no-fund >"$work/electron-npm-ci.log" 2>&1
electron_bin="$work/electron/node_modules/.bin/electron"
cat >"$work/electron/probe.cjs" <<'EOF'
const { app } = require("electron");
app.whenReady().then(() => {
  process.stdout.write(`${process.versions.electron} ${process.versions.chrome}\n`);
  app.quit();
});
EOF
probe=$(gtimeout 60 "$electron_bin" "$work/electron/probe.cjs" 2>/dev/null | tail -n 1)
if [ "$probe" != "$expected_electron $expected_chrome" ]; then
  printf 'Electron probe mismatch: expected "%s %s", got "%s"\n' "$expected_electron" "$expected_chrome" "$probe" >&2
  exit 1
fi

# Original app: isolated daemon plus the Metro web build, as the browser gate does.
if [ ! -f "$prepared" ]; then
  $gate gtimeout 900 npm ci --prefix "$work/reference" --ignore-scripts --no-audit --no-fund \
    >"$work/reference-npm-ci.log" 2>&1
  (
    cd "$work/reference"
    PATH="$work/reference/node_modules/.bin:$PATH" node scripts/postinstall-patches.mjs
    $gate gtimeout 900 npm run build:server
    $gate gtimeout 900 npm run build:app-deps
  ) >"$work/reference-build.log" 2>&1
  : >"$prepared"
fi
# A fresh disposable daemon home every run.
rm -rf "$work/daemon-home"
tmux -L "$tmux_socket" new-session -d -s "$daemon_session" -n server
tmux -L "$tmux_socket" send-keys -t "$daemon_session:server" \
  "cd '$work/reference/packages/server' && PASEO_HOME='$work/daemon-home' PASEO_SERVER_ID='browser-baseline-daemon' PASEO_LISTEN='127.0.0.1:$daemon_port' PASEO_CORS_ORIGINS='http://127.0.0.1:$metro_port' PASEO_RELAY_ENABLED=0 PASEO_DICTATION_ENABLED=0 PASEO_VOICE_MODE_ENABLED=0 PASEO_DICTATION_STT_PROVIDER=openai PASEO_VOICE_TURN_DETECTION_PROVIDER=openai PASEO_VOICE_STT_PROVIDER=openai PASEO_VOICE_TTS_PROVIDER=openai PASEO_NODE_ENV=development NODE_ENV=development ../../node_modules/.bin/tsx scripts/supervisor-entrypoint.ts --dev 2>&1 | tee '$work/daemon.log'" C-m
tmux -L "$tmux_socket" new-session -d -s "$metro_session" -n server
tmux -L "$tmux_socket" send-keys -t "$metro_session:server" \
  "cd '$work/reference/packages/app' && BROWSER=none ../../node_modules/.bin/expo start --web --port '$metro_port' 2>&1 | tee '$work/metro.log'" C-m

wait_for_url() {
  count=0
  until curl --silent --fail --max-time 2 "$1" >/dev/null 2>&1; do
    count=$((count + 1))
    if [ "$count" -ge 180 ]; then
      printf '%s did not become ready within 180 seconds: %s\n' "$2" "$1" >&2
      return 1
    fi
    sleep 1
  done
}
wait_for_url "http://127.0.0.1:$metro_port/status" 'Metro'
wait_for_url "http://127.0.0.1:$daemon_port/api/health" 'isolated daemon'
gtimeout 300 curl --silent --fail "http://127.0.0.1:$metro_port/" >/dev/null

capture() {
  name=$1
  cdp_port=$(free_port)
  if [ "$cdp_port" = 6767 ]; then
    printf '%s\n' 'port allocator selected forbidden port 6767' >&2
    exit 1
  fi
  CDP_PORT=$cdp_port HOST_BOUND_MS=600000 "$electron_bin" "$work/electron/renderer-platform-electron-host.cjs" \
    >"$work/host-$name.log" 2>&1 &
  host_pid=$!
  wait_for_url "http://127.0.0.1:$cdp_port/json/version" "Electron DevTools ($name)"
  NODE_PATH="$work/electron/node_modules" gtimeout 300 node "$work/electron/renderer-platform-cdp-capture.cjs" \
    "$cdp_port" "http://127.0.0.1:$metro_port/" "$evidence_dir" "$name" original "$daemon_port"
  stop_host
}
capture original-desktop
capture original-repeat-desktop

python3 - "$evidence_dir" "$expected_reference" "$reference_lock_sha" "$probe" <<'EOF'
import json, platform, subprocess, sys
evidence, reference, lock, probe = sys.argv[1:5]
def sh(*cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip()
json.dump({
    "reference": reference,
    "referencePackageLockSha256": lock,
    "electronAndChromium": probe,
    "os": {"name": sh("sw_vers", "-productName"), "version": sh("sw_vers", "-productVersion"),
           "build": sh("sw_vers", "-buildVersion"), "kernel": platform.release(), "arch": platform.machine()},
    "normalization": "none",
}, open(f"{evidence}/environment.json", "w"), indent=2)
open(f"{evidence}/environment.json", "a").write("\n")
EOF
printf 'Electron macOS baseline evidence: %s\n' "$evidence_dir"
find "$evidence_dir" -maxdepth 1 -type f -exec shasum -a 256 {} +
