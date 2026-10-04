#!/bin/sh
# Runs inside the Linux gate container (--network none). Captures the shipped app,
# host A (CEF) and host B (Electron) on one Xvfb display and compares them.
set -eu

scripts=/workspace/scripts/phase2
out=/output
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 DISPLAY=:99
export NODE_PATH=/ref/node_modules

Xvfb :99 -screen 0 1280x800x24 -dpi 96 -nolisten tcp >/tmp/xvfb.log 2>&1 &
xvfb_pid=$!
trap 'kill $xvfb_pid 2>/dev/null || true' EXIT
n=0
until xdpyinfo -display :99 >/dev/null 2>&1; do
  n=$((n + 1)); [ "$n" -lt 100 ] || { echo 'Xvfb did not start' >&2; exit 1; }
  sleep 0.1
done

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}
refuse_6767() { [ "$1" != 6767 ] || { echo 'forbidden port 6767' >&2; exit 1; }; }
wait_cdp() {
  n=0
  until curl -fsS -m 2 "http://127.0.0.1:$1/json/version" >/dev/null 2>&1; do
    n=$((n + 1)); [ "$n" -lt 120 ] || { echo "DevTools not ready on $1" >&2; return 1; }
    sleep 1
  done
}
# Each host runs in its own session so the whole tree is stopped by the exact group.
stop_group() {
  if [ -n "${group_pid:-}" ]; then
    kill -TERM -- "-$group_pid" 2>/dev/null || true
    n=0
    while kill -0 -- "-$group_pid" 2>/dev/null && [ "$n" -lt 30 ]; do sleep 1; n=$((n + 1)); done
    kill -KILL -- "-$group_pid" 2>/dev/null || true
    group_pid=
  fi
}

http_port=$(free_port); refuse_6767 "$http_port"
python3 -m http.server "$http_port" --bind 127.0.0.1 --directory /bundle >/tmp/http.log 2>&1 &
http_pid=$!
trap 'kill $http_pid $xvfb_pid 2>/dev/null || true' EXIT

shipped() {
  name=$1
  home=$(mktemp -d /tmp/paseo-home.XXXXXX)
  daemon_port=$(free_port); refuse_6767 "$daemon_port"
  cdp=$(free_port); refuse_6767 "$cdp"
  python3 - "$home/config.json" "127.0.0.1:$daemon_port" <<'PY'
import json, sys
json.dump({"version": 1, "daemon": {"listen": sys.argv[2], "relay": {"enabled": False},
           "mcp": {"enabled": False, "injectIntoAgents": False}}}, open(sys.argv[1], "w"), indent=2)
PY
  setsid env -i PATH="$PATH" HOME="$home" USERPROFILE="$home" DISPLAY=:99 LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
    PASEO_HOME="$home" PASEO_LISTEN="127.0.0.1:$daemon_port" PASEO_ELECTRON_USER_DATA_DIR="$home/user-data" \
    PASEO_DISABLE_SINGLE_INSTANCE_LOCK=1 \
    PASEO_ELECTRON_FLAGS="--no-sandbox --remote-debugging-address=127.0.0.1 --remote-debugging-port=$cdp --lang=en-US" \
    /ref/packages/desktop/release/linux-unpacked/Paseo >"/tmp/shipped-$name.log" 2>&1 &
  group_pid=$!
  wait_cdp "$cdp"
  timeout 600 node "$scripts/renderer-platform-cdp-capture.cjs" "$cdp" - "$out" "$name" desktop
  stop_group
}
host_a() {
  name=$1
  cdp=$(free_port); refuse_6767 "$cdp"
  cache=$(mktemp -d /tmp/cef-cache.XXXXXX)
  setsid /cefbuild/Release/spocky-cef-host --spocky-cdp-port="$cdp" --spocky-cache="$cache" --spocky-bound-ms=600000 \
    "--spocky-url=http://127.0.0.1:$http_port/" >"/tmp/cef-$name.log" 2>&1 &
  group_pid=$!
  wait_cdp "$cdp"
  timeout 300 node "$scripts/renderer-platform-cdp-capture.cjs" "$cdp" "http://127.0.0.1:$http_port/" "$out" "$name" candidate
  stop_group
}
host_b() {
  name=$1
  cdp=$(free_port); refuse_6767 "$cdp"
  cp "$scripts/renderer-platform-electron-host.cjs" /hostb/
  (cd /hostb && setsid env CDP_PORT="$cdp" HOST_BOUND_MS=600000 node_modules/.bin/electron --no-sandbox renderer-platform-electron-host.cjs) \
    >"/tmp/electron-$name.log" 2>&1 &
  group_pid=$!
  wait_cdp "$cdp"
  timeout 300 node "$scripts/renderer-platform-cdp-capture.cjs" "$cdp" "http://127.0.0.1:$http_port/" "$out" "$name" candidate
  stop_group
}

shipped original-desktop
shipped original-repeat-desktop
host_a candidate-desktop
host_a candidate-repeat-desktop
host_b candidate-electron-desktop
host_b candidate-electron-repeat-desktop

compare() { python3 "$scripts/renderer-platform-runtime-compare.py" "$out/$1.json" "$out/$2.json" "$out/$3.json"; }
compare original-desktop candidate-desktop compare-first
compare original-desktop candidate-repeat-desktop compare-repeat
compare original-desktop candidate-electron-desktop compare-electron-first
compare candidate-electron-desktop candidate-desktop engine-electron-vs-cef
compare original-desktop original-repeat-desktop original-stability
compare candidate-desktop candidate-repeat-desktop candidate-stability
compare candidate-electron-desktop candidate-electron-repeat-desktop electron-stability
echo RENDERER_LINUX_GATE_OK
