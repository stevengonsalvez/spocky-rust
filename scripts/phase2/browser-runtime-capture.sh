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
expected_chromium_executable='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
expected_chromium_version='Google Chrome 154.0.8037.59'
expected_font_family='system-ui, -apple-system, "system-ui", "Segoe UI", Roboto, Helvetica, Arial, sans-serif'
desktop_hash_a=fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f
desktop_hash_b=597095777e1d610387667c732b7c08624e4f135a6064e1b1b739ec1342f4dc7d
mobile_hash=37ff2c272ad311efe1fc2e22df94ecb75af3a5f74a47b2ee6c7b356e58d99075

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

parse_different_pixels() {
  metric=$1
  case "$metric" in
    ''|*[!0-9]*) ;;
    *)
      printf '%s\n' "$metric"
      return 0
      ;;
  esac
  if printf '%s\n' "$metric" \
    | grep -Eq '^[0-9]+ \(([0-9]+([.][0-9]+)?|[.][0-9]+)([eE][+-]?[0-9]+)?\)$'; then
    printf '%s\n' "${metric%% *}"
    return 0
  fi
  printf 'could not parse absolute pixel difference: %s\n' "$metric" >&2
  return 1
}

if [ "${1:-}" = "--parse-different-pixels" ]; then
  if [ "$#" -ne 2 ]; then
    printf 'usage: %s --parse-different-pixels IMAGE_MAGICK_METRIC\n' "$0" >&2
    exit 2
  fi
  parse_different_pixels "$2"
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
    'original repeat desktop and mobile rejected-mode evidence' \
    'candidate desktop 1280x800' \
    'candidate mobile 390x844' \
    'candidate consecutive same-page and fresh-context stability captures' \
    'exact full-PNG hash contract for two pinned desktop modes and one mobile mode' \
    'direct pixel evidence with zero normalization, masking, or threshold tolerance' \
    'stable product-state readiness before interaction and screenshot' \
    'layout geometry and computed styles' \
    'complete keyboard focus cycle and activation dialog outcome' \
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
  printf 'usage: %s [--preflight-only|--print-plan|--parse-different-pixels IMAGE_MAGICK_METRIC|--enforce-result RESULT_JSON|--evidence-paths ATTEMPT_ID]\n' "$0" >&2
  exit 2
fi
if [ -n "$(git -C "$repository_root" status --porcelain --untracked-files=no)" ]; then
  printf 'candidate and harness tracked tree is dirty: %s\n' "$repository_root" >&2
  exit 1
fi

for command in gtimeout tmux python3 npm curl jq magick shasum sw_vers uname; do
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
chromium_executable=${PASEO_CHROMIUM_EXECUTABLE:-"$expected_chromium_executable"}
if [ "$chromium_executable" != "$expected_chromium_executable" ]; then
  printf 'Chromium executable is outside the pinned visual contract: %s\n' \
    "$chromium_executable" >&2
  exit 1
fi
if [ ! -x "$chromium_executable" ]; then
  printf 'Pinned Chromium executable is unavailable: %s\n' "$chromium_executable" >&2
  exit 1
fi
chromium_version=$(gtimeout 10 "$chromium_executable" --version)
if [ "$chromium_version" != "$expected_chromium_version" ]; then
  printf 'Chromium version mismatch: expected %s, got %s\n' \
    "$expected_chromium_version" "$chromium_version" >&2
  exit 1
fi
os_name=$(sw_vers -productName)
os_version=$(sw_vers -productVersion)
os_build=$(sw_vers -buildVersion)
kernel_version=$(uname -r)
architecture=$(uname -m)
if [ "$os_name" != macOS ] || [ "$os_version" != 15.7.3 ] || \
  [ "$os_build" != 24G419 ] || [ "$kernel_version" != 24.6.0 ] || \
  [ "$architecture" != x86_64 ]; then
  printf 'OS is outside pinned visual contract: %s %s %s Darwin %s %s\n' \
    "$os_name" "$os_version" "$os_build" "$kernel_version" "$architecture" >&2
  exit 1
fi
baseline_lock_sha=$(shasum -a 256 "$capture_dir/reference/package-lock.json" | awk '{print $1}')
candidate_lock_sha=$(shasum -a 256 "$repository_root/Cargo.lock" | awk '{print $1}')
if [ "$baseline_lock_sha" != 844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6 ] || \
  [ "$candidate_lock_sha" != b1528e012f06833312ce6dd6ab206cb1db28569159c71a1fe71ac844137ba4b7 ]; then
  printf 'dependency lock hash is outside pinned visual contract\n' >&2
  exit 1
fi
candidate_commit=$(git -C "$repository_root" rev-parse HEAD)
harness_commit=$candidate_commit
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

different_pixels() {
  original=$1
  candidate=$2
  set +e
  metric=$(gtimeout 30 magick compare -metric AE "$original" "$candidate" null: 2>&1)
  status=$?
  set -e
  if [ "$status" -gt 1 ]; then
    printf 'ImageMagick comparison failed with status %s: %s\n' "$status" "$metric" >&2
    return "$status"
  fi
  parse_different_pixels "$metric"
}

desktop_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/original-desktop.png" \
  "$attempt_screenshot_dir/candidate-desktop.png")
mobile_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/original-mobile.png" \
  "$attempt_screenshot_dir/candidate-mobile.png")
original_desktop_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/original-desktop.png" \
  "$attempt_screenshot_dir/original-repeat-desktop.png")
original_mobile_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/original-mobile.png" \
  "$attempt_screenshot_dir/original-repeat-mobile.png")
candidate_same_page_desktop_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/candidate-desktop.png" \
  "$attempt_screenshot_dir/candidate-same-page-desktop.png")
candidate_same_page_mobile_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/candidate-mobile.png" \
  "$attempt_screenshot_dir/candidate-same-page-mobile.png")
candidate_fresh_desktop_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/candidate-desktop.png" \
  "$attempt_screenshot_dir/candidate-fresh-desktop.png")
candidate_fresh_mobile_different_pixels=$(different_pixels \
  "$attempt_screenshot_dir/candidate-mobile.png" \
  "$attempt_screenshot_dir/candidate-fresh-mobile.png")
if ! jq -e --arg family "$expected_font_family" '
  all(.captures[];
    any(.instrumentation.timeline[]?;
      .event == "fonts:ready" and
      .observed == true and
      .probe.fonts.status == "loaded" and
      .probe.fonts.pending == false
    ) and
    .layoutGeometry.sidebarEmpty.style.fontFamily == $family
  )
' "$attempt_result_file" >/dev/null; then
  printf 'captured fonts are outside the pinned visual contract\n' >&2
  exit 1
fi
sha256_file() {
  shasum -a 256 "$1" | awk '{print $1}'
}
original_desktop_sha=$(sha256_file "$attempt_screenshot_dir/original-desktop.png")
original_repeat_desktop_sha=$(sha256_file "$attempt_screenshot_dir/original-repeat-desktop.png")
original_mobile_sha=$(sha256_file "$attempt_screenshot_dir/original-mobile.png")
original_repeat_mobile_sha=$(sha256_file "$attempt_screenshot_dir/original-repeat-mobile.png")
candidate_desktop_sha=$(sha256_file "$attempt_screenshot_dir/candidate-desktop.png")
candidate_same_page_desktop_sha=$(sha256_file "$attempt_screenshot_dir/candidate-same-page-desktop.png")
candidate_fresh_desktop_sha=$(sha256_file "$attempt_screenshot_dir/candidate-fresh-desktop.png")
candidate_mobile_sha=$(sha256_file "$attempt_screenshot_dir/candidate-mobile.png")
candidate_same_page_mobile_sha=$(sha256_file "$attempt_screenshot_dir/candidate-same-page-mobile.png")
candidate_fresh_mobile_sha=$(sha256_file "$attempt_screenshot_dir/candidate-fresh-mobile.png")
result_temp="$attempt_result_file.tmp"
jq \
  --arg desktop "$desktop_different_pixels" \
  --arg mobile "$mobile_different_pixels" \
  --arg originalDesktop "$original_desktop_different_pixels" \
  --arg originalMobile "$original_mobile_different_pixels" \
  --arg candidateSamePageDesktop "$candidate_same_page_desktop_different_pixels" \
  --arg candidateSamePageMobile "$candidate_same_page_mobile_different_pixels" \
  --arg candidateFreshDesktop "$candidate_fresh_desktop_different_pixels" \
  --arg candidateFreshMobile "$candidate_fresh_mobile_different_pixels" \
  --arg chromiumExecutable "$chromium_executable" \
  --arg chromiumVersion "$chromium_version" \
  --arg osName "$os_name" \
  --arg osVersion "$os_version" \
  --arg osBuild "$os_build" \
  --arg kernelVersion "$kernel_version" \
  --arg architecture "$architecture" \
  --arg fontFamily "$expected_font_family" \
  --arg baselineLockSha "$baseline_lock_sha" \
  --arg candidateLockSha "$candidate_lock_sha" \
  --arg baselineCommit "$actual_baseline" \
  --arg candidateCommit "$candidate_commit" \
  --arg harnessCommit "$harness_commit" \
  --arg desktopHashA "$desktop_hash_a" \
  --arg desktopHashB "$desktop_hash_b" \
  --arg mobileHash "$mobile_hash" \
  --arg originalDesktopSha "$original_desktop_sha" \
  --arg originalRepeatDesktopSha "$original_repeat_desktop_sha" \
  --arg originalMobileSha "$original_mobile_sha" \
  --arg originalRepeatMobileSha "$original_repeat_mobile_sha" \
  --arg candidateDesktopSha "$candidate_desktop_sha" \
  --arg candidateSamePageDesktopSha "$candidate_same_page_desktop_sha" \
  --arg candidateFreshDesktopSha "$candidate_fresh_desktop_sha" \
  --arg candidateMobileSha "$candidate_mobile_sha" \
  --arg candidateSamePageMobileSha "$candidate_same_page_mobile_sha" \
  --arg candidateFreshMobileSha "$candidate_fresh_mobile_sha" \
  '.comparison.visual = {
    threshold: { metric: "different pixels", maximum: 0, normalization: "none" },
    desktop: { differentPixels: ($desktop | tonumber), passes: (($desktop | tonumber) == 0) },
    mobile: { differentPixels: ($mobile | tonumber), passes: (($mobile | tonumber) == 0) },
    originalStability: {
      acceptanceGate: false,
      desktop: { differentPixels: ($originalDesktop | tonumber), passes: (($originalDesktop | tonumber) == 0) },
      mobile: { differentPixels: ($originalMobile | tonumber), passes: (($originalMobile | tonumber) == 0) }
    },
    candidateStability: {
      samePage: {
        desktop: { differentPixels: ($candidateSamePageDesktop | tonumber), passes: (($candidateSamePageDesktop | tonumber) == 0) },
        mobile: { differentPixels: ($candidateSamePageMobile | tonumber), passes: (($candidateSamePageMobile | tonumber) == 0) }
      },
      freshContext: {
        desktop: { differentPixels: ($candidateFreshDesktop | tonumber), passes: (($candidateFreshDesktop | tonumber) == 0) },
        mobile: { differentPixels: ($candidateFreshMobile | tonumber), passes: (($candidateFreshMobile | tonumber) == 0) }
      }
    },
    baselineDefect: {
      contract: "empty-project-chromium-v1",
      browser: {
        engine: "chromium",
        executable: $chromiumExecutable,
        version: $chromiumVersion
      },
      os: {
        name: $osName,
        version: $osVersion,
        build: $osBuild,
        kernel: ("Darwin " + $kernelVersion),
        arch: $architecture
      },
      rendering: {
        deviceScaleFactor: 1,
        fontsStatus: "loaded",
        fontFamily: $fontFamily,
        theme: "light",
        locale: "en-US"
      },
      dependencies: {
        baselinePackageLockSha256: $baselineLockSha,
        candidateCargoLockSha256: $candidateLockSha
      },
      source: {
        baselineCommit: $baselineCommit,
        candidateCommit: $candidateCommit,
        harnessCommit: $harnessCommit
      },
      acceptedCandidateSha256: {
        desktop: [$desktopHashA, $desktopHashB],
        mobile: $mobileHash
      },
      observedSha256: {
        original: {
          desktop: $originalDesktopSha,
          repeatDesktop: $originalRepeatDesktopSha,
          mobile: $originalMobileSha,
          repeatMobile: $originalRepeatMobileSha
        },
        candidate: {
          desktop: $candidateDesktopSha,
          samePageDesktop: $candidateSamePageDesktopSha,
          freshDesktop: $candidateFreshDesktopSha,
          mobile: $candidateMobileSha,
          samePageMobile: $candidateSamePageMobileSha,
          freshMobile: $candidateFreshMobileSha
        }
      }
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
