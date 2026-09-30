#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-"$repository_root/../paseo-rewrite"}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
raw_dir="$repository_root/evidence/raw/phase2"
result_file="$raw_dir/browser-runtime-comparison.json"
screenshot_dir="$raw_dir/browser-runtime-comparison"

parse_normalized_rmse() {
  metric=$1
  value=$(printf '%s\n' "$metric" | sed -n 's/.*(\([-+0-9.eE][^)]*\)).*/\1/p')
  if [ -z "$value" ]; then
    printf 'could not parse normalized RMSE: %s\n' "$metric" >&2
    return 1
  fi
  printf '%s\n' "$value"
}

if [ "${1:-}" = "--parse-rmse" ]; then
  if [ "$#" -ne 2 ]; then
    printf 'usage: %s --parse-rmse IMAGE_MAGICK_METRIC\n' "$0" >&2
    exit 2
  fi
  parse_normalized_rmse "$2"
  exit 0
fi

actual_baseline=$(git -C "$reference_root" rev-parse HEAD)
if [ "$actual_baseline" != "$expected_baseline" ]; then
  printf 'Paseo baseline HEAD mismatch: expected %s, got %s\n' \
    "$expected_baseline" "$actual_baseline" >&2
  exit 1
fi
if [ -n "$(git -C "$reference_root" status --porcelain --untracked-files=no)" ]; then
  printf 'Paseo baseline tracked tree is dirty: %s\n' "$reference_root" >&2
  exit 1
fi

if [ "${1:-}" = "--preflight-only" ]; then
  printf 'Paseo baseline preflight passed: %s\n' "$actual_baseline"
  printf 'safety boundary: disposable local runtimes only; port 6767 excluded\n'
  exit 0
fi

if [ "${1:-}" = "--print-plan" ]; then
  printf '%s\n' \
    'original desktop 1280x800' \
    'original mobile 390x844' \
    'candidate desktop 1280x800' \
    'candidate mobile 390x844' \
    'exact-pixel threshold: normalized RMSE 0' \
    'stable product-state readiness before interaction and screenshot' \
    'layout geometry and computed styles' \
    'keyboard focus order and activation' \
    'prefers-reduced-motion: reduce' \
    'online reload and offline reload' \
    'guest startup and browser runtime boundary' \
    'isolated pinned daemon on a random non-6767 port' \
    'exact named tmux sessions with bounded waits' \
    'evidence/raw/phase2/browser-runtime-comparison.json'
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--preflight-only|--print-plan|--parse-rmse IMAGE_MAGICK_METRIC]\n' "$0" >&2
  exit 2
fi

for command in gtimeout tmux python3 npm curl jq magick; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf '%s is required for bounded browser runtime capture\n' "$command" >&2
    exit 1
  fi
done

capture_dir=$(mktemp -d /private/tmp/paseo-browser-runtime.XXXXXX)
baseline_session="paseo-p2-browser-baseline-$$"
candidate_session="paseo-p2-browser-candidate-$$"
daemon_session="paseo-p2-browser-daemon-$$"
cleanup() {
  if tmux has-session -t "$baseline_session" 2>/dev/null; then
    tmux kill-session -t "$baseline_session"
  fi
  if tmux has-session -t "$candidate_session" 2>/dev/null; then
    tmux kill-session -t "$candidate_session"
  fi
  if tmux has-session -t "$daemon_session" 2>/dev/null; then
    tmux kill-session -t "$daemon_session"
  fi
  case "$capture_dir" in
    /private/tmp/paseo-browser-runtime.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing to remove unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

baseline_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
candidate_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
daemon_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
if [ "$baseline_port" = 6767 ] || [ "$candidate_port" = 6767 ] || [ "$daemon_port" = 6767 ]; then
  printf 'random port allocator selected forbidden port 6767\n' >&2
  exit 1
fi

mkdir -p "$raw_dir" "$screenshot_dir" "$capture_dir/reference"
git -C "$reference_root" archive "$actual_baseline" | tar -x -C "$capture_dir/reference"
cp "$repository_root/scripts/phase2/browser-runtime-capture.cjs" "$capture_dir/reference/browser-runtime-capture.cjs"

gtimeout 900 npm ci --prefix "$capture_dir/reference" --ignore-scripts --no-audit --no-fund \
  >"$raw_dir/browser-runtime-npm-ci.log" 2>&1
chromium_executable=${PASEO_CHROMIUM_EXECUTABLE:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}
if [ ! -x "$chromium_executable" ]; then
  gtimeout 300 "$capture_dir/reference/node_modules/.bin/playwright" install chromium \
    >"$raw_dir/browser-runtime-playwright-install.log" 2>&1
  chromium_executable=
fi
(
  cd "$capture_dir/reference"
  PATH="$capture_dir/reference/node_modules/.bin:$PATH" node scripts/postinstall-patches.mjs
  gtimeout 900 npm run build:server
  gtimeout 900 npm run build:app-deps
) >"$raw_dir/browser-runtime-build.log" 2>&1

(
  cd "$repository_root"
  gtimeout 300 cargo fetch --locked
  gtimeout 900 .tools/bin/dx build --web -p paseo-ui-renderer-pilot \
    --bin paseo-ui-web --no-default-features --features web \
    --bundle web --release --frozen
) >"$raw_dir/browser-runtime-candidate-build.log" 2>&1

baseline_log="$raw_dir/browser-runtime-baseline-server.log"
candidate_log="$raw_dir/browser-runtime-candidate-server.log"
daemon_log="$raw_dir/browser-runtime-daemon.log"
mkdir -p "$capture_dir/daemon-home"
tmux new-session -d -s "$daemon_session" -n server
tmux send-keys -t "$daemon_session:server" \
  "cd '$capture_dir/reference/packages/server' && PASEO_HOME='$capture_dir/daemon-home' PASEO_SERVER_ID='browser-baseline-daemon' PASEO_LISTEN='127.0.0.1:$daemon_port' PASEO_CORS_ORIGINS='http://127.0.0.1:$baseline_port' PASEO_RELAY_ENABLED=0 PASEO_DICTATION_ENABLED=0 PASEO_VOICE_MODE_ENABLED=0 PASEO_DICTATION_STT_PROVIDER=openai PASEO_VOICE_TURN_DETECTION_PROVIDER=openai PASEO_VOICE_STT_PROVIDER=openai PASEO_VOICE_TTS_PROVIDER=openai PASEO_NODE_ENV=development NODE_ENV=development ../../node_modules/.bin/tsx scripts/supervisor-entrypoint.ts --dev 2>&1 | tee '$daemon_log'" C-m
tmux new-session -d -s "$baseline_session" -n server
tmux send-keys -t "$baseline_session:server" \
  "cd '$capture_dir/reference/packages/app' && BROWSER=none ../../node_modules/.bin/expo start --web --port '$baseline_port' 2>&1 | tee '$baseline_log'" C-m
tmux new-session -d -s "$candidate_session" -n server
tmux send-keys -t "$candidate_session:server" \
  "cd '$repository_root/target/dx/paseo-ui-web/release/web/public' && python3 -m http.server '$candidate_port' --bind 127.0.0.1 2>&1 | tee '$candidate_log'" C-m

wait_for_url() {
  url=$1
  label=$2
  count=0
  until curl --silent --fail --max-time 2 "$url" >/dev/null 2>&1; do
    count=$((count + 1))
    if [ "$count" -ge 120 ]; then
      printf '%s did not become ready within 120 seconds: %s\n' "$label" "$url" >&2
      return 1
    fi
    sleep 1
  done
}
wait_for_url "http://127.0.0.1:$baseline_port/status" 'baseline Metro'
wait_for_url "http://127.0.0.1:$candidate_port/" 'candidate server'
wait_for_url "http://127.0.0.1:$daemon_port/api/health" 'isolated baseline daemon'
gtimeout 300 curl --silent --fail "http://127.0.0.1:$baseline_port/" >/dev/null

(
  cd "$capture_dir/reference"
  PASEO_CHROMIUM_EXECUTABLE="$chromium_executable" gtimeout 300 node browser-runtime-capture.cjs \
    "http://127.0.0.1:$baseline_port/" \
    "http://127.0.0.1:$candidate_port/" \
    "$result_file" \
    "$screenshot_dir" \
    "$daemon_port"
) >"$raw_dir/browser-runtime-capture.log" 2>&1

normalized_rmse() {
  original=$1
  candidate=$2
  set +e
  metric=$(magick compare -metric RMSE "$original" "$candidate" null: 2>&1)
  status=$?
  set -e
  if [ "$status" -gt 1 ]; then
    printf 'ImageMagick comparison failed with status %s: %s\n' "$status" "$metric" >&2
    return "$status"
  fi
  parse_normalized_rmse "$metric"
}

desktop_rmse=$(normalized_rmse \
  "$screenshot_dir/original-desktop.png" \
  "$screenshot_dir/candidate-desktop.png")
mobile_rmse=$(normalized_rmse \
  "$screenshot_dir/original-mobile.png" \
  "$screenshot_dir/candidate-mobile.png")
result_temp="$result_file.tmp"
jq \
  --arg desktop "$desktop_rmse" \
  --arg mobile "$mobile_rmse" \
  '.comparison.visual = {
    threshold: { metric: "normalized RMSE", maximum: 0 },
    desktop: { rmse: ($desktop | tonumber), passes: (($desktop | tonumber) == 0) },
    mobile: { rmse: ($mobile | tonumber), passes: (($mobile | tonumber) == 0) }
  }' "$result_file" >"$result_temp"
mv "$result_temp" "$result_file"

printf 'Browser runtime comparison captured: %s\n' "$result_file"
shasum -a 256 "$result_file" "$screenshot_dir"/*.png
