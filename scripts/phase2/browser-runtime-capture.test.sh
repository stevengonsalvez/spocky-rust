#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
capture="$repository_root/scripts/phase2/browser-runtime-capture.sh"

parsed_rmse=$($capture --parse-rmse '5.65429 (8.62789e-05)')
if [ "$parsed_rmse" != "8.62789e-05" ]; then
  printf 'scientific normalized RMSE parsed as %s\n' "$parsed_rmse" >&2
  exit 1
fi

preflight=$($capture --preflight-only)
printf '%s\n' "$preflight" | grep -F \
  'Paseo baseline preflight passed: 5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$preflight" | grep -F 'port 6767 excluded'

plan=$($capture --print-plan)
printf '%s\n' "$plan" | grep -F 'original desktop 1280x800'
printf '%s\n' "$plan" | grep -F 'original mobile 390x844'
printf '%s\n' "$plan" | grep -F 'original repeat desktop and mobile stability captures'
printf '%s\n' "$plan" | grep -F 'candidate desktop 1280x800'
printf '%s\n' "$plan" | grep -F 'candidate mobile 390x844'
printf '%s\n' "$plan" | grep -F 'exact-pixel threshold: normalized RMSE 0'
printf '%s\n' "$plan" | grep -F 'stable product-state readiness before interaction and screenshot'
printf '%s\n' "$plan" | grep -F 'layout geometry and computed styles'
printf '%s\n' "$plan" | grep -F 'keyboard focus order and activation'
printf '%s\n' "$plan" | grep -F 'prefers-reduced-motion: reduce'
printf '%s\n' "$plan" | grep -F 'online reload and offline reload'
printf '%s\n' "$plan" | grep -F 'guest startup and browser runtime boundary'
printf '%s\n' "$plan" | grep -F 'isolated pinned daemon on a random non-6767 port'
printf '%s\n' "$plan" | grep -F 'exact named tmux sessions with bounded waits'
printf '%s\n' "$plan" | grep -F 'evidence/raw/phase2/browser-runtime-comparison.json'

branded_plan=$(
  SPOCKY_BROWSER_EVIDENCE_STEM=spocky-brand-runtime \
    "$capture" --print-plan
)
printf '%s\n' "$branded_plan" \
  | grep -F 'evidence/raw/phase2/spocky-brand-runtime-comparison.json'
if SPOCKY_BROWSER_EVIDENCE_STEM='../invalid' \
  "$capture" --print-plan >/dev/null 2>&1; then
  printf 'invalid branded evidence stem unexpectedly passed\n' >&2
  exit 1
fi

fixture_dir=$(mktemp -d /private/tmp/spocky-browser-validation.XXXXXX)
cleanup() {
  case "$fixture_dir" in
    /private/tmp/spocky-browser-validation.*) rm -rf "$fixture_dir" ;;
    *) printf 'refusing to remove unexpected fixture directory: %s\n' "$fixture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM
fixture="$fixture_dir/incomparable.json"
printf '%s\n' '{"captures":[{"name":"original","guestStartup":{"visibleText":""}},{"name":"candidate","guestStartup":{"visibleText":"ready"}}]}' >"$fixture"
set +e
node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" --validate-result "$fixture" >/dev/null
validation_status=$?
set -e
if [ "$validation_status" -ne 2 ]; then
  printf 'incomparable browser capture returned %s instead of 2\n' "$validation_status" >&2
  exit 1
fi

valid_fixture="$fixture_dir/valid.json"
printf '%s\n' '{
  "captures": [
    {"name":"original-desktop","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":[{"tag":"button","label":"Add a project","text":"Add a project"}],"keyboardActivation":{"attempted":true,"changed":true},"offlineReload":{"loaded":false}},
    {"name":"original-mobile","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":[{"tag":"button","label":"Add a project","text":"Add a project"}],"keyboardActivation":{"attempted":true,"changed":true},"offlineReload":{"loaded":false}},
    {"name":"candidate-desktop","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":[{"tag":"button","label":"Add a project","text":"Add a project"}],"keyboardActivation":{"attempted":true,"changed":true},"offlineReload":{"loaded":false}},
    {"name":"candidate-mobile","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":[{"tag":"button","label":"Add a project","text":"Add a project"}],"keyboardActivation":{"attempted":true,"changed":true},"offlineReload":{"loaded":false}}
  ],
  "comparison": {"visual":{"desktop":{"rmse":0,"passes":true},"mobile":{"rmse":0,"passes":true}}}
}' >"$valid_fixture"
valid_output=$(node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --validate-result "$valid_fixture")
printf '%s\n' "$valid_output" | grep -F '"accepted":true' >/dev/null
printf '%s\n' "$valid_output" | grep -F '"classification":"shared-pinned-failure"' >/dev/null

expect_rejected() {
  label=$1
  rejected_fixture=$2
  set +e
  "$capture" --enforce-result "$rejected_fixture" >/dev/null 2>&1
  rejected_status=$?
  set -e
  if [ "$rejected_status" -ne 2 ]; then
    printf '%s regression returned %s instead of 2\n' "$label" "$rejected_status" >&2
    exit 1
  fi
}

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.desktop = { rmse: 0.01, passes: false };
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/pixel-desktop.json"
expect_rejected 'desktop pixel' "$fixture_dir/pixel-desktop.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.mobile = { rmse: 0.01, passes: false };
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/pixel-mobile.json"
expect_rejected 'mobile pixel' "$fixture_dir/pixel-mobile.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-mobile")
    .keyboardActivation.changed = false;
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/interaction.json"
expect_rejected 'interaction' "$fixture_dir/interaction.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-desktop")
    .keyboardFocus[0].label = "Regressed label";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/accessibility.json"
expect_rejected 'accessibility' "$fixture_dir/accessibility.json"

grep -F 'page.routeWebSocket(/:(6767)' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'sidebar-project-empty-state' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'layoutGeometry' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'fontWeight: style.fontWeight' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'sidebarEmptyDetail' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'PASEO_DX_EXECUTABLE' \
  "$repository_root/scripts/phase2/browser-runtime-capture.sh" >/dev/null
