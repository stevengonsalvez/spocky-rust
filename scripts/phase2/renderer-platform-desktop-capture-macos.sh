#!/bin/sh
# Capture the ORIGINAL Paseo desktop app as shipped (packages/desktop main process,
# preload, chrome mode, webviewTag, production web export served over paseo://app)
# on macOS. Isolation follows the reference packaged-app smoke test: a disposable
# PASEO_HOME, a random non-6767 daemon port, a disposable Electron userData
# directory, and a loopback DevTools port.
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
expected_reference=5de45e208690b0efc51c59a585ae9729325a9204
expected_reference_lock_sha=844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6
expected_electron=44.2.0
expected_chrome=152.0.7977.76
scripts="$repository_root/scripts/phase2"
evidence_dir="$repository_root/evidence/phase2/renderer-platform-desktop-macos"
build_gate=/private/tmp/spocky-targets/build-gate.sh

if [ "${1:-}" = "--print-plan" ]; then
  printf 'electron: %s, chromium %s\n' "$expected_electron" "$expected_chrome"
  printf '%s\n' 'app: packages/desktop built as shipped (production web export, electron-builder unpacked and unsigned)'
  printf '%s\n' 'isolation: disposable PASEO_HOME, userData and HOME, daemon on a random port, never 6767'
  printf '%s\n' 'storage: no seeded storage, the app opens its own page'
  printf '%s\n' 'captures: original-desktop and original-repeat-desktop, each in a fresh app launch'
  printf '%s\n' 'measured: full-PNG SHA-256, focus walk, Plus and Add project activation, accessibility tree'
  printf '%s\n' 'evidence/phase2/renderer-platform-desktop-macos/'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-plan]\n' "$0" >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'macOS desktop capture must run on macOS' >&2
  exit 1
fi
for command in gtimeout python3 npm node curl shasum git lsof; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf '%s is required for the desktop capture\n' "$command" >&2
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
refuse_6767() {
  if [ "$1" = 6767 ]; then
    printf '%s\n' 'port allocator selected forbidden port 6767' >&2
    exit 1
  fi
}

work=${SPOCKY_ELECTRON_CAPTURE_WORK:-/private/tmp/spocky-targets/electron-capture-work}
case "$work" in
  /private/tmp/spocky-targets/*) ;;
  *) printf 'work directory must be under /private/tmp/spocky-targets: %s\n' "$work" >&2; exit 1 ;;
esac
prepared="$work/.prepared-$expected_reference"
desktop_built="$work/.desktop-built-$expected_reference"
app_pid=
daemon_port=
stop_app() {
  # Exact PID for the app, exact port for the daemon the app started.
  if [ -n "$app_pid" ] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  app_pid=
  if [ -n "$daemon_port" ]; then
    for pid in $(lsof -ti "tcp:$daemon_port" -sTCP:LISTEN 2>/dev/null || true); do
      kill "$pid" 2>/dev/null || true
    done
  fi
  daemon_port=
}
cleanup() {
  status=$?
  stop_app
  if [ "$status" -ne 0 ]; then
    mkdir -p "$evidence_dir/failed-run"
    cp "$work"/desktop-*.log "$evidence_dir/failed-run/" 2>/dev/null || true
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

mkdir -p "$evidence_dir" "$work/electron"
if [ ! -f "$prepared" ]; then
  printf '%s\n' 'run scripts/phase2/renderer-platform-electron-capture.sh first to prepare the reference' >&2
  exit 1
fi
reference_lock_sha=$(shasum -a 256 "$work/reference/package-lock.json" | awk '{print $1}')
if [ "$reference_lock_sha" != "$expected_reference_lock_sha" ]; then
  printf 'reference package-lock hash is outside the pinned contract: %s\n' "$reference_lock_sha" >&2
  exit 1
fi
cp "$scripts/renderer-platform-electron/package.json" "$scripts/renderer-platform-electron/package-lock.json" \
  "$scripts/renderer-platform-cdp-capture.cjs" "$work/electron/"
$gate gtimeout 900 npm ci --prefix "$work/electron" --no-audit --no-fund >"$work/desktop-driver-npm-ci.log" 2>&1

# Build the desktop app as the reference ships it (root build:desktop), restricted to
# an unpacked, unsigned directory so no notarization or publishing is attempted.
# Each stage has its own marker so a timeout under load resumes where it stopped.
stage() {
  marker="$work/.desktop-$1-$expected_reference"
  if [ ! -f "$marker" ]; then
    shift
    "$@"
    : >"$marker"
  fi
}
build_deps() {
  (cd "$work/reference" && PATH="$work/reference/node_modules/.bin:$PATH" \
    $gate gtimeout 3600 npm run build:app-deps:clean) >"$work/desktop-build-deps.log" 2>&1
}
export_web() {
  (cd "$work/reference/packages/app" && PATH="$work/reference/node_modules/.bin:$PATH" \
    PASEO_WEB_PLATFORM=electron $gate gtimeout 7200 npx expo export --platform web) >"$work/desktop-build-export.log" 2>&1
}
package_app() {
  (cd "$work/reference" && PATH="$work/reference/node_modules/.bin:$PATH" \
    $gate gtimeout 7200 npm run build --workspace=@getpaseo/desktop -- \
    --dir --publish never -c.mac.notarize=false -c.mac.identity=null) >"$work/desktop-build-package.log" 2>&1
}
stage deps build_deps
stage export export_web
stage package package_app
app=$(find "$work/reference/packages/desktop/release" -maxdepth 2 -name Paseo.app -type d | head -n 1)
if [ -z "$app" ]; then
  printf '%s\n' 'packaged Paseo.app not found after the desktop build' >&2
  exit 1
fi

capture() {
  name=$1
  home=$(mktemp -d "$work/desktop-home.XXXXXX")
  user_data="$home/electron-user-data"
  mkdir -p "$user_data"
  daemon_port=$(free_port)
  refuse_6767 "$daemon_port"
  cdp_port=$(free_port)
  refuse_6767 "$cdp_port"
  listen="127.0.0.1:$daemon_port"
  python3 - "$home/config.json" "$listen" <<'PY'
import json, sys
json.dump({"version": 1, "daemon": {"listen": sys.argv[2], "relay": {"enabled": False},
           "mcp": {"enabled": False, "injectIntoAgents": False}}}, open(sys.argv[1], "w"), indent=2)
PY
  # Same isolation as packages/desktop/e2e/packaged-app-smoke.js createIsolatedDesktopEnv,
  # plus --lang=en-US so the locale is a gate condition, not the machine's setting.
  env -i PATH="$PATH" HOME="$home" USERPROFILE="$home" TMPDIR="${TMPDIR:-/private/tmp}" \
    PASEO_HOME="$home" PASEO_LISTEN="$listen" PASEO_ELECTRON_USER_DATA_DIR="$user_data" \
    PASEO_DISABLE_SINGLE_INSTANCE_LOCK=1 \
    PASEO_ELECTRON_FLAGS="--remote-debugging-address=127.0.0.1 --remote-debugging-port=$cdp_port --lang=en-US" \
    "$app/Contents/MacOS/Paseo" >"$work/desktop-app-$name.log" 2>&1 &
  app_pid=$!
  count=0
  until curl --silent --fail --max-time 2 "http://127.0.0.1:$cdp_port/json/version" >/dev/null 2>&1; do
    count=$((count + 1))
    if [ "$count" -ge 180 ]; then
      printf 'Electron DevTools did not become ready for %s\n' "$name" >&2
      return 1
    fi
    sleep 1
  done
  NODE_PATH="$work/electron/node_modules" gtimeout 600 node "$work/electron/renderer-platform-cdp-capture.cjs" \
    "$cdp_port" - "$evidence_dir" "$name" desktop
  stop_app
}
capture original-desktop
capture original-repeat-desktop

SPOCKY_INPUTS_OUT="$evidence_dir/inputs.json" SPOCKY_APP_ASAR="$app/Contents/Resources/app.asar" \
  SPOCKY_WEB_EXPORT="$work/reference/packages/app/dist" SPOCKY_HOSTB="$work/electron" \
  REFERENCE_COMMIT="$expected_reference" node "$scripts/renderer-platform-inputs.cjs"
python3 - "$evidence_dir" "$expected_reference" "$reference_lock_sha" "$expected_electron $expected_chrome" <<'PY'
import json, platform, subprocess, sys
evidence, reference, lock, probe = sys.argv[1:5]
observed = json.load(open(f"{evidence}/original-desktop.json"))["browser"]
inputs = json.load(open(f"{evidence}/inputs.json"))
assert inputs["shippedApp"]["appAsarSha256"], "built app asar not found"
assert observed["product"] == "Chrome/" + probe.split()[1], observed
assert "Electron/" + probe.split()[0] in observed["userAgent"], observed
def sh(*cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.strip()
json.dump({
    "reference": reference,
    "referencePackageLockSha256": lock,
    "expectedElectronAndChromium": probe,
    "observedBrowser": observed,
    "app": "packages/desktop built as shipped, unpacked and unsigned",
    "os": {"name": sh("sw_vers", "-productName"), "version": sh("sw_vers", "-productVersion"),
           "build": sh("sw_vers", "-buildVersion"), "kernel": platform.release(), "arch": platform.machine()},
    "builtApp": {k: inputs["shippedApp"][k] for k in ("appAsarSha256", "webExport")},
    "normalization": "none",
}, open(f"{evidence}/environment.json", "w"), indent=2)
open(f"{evidence}/environment.json", "a").write("\n")
PY
printf 'Desktop macOS baseline evidence: %s\n' "$evidence_dir"
find "$evidence_dir" -maxdepth 1 -type f -exec shasum -a 256 {} +
