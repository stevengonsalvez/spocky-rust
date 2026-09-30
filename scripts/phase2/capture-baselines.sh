#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-"$repository_root/../paseo-rewrite"}
expected_baseline=5de45e208690b0efc51c59a585ae9729325a9204
capture_dir=$(mktemp -d /private/tmp/spocky-phase2-capture.XXXXXX)

cleanup() {
  case "$capture_dir" in
    /private/tmp/spocky-phase2-capture.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing to remove unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

actual_baseline=$(git -C "$reference_root" rev-parse HEAD)
if [ "$actual_baseline" != "$expected_baseline" ]; then
  printf 'baseline HEAD mismatch: expected %s, got %s\n' "$expected_baseline" "$actual_baseline" >&2
  exit 1
fi
if [ -n "$(git -C "$reference_root" status --porcelain --untracked-files=no)" ]; then
  printf 'baseline tracked tree is dirty: %s\n' "$reference_root" >&2
  exit 1
fi

if [ "${1:-}" = "--preflight-only" ]; then
  printf 'baseline preflight passed: %s\n' "$actual_baseline"
  exit 0
fi

mkdir -p "$capture_dir/protocol" "$repository_root/evidence/raw/phase2"
cp -R "$reference_root/packages/protocol/src" "$capture_dir/protocol/src"
cp "$reference_root/packages/relay/src/crypto.ts" "$capture_dir/crypto.ts"
cp "$repository_root/scripts/phase2/wire-baseline.ts" "$capture_dir/wire-baseline.ts"
cp "$repository_root/scripts/phase2/crypto-baseline.ts" "$capture_dir/crypto-baseline.ts"
cp "$repository_root/scripts/phase2/runtime/package.json" "$capture_dir/package.json"
cp "$repository_root/scripts/phase2/runtime/package-lock.json" "$capture_dir/package-lock.json"

npm ci --prefix "$capture_dir" --ignore-scripts --no-audit --no-fund >/dev/null

PASEO_CAPTURE_BASELINE=$actual_baseline \
  "$capture_dir/node_modules/.bin/tsx" "$capture_dir/wire-baseline.ts" \
  "$repository_root/evidence/raw/phase2/pinned-wire.json"
PASEO_CAPTURE_BASELINE=$actual_baseline \
  "$capture_dir/node_modules/.bin/tsx" "$capture_dir/crypto-baseline.ts" \
  "$repository_root/evidence/raw/phase2/pinned-crypto.json"

shasum -a 256 \
  "$repository_root/evidence/raw/phase2/pinned-wire.json" \
  "$repository_root/evidence/raw/phase2/pinned-crypto.json"
