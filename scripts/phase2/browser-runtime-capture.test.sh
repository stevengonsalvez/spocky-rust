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

fixture=$(mktemp /private/tmp/spocky-browser-validation.XXXXXX)
cleanup() {
  case "$fixture" in
    /private/tmp/spocky-browser-validation.*) rm -f "$fixture" ;;
    *) printf 'refusing to remove unexpected fixture: %s\n' "$fixture" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM
printf '%s\n' '{"captures":[{"name":"original","guestStartup":{"visibleText":""}},{"name":"candidate","guestStartup":{"visibleText":"ready"}}]}' >"$fixture"
set +e
node "$repository_root/scripts/phase2/browser-runtime-capture.cjs" --validate-result "$fixture" >/dev/null
validation_status=$?
set -e
if [ "$validation_status" -ne 2 ]; then
  printf 'incomparable browser capture returned %s instead of 2\n' "$validation_status" >&2
  exit 1
fi
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
