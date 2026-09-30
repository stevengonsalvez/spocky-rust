#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-"$repository_root/../paseo-rewrite"}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
raw_dir="$repository_root/evidence/raw/phase2"
evidence_stem=${SPOCKY_BROWSER_EVIDENCE_STEM:-browser-runtime}
case "$evidence_stem" in
  *[!a-z0-9-]*|'')
    printf 'SPOCKY_BROWSER_EVIDENCE_STEM must contain lowercase letters, digits, or hyphens: %s\n' \
      "$evidence_stem" >&2
    exit 2
    ;;
esac
result_file="$raw_dir/$evidence_stem-comparison.json"
screenshot_dir="$raw_dir/$evidence_stem-comparison"
dx_executable=${PASEO_DX_EXECUTABLE:-"$repository_root/.tools/bin/dx"}

print_evidence_paths() {
  attempt_id=$1
  case "$attempt_id" in
    *[!A-Za-z0-9-]*|'')
      printf 'browser attempt id must contain letters, digits, or hyphens: %s\n' \
        "$attempt_id" >&2
      return 2
      ;;
  esac
  printf '%s\n' \
    "attempt-result=evidence/raw/phase2/$evidence_stem-attempts/$attempt_id/comparison.json" \
    "published-result=evidence/raw/phase2/$evidence_stem-comparison.json" \
    'publish-policy=accepted-attempt-only'
}

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

if [ "${1:-}" = "--enforce-result" ]; then
  if [ "$#" -ne 2 ]; then
    printf 'usage: %s --enforce-result RESULT_JSON\n' "$0" >&2
    exit 2
  fi
  exec node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
    --validate-result "$2"
fi

if [ "${1:-}" = "--evidence-paths" ]; then
  if [ "$#" -ne 2 ]; then
    printf 'usage: %s --evidence-paths ATTEMPT_ID\n' "$0" >&2
    exit 2
  fi
  print_evidence_paths "$2"
  exit $?
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
    'original repeat desktop and mobile stability captures' \
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
    'attempt-scoped evidence promoted only after acceptance' \
    "evidence/raw/phase2/$evidence_stem-comparison.json"
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--preflight-only|--print-plan|--parse-rmse IMAGE_MAGICK_METRIC|--enforce-result RESULT_JSON|--evidence-paths ATTEMPT_ID]\n' "$0" >&2
  exit 2
fi

for command in gtimeout tmux python3 npm curl jq magick; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf '%s is required for bounded browser runtime capture\n' "$command" >&2
    exit 1
  fi
done
if [ ! -x "$dx_executable" ]; then
  printf 'Dioxus CLI is not executable: %s\n' "$dx_executable" >&2
  exit 1
fi

capture_dir=$(mktemp -d /private/tmp/spocky-browser-runtime.XXXXXX)
attempt_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
attempt_dir="$raw_dir/$evidence_stem-attempts/$attempt_id"
attempt_result_file="$attempt_dir/comparison.json"
attempt_screenshot_dir="$attempt_dir/comparison"
publish_result_temp="$raw_dir/.$evidence_stem-comparison.$$.json"
publish_screenshot_temp="$raw_dir/.$evidence_stem-comparison.$$"
attempt_status=failed
baseline_session="spocky-p2-browser-baseline-$$"
candidate_session="spocky-p2-browser-candidate-$$"
daemon_session="spocky-p2-browser-daemon-$$"
mkdir -p "$raw_dir" "$attempt_screenshot_dir"
cleanup() {
  cleanup_status=$?
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
    /private/tmp/spocky-browser-runtime.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing to remove unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
  case "$publish_result_temp" in
    "$raw_dir"/.*-comparison.*.json) rm -f "$publish_result_temp" ;;
    *) printf 'refusing to remove unexpected result staging path: %s\n' "$publish_result_temp" >&2 ;;
  esac
  case "$publish_screenshot_temp" in
    "$raw_dir"/.*-comparison.*) rm -rf "$publish_screenshot_temp" ;;
    *) printf 'refusing to remove unexpected screenshot staging path: %s\n' "$publish_screenshot_temp" >&2 ;;
  esac
  printf '{"attemptId":"%s","evidenceStem":"%s","status":"%s","exitStatus":%s}\n' \
    "$attempt_id" "$evidence_stem" "$attempt_status" "$cleanup_status" \
    >"$attempt_dir/attempt.json.tmp"
  mv "$attempt_dir/attempt.json.tmp" "$attempt_dir/attempt.json"
}
trap cleanup EXIT HUP INT TERM

baseline_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
candidate_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
daemon_port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
if [ "$baseline_port" = 6767 ] || [ "$candidate_port" = 6767 ] || [ "$daemon_port" = 6767 ]; then
  printf 'random port allocator selected forbidden port 6767\n' >&2
  exit 1
fi

mkdir -p "$capture_dir/reference"
printf 'Browser runtime attempt evidence: %s\n' "$attempt_dir"
git -C "$reference_root" archive "$actual_baseline" | tar -x -C "$capture_dir/reference"
cp "$repository_root/scripts/phase2/browser-runtime-capture.cjs" "$capture_dir/reference/browser-runtime-capture.cjs"

gtimeout 900 npm ci --prefix "$capture_dir/reference" --ignore-scripts --no-audit --no-fund \
  >"$attempt_dir/npm-ci.log" 2>&1
chromium_executable=${PASEO_CHROMIUM_EXECUTABLE:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}
if [ ! -x "$chromium_executable" ]; then
  gtimeout 300 "$capture_dir/reference/node_modules/.bin/playwright" install chromium \
    >"$attempt_dir/playwright-install.log" 2>&1
  chromium_executable=
fi
(
  cd "$capture_dir/reference"
  PATH="$capture_dir/reference/node_modules/.bin:$PATH" node scripts/postinstall-patches.mjs
  gtimeout 900 npm run build:server
  gtimeout 900 npm run build:app-deps
) >"$attempt_dir/baseline-build.log" 2>&1

(
  cd "$repository_root"
  gtimeout 300 cargo fetch --locked
  gtimeout 900 "$dx_executable" build --web -p spocky-ui-renderer-pilot \
    --bin spocky-ui-web --no-default-features --features web \
    --bundle web --release --frozen
) >"$attempt_dir/candidate-build.log" 2>&1

baseline_log="$attempt_dir/baseline-server.log"
candidate_log="$attempt_dir/candidate-server.log"
daemon_log="$attempt_dir/daemon.log"
mkdir -p "$capture_dir/daemon-home"
tmux new-session -d -s "$daemon_session" -n server
tmux send-keys -t "$daemon_session:server" \
  "cd '$capture_dir/reference/packages/server' && PASEO_HOME='$capture_dir/daemon-home' PASEO_SERVER_ID='browser-baseline-daemon' PASEO_LISTEN='127.0.0.1:$daemon_port' PASEO_CORS_ORIGINS='http://127.0.0.1:$baseline_port' PASEO_RELAY_ENABLED=0 PASEO_DICTATION_ENABLED=0 PASEO_VOICE_MODE_ENABLED=0 PASEO_DICTATION_STT_PROVIDER=openai PASEO_VOICE_TURN_DETECTION_PROVIDER=openai PASEO_VOICE_STT_PROVIDER=openai PASEO_VOICE_TTS_PROVIDER=openai PASEO_NODE_ENV=development NODE_ENV=development ../../node_modules/.bin/tsx scripts/supervisor-entrypoint.ts --dev 2>&1 | tee '$daemon_log'" C-m
tmux new-session -d -s "$baseline_session" -n server
tmux send-keys -t "$baseline_session:server" \
  "cd '$capture_dir/reference/packages/app' && BROWSER=none ../../node_modules/.bin/expo start --web --port '$baseline_port' 2>&1 | tee '$baseline_log'" C-m
tmux new-session -d -s "$candidate_session" -n server
tmux send-keys -t "$candidate_session:server" \
  "cd '$repository_root/target/dx/spocky-ui-web/release/web/public' && python3 -m http.server '$candidate_port' --bind 127.0.0.1 2>&1 | tee '$candidate_log'" C-m

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
    "$attempt_result_file" \
    "$attempt_screenshot_dir" \
    "$daemon_port"
) >"$attempt_dir/capture.log" 2>&1

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
  "$attempt_screenshot_dir/original-desktop.png" \
  "$attempt_screenshot_dir/candidate-desktop.png")
mobile_rmse=$(normalized_rmse \
  "$attempt_screenshot_dir/original-mobile.png" \
  "$attempt_screenshot_dir/candidate-mobile.png")
original_desktop_rmse=$(normalized_rmse \
  "$attempt_screenshot_dir/original-desktop.png" \
  "$attempt_screenshot_dir/original-repeat-desktop.png")
original_mobile_rmse=$(normalized_rmse \
  "$attempt_screenshot_dir/original-mobile.png" \
  "$attempt_screenshot_dir/original-repeat-mobile.png")
result_temp="$attempt_result_file.tmp"
jq \
  --arg desktop "$desktop_rmse" \
  --arg mobile "$mobile_rmse" \
  --arg originalDesktop "$original_desktop_rmse" \
  --arg originalMobile "$original_mobile_rmse" \
  '.comparison.visual = {
    threshold: { metric: "normalized RMSE", maximum: 0 },
    desktop: { rmse: ($desktop | tonumber), passes: (($desktop | tonumber) == 0) },
    mobile: { rmse: ($mobile | tonumber), passes: (($mobile | tonumber) == 0) },
    originalStability: {
      desktop: { rmse: ($originalDesktop | tonumber), passes: (($originalDesktop | tonumber) == 0) },
      mobile: { rmse: ($originalMobile | tonumber), passes: (($originalMobile | tonumber) == 0) }
    }
  }' "$attempt_result_file" >"$result_temp"
mv "$result_temp" "$attempt_result_file"

set +e
comparison=$(node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --validate-result "$attempt_result_file")
acceptance_status=$?
set -e
jq --argjson comparison "$comparison" '.comparison = $comparison' \
  "$attempt_result_file" >"$result_temp"
mv "$result_temp" "$attempt_result_file"
if [ "$acceptance_status" -ne 0 ]; then
  printf 'Browser runtime comparison rejected by acceptance contract: %s\n' \
    "$attempt_result_file" >&2
  exit "$acceptance_status"
fi

cp -R "$attempt_screenshot_dir" "$publish_screenshot_temp"
cp "$attempt_result_file" "$publish_result_temp"
if [ -d "$screenshot_dir" ]; then
  mv "$screenshot_dir" "$attempt_dir/previous-published-comparison"
fi
if [ -f "$result_file" ]; then
  cp "$result_file" "$attempt_dir/previous-published-comparison.json"
fi
mv "$publish_screenshot_temp" "$screenshot_dir"
mv "$publish_result_temp" "$result_file"
attempt_status=accepted
printf 'Browser runtime comparison captured: %s\n' "$result_file"
shasum -a 256 "$result_file" "$screenshot_dir"/*.png
