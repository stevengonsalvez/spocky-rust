#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
capture="$repository_root/scripts/phase2/browser-runtime-capture.sh"

parsed_zero_pixels=$($capture --parse-different-pixels '0 (0)')
if [ "$parsed_zero_pixels" != "0" ]; then
  printf 'zero absolute pixel difference parsed as %s\n' "$parsed_zero_pixels" >&2
  exit 1
fi
parsed_different_pixels=$($capture --parse-different-pixels '19 (1.75781e-05)')
if [ "$parsed_different_pixels" != "19" ]; then
  printf 'nonzero absolute pixel difference parsed as %s\n' "$parsed_different_pixels" >&2
  exit 1
fi
if $capture --parse-different-pixels '19 (invalid)' >/dev/null 2>&1; then
  printf 'malformed absolute pixel difference unexpectedly parsed\n' >&2
  exit 1
fi

version_with_trailing_whitespace=$(printf ' \tGoogle Chrome 154.0.8037.59 \t\r\n ')
normalized_version=$($capture --validate-chromium-version "$version_with_trailing_whitespace")
if [ "$normalized_version" != 'Google Chrome 154.0.8037.59' ]; then
  printf 'Chromium version edge whitespace normalized as %s\n' "$normalized_version" >&2
  exit 1
fi
if $capture --validate-chromium-version \
  'Google  Chrome 154.0.8037.59' >/dev/null 2>&1; then
  printf 'Chromium version with internal extra space unexpectedly passed\n' >&2
  exit 1
fi
if $capture --validate-chromium-version \
  'Google Chrome 155.0.0.0' >/dev/null 2>&1; then
  printf 'changed Chromium version unexpectedly passed\n' >&2
  exit 1
fi

preflight=$($capture --preflight-only)
printf '%s\n' "$preflight" | grep -F \
  'Paseo baseline preflight passed: 5de45e208690b0efc51c59a585ae9729325a9204'
printf '%s\n' "$preflight" | grep -F 'port 6767 excluded'

plan=$($capture --print-plan)
printf '%s\n' "$plan" | grep -F 'original desktop 1280x800'
printf '%s\n' "$plan" | grep -F 'original mobile 390x844'
printf '%s\n' "$plan" | grep -F 'original repeat desktop and mobile rejected-mode evidence'
printf '%s\n' "$plan" | grep -F 'candidate desktop 1280x800'
printf '%s\n' "$plan" | grep -F 'candidate mobile 390x844'
printf '%s\n' "$plan" | grep -F 'candidate consecutive same-page and fresh-context stability captures'
printf '%s\n' "$plan" \
  | grep -F 'exact full-PNG hash contract for two pinned desktop modes and one mobile mode'
printf '%s\n' "$plan" \
  | grep -F 'direct pixel evidence with zero normalization, masking, or threshold tolerance'
printf '%s\n' "$plan" | grep -F 'stable product-state readiness before interaction and screenshot'
printf '%s\n' "$plan" | grep -F 'layout geometry and computed styles'
printf '%s\n' "$plan" | grep -F 'complete keyboard focus cycle and activation dialog outcome'
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

original_activation_selector=$(node \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --activation-selector original)
if [ "$original_activation_selector" != '[data-testid="open-project-submit"]' ]; then
  printf 'original activation selector unexpectedly depends on label: %s\n' \
    "$original_activation_selector" >&2
  exit 1
fi
candidate_activation_selector=$(node \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --activation-selector candidate)
if [ "$candidate_activation_selector" != '.action:nth-child(1)' ]; then
  printf 'candidate activation selector is not first semantic project action: %s\n' \
    "$candidate_activation_selector" >&2
  exit 1
fi

evidence_paths=$(
  SPOCKY_BROWSER_EVIDENCE_STEM=spocky-brand-runtime \
    "$capture" --evidence-paths contract-attempt
)
printf '%s\n' "$evidence_paths" | grep -F \
  'attempt-result=evidence/raw/phase2/spocky-brand-runtime-attempts/contract-attempt/comparison.json'
printf '%s\n' "$evidence_paths" | grep -F \
  'published-result=evidence/raw/phase2/spocky-brand-runtime-comparison.json'
printf '%s\n' "$evidence_paths" | grep -F \
  'publish-policy=accepted-attempt-only'
if "$capture" --evidence-paths '../invalid' >/dev/null 2>&1; then
  printf 'invalid browser attempt id unexpectedly passed\n' >&2
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
font_fixture="$fixture_dir/font-contract.json"
printf '%s\n' '{
  "captures": [
    {
      "name": "original-desktop",
      "instrumentation": {"timeline": [{"event": "fonts:ready", "observed": true, "probe": {"fonts": {"status": "loaded", "pending": false}}}]},
      "layoutGeometry": {"sidebarEmpty": {"style": {"fontFamily": "system-ui, -apple-system, \"system-ui\", \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif"}}}
    },
    {
      "name": "original-repeat-desktop",
      "instrumentation": {"timeline": [{"event": "fonts:ready", "observed": true, "probe": {"fonts": {"status": "loaded", "pending": false}}}]},
      "layoutGeometry": {
        "sidebarEmpty": null,
        "menu": {"style": {"fontFamily": "system-ui, -apple-system, \"system-ui\", \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif"}},
        "logo": {"style": {"fontFamily": "system-ui, -apple-system, \"system-ui\", \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif"}}
      }
    }
  ]
}' >"$font_fixture"
"$capture" --validate-font-contract "$font_fixture"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures[1].layoutGeometry.menu.style.fontFamily = "Times";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$font_fixture" "$fixture_dir/font-family-drift.json"
if "$capture" --validate-font-contract "$fixture_dir/font-family-drift.json" >/dev/null 2>&1; then
  printf 'font-family drift unexpectedly passed\n' >&2
  exit 1
fi

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures[1].instrumentation.timeline[0].observed = false;
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$font_fixture" "$fixture_dir/font-readiness.json"
if "$capture" --validate-font-contract "$fixture_dir/font-readiness.json" >/dev/null 2>&1; then
  printf 'missing font readiness unexpectedly passed\n' >&2
  exit 1
fi

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures[1].layoutGeometry = { sidebarEmpty: null };
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$font_fixture" "$fixture_dir/font-geometry-missing.json"
if "$capture" --validate-font-contract "$fixture_dir/font-geometry-missing.json" >/dev/null 2>&1; then
  printf 'missing font-bearing geometry unexpectedly passed\n' >&2
  exit 1
fi

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
    {"name":"original-desktop","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":{"entries":[{"tag":"button","role":"button","label":"Add a project","text":"Add a project","disabled":false}],"completed":true},"keyboardActivation":{"attempted":true,"changed":true,"before":{"dialogs":[],"status":[]},"after":{"dialogs":[{"role":"dialog","label":"Add project: method","text":"Add project isolated-baseline Search for directory","controls":[{"tag":"div","role":"button","label":null,"text":"Search for directory","disabled":false}]}],"status":[]}},"offlineReload":{"loaded":false},"instrumentation":{"timeline":[{"event":"navigation:start","monotonicMs":0}],"requests":{"completed":[],"failed":[]}}},
    {"name":"original-mobile","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":{"entries":[{"tag":"button","role":"button","label":"Add a project","text":"Add a project","disabled":false}],"completed":true},"keyboardActivation":{"attempted":true,"changed":true,"before":{"dialogs":[],"status":[]},"after":{"dialogs":[{"role":"dialog","label":"Add project: method","text":"Add project isolated-baseline Search for directory","controls":[{"tag":"div","role":"button","label":null,"text":"Search for directory","disabled":false}]}],"status":[]}},"offlineReload":{"loaded":false}},
    {"name":"candidate-desktop","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":{"entries":[{"tag":"button","role":"button","label":"Add a project","text":"Add a project","disabled":false}],"completed":true},"keyboardActivation":{"attempted":true,"changed":true,"before":{"dialogs":[],"status":[]},"after":{"dialogs":[{"role":"dialog","label":"Add project: method","text":"Add project isolated-baseline Search for directory","controls":[{"tag":"div","role":"button","label":null,"text":"Search for directory","disabled":false}]}],"status":[]}},"offlineReload":{"loaded":false}},
    {"name":"candidate-mobile","guestStartup":{"visibleText":"ready"},"reducedMotion":true,"keyboardFocus":{"entries":[{"tag":"button","role":"button","label":"Add a project","text":"Add a project","disabled":false}],"completed":true},"keyboardActivation":{"attempted":true,"changed":true,"before":{"dialogs":[],"status":[]},"after":{"dialogs":[{"role":"dialog","label":"Add project: method","text":"Add project isolated-baseline Search for directory","controls":[{"tag":"div","role":"button","label":null,"text":"Search for directory","disabled":false}]}],"status":[]}},"offlineReload":{"loaded":false}},
    {"name":"candidate-fresh-desktop","guestStartup":{"visibleText":"ready"}},
    {"name":"candidate-fresh-mobile","guestStartup":{"visibleText":"ready"}}
  ],
  "comparison": {"visual":{"threshold":{"metric":"different pixels","maximum":0,"normalization":"none"},"desktop":{"differentPixels":0,"passes":true},"mobile":{"differentPixels":0,"passes":true},"candidateStability":{"samePage":{"desktop":{"differentPixels":0,"passes":true},"mobile":{"differentPixels":0,"passes":true}},"freshContext":{"desktop":{"differentPixels":0,"passes":true},"mobile":{"differentPixels":0,"passes":true}}}}}
}' >"$valid_fixture"
node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const desktopB = "597095777e1d610387667c732b7c08624e4f135a6064e1b1b739ec1342f4dc7d";
  const mobile = "37ff2c272ad311efe1fc2e22df94ecb75af3a5f74a47b2ee6c7b356e58d99075";
  result.captures.push(
    { name: "original-repeat-desktop", guestStartup: { visibleText: "ready" } },
    { name: "original-repeat-mobile", guestStartup: { visibleText: "ready" } },
  );
  for (const capture of result.captures) {
    capture.viewport = capture.name.endsWith("desktop")
      ? { width: 1280, height: 800 }
      : { width: 390, height: 844 };
  }
  result.comparison.visual.desktop = { differentPixels: 19, passes: false };
  result.comparison.visual.baselineDefect = {
    contract: "empty-project-chromium-v1",
    browser: {
      engine: "chromium",
      executable: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      version: "Google Chrome 154.0.8037.59",
    },
    os: {
      name: "macOS",
      version: "15.7.3",
      build: "24G419",
      kernel: "Darwin 24.6.0",
      arch: "x86_64",
    },
    rendering: {
      deviceScaleFactor: 1,
      fontsStatus: "loaded",
      fontFamily: `system-ui, -apple-system, "system-ui", "Segoe UI", Roboto, Helvetica, Arial, sans-serif`,
      theme: "light",
      locale: "en-US",
    },
    dependencies: {
      baselinePackageLockSha256: "844e8e2e4d3af3407fa8b54534888a4bf6c155a91f4d7121ae64d8f995863cd6",
      candidateCargoLockSha256: "b1528e012f06833312ce6dd6ab206cb1db28569159c71a1fe71ac844137ba4b7",
    },
    source: {
      baselineCommit: "5de45e208690b0efc51c59a585ae9729325a9204",
      candidateCommit: "1111111111111111111111111111111111111111",
      harnessCommit: "1111111111111111111111111111111111111111",
    },
    acceptedCandidateSha256: {
      desktop: [
        "fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f",
        desktopB,
      ],
      mobile,
    },
    observedSha256: {
      original: {
        desktop: "fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f",
        repeatDesktop: "fad844b57077bcdbed0c93db7de03e5811243049ef7b6b284dbb2a8286a6480f",
        mobile,
        repeatMobile: mobile,
      },
      candidate: {
        desktop: desktopB,
        samePageDesktop: desktopB,
        freshDesktop: desktopB,
        mobile,
        samePageMobile: mobile,
        freshMobile: mobile,
      },
    },
  };
  fs.writeFileSync(process.argv[1], JSON.stringify(result));
' "$valid_fixture"
valid_output=$(node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --validate-result "$valid_fixture")
printf '%s\n' "$valid_output" | grep -F '"accepted":true' >/dev/null
printf '%s\n' "$valid_output" | grep -F '"classification":"shared-pinned-failure"' >/dev/null

blank_focus_fixture="$fixture_dir/blank-focus.json"
node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const blank = { tag: "div", role: null, label: null, text: "", disabled: false };
  for (const name of ["original-desktop", "candidate-desktop"]) {
    result.captures.find((capture) => capture.name === name)
      .keyboardFocus.entries.push(blank);
  }
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$blank_focus_fixture"
blank_focus_output=$(node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --validate-result "$blank_focus_fixture")
printf '%s\n' "$blank_focus_output" | grep -F '"accepted":true' >/dev/null

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
  result.comparison.visual.baselineDefect.observedSha256.candidate.desktop =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/third-desktop-hash.json"
expect_rejected 'third desktop hash' "$fixture_dir/third-desktop-hash.json"
node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const instrumentation = result.captures.find(
    (capture) => capture.name === "original-desktop",
  ).instrumentation;
  if (instrumentation.timeline[0].event !== "navigation:start") process.exit(1);
' "$fixture_dir/third-desktop-hash.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  const contract = result.comparison.visual.baselineDefect;
  contract.observedSha256.candidate.desktop = contract.acceptedCandidateSha256.desktop[0];
  contract.observedSha256.candidate.samePageDesktop = contract.acceptedCandidateSha256.desktop[0];
  contract.observedSha256.candidate.freshDesktop = contract.acceptedCandidateSha256.desktop[0];
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/desktop-mode-a.json"
desktop_mode_a_output=$(node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" \
  --validate-result "$fixture_dir/desktop-mode-a.json")
printf '%s\n' "$desktop_mode_a_output" | grep -F '"accepted":true' >/dev/null

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.observedSha256.candidate.mobile =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/mobile-hash.json"
expect_rejected 'mobile hash' "$fixture_dir/mobile-hash.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.browser.version = "Google Chrome 155.0.0.0";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/browser-version.json"
expect_rejected 'browser version drift' "$fixture_dir/browser-version.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.dependencies.candidateCargoLockSha256 =
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/dependency-drift.json"
expect_rejected 'dependency drift' "$fixture_dir/dependency-drift.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.source.harnessCommit =
    "2222222222222222222222222222222222222222";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/harness-source-drift.json"
expect_rejected 'harness source drift' "$fixture_dir/harness-source-drift.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-desktop")
    .viewport.width = 1279;
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/viewport-drift.json"
expect_rejected 'viewport drift' "$fixture_dir/viewport-drift.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.acceptedCandidateSha256.desktop.push(
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  );
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/golden-expansion.json"
expect_rejected 'automatic golden expansion' "$fixture_dir/golden-expansion.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-mobile")
    .keyboardActivation.after.dialogs[0].text = "status-only substitution";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/interaction.json"
expect_rejected 'interaction' "$fixture_dir/interaction.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-desktop")
    .keyboardFocus.entries[0].label = "Regressed label";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/accessibility.json"
expect_rejected 'accessibility' "$fixture_dir/accessibility.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.captures.find((capture) => capture.name === "candidate-mobile")
    .keyboardFocus.completed = false;
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/incomplete-focus.json"
expect_rejected 'incomplete focus cycle' "$fixture_dir/incomplete-focus.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  delete result.captures.find((capture) => capture.name === "candidate-desktop")
    .keyboardFocus.entries[0].disabled;
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/malformed-focus.json"
expect_rejected 'malformed focus entry' "$fixture_dir/malformed-focus.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.observedSha256.candidate.samePageDesktop =
    result.comparison.visual.baselineDefect.acceptedCandidateSha256.desktop[0];
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/candidate-same-page-instability.json"
expect_rejected 'candidate same-page stability' "$fixture_dir/candidate-same-page-instability.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.baselineDefect.observedSha256.candidate.freshMobile =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/candidate-fresh-context-instability.json"
expect_rejected 'candidate fresh-context stability' "$fixture_dir/candidate-fresh-context-instability.json"

node -e '
  const fs = require("node:fs");
  const result = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  result.comparison.visual.desktop = { differentPixels: 1, passes: true };
  fs.writeFileSync(process.argv[2], JSON.stringify(result));
' "$valid_fixture" "$fixture_dir/visual-flag-bypass.json"
expect_rejected 'zero-pixel flag bypass' "$fixture_dir/visual-flag-bypass.json"

grep -F 'page.routeWebSocket(/:(6767)' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'deviceScaleFactor: 1' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'colorScheme: "light"' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'locale: "en-US"' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'gtimeout 30 magick compare -metric AE' \
  "$repository_root/scripts/phase2/browser-runtime-capture.sh" >/dev/null
grep -F 'sidebar-project-empty-state' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'page.locator(".action:nth-child(1)")' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'layoutGeometry' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'fontWeight: style.fontWeight' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
grep -F 'sidebarEmptyDetail' \
  "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
for checkpoint in \
  'navigation:start' \
  'meaningful-text:ready' \
  'fonts:ready' \
  'animation-frames:settled' \
  'keyboard-focus-scan:complete' \
  'activation:dispatch' \
  'activation:result' \
  'screenshot:complete' \
  'request:finished' \
  'request:failed' \
  'captureFailure'; do
  grep -F "$checkpoint" \
    "$repository_root/scripts/phase2/browser-runtime-capture.cjs" >/dev/null
done
grep -F 'PASEO_DX_EXECUTABLE' \
  "$repository_root/scripts/phase2/browser-runtime-capture.sh" >/dev/null
