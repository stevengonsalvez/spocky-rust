#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
reference_root=${PASEO_REFERENCE_ROOT:-"$repository_root/../paseo-rewrite"}
capture_dir=$(mktemp -d /private/tmp/paseo-phase2-capture.XXXXXX)

cleanup() {
  case "$capture_dir" in
    /private/tmp/paseo-phase2-capture.*) rm -rf "$capture_dir" ;;
    *) printf 'refusing to remove unexpected capture directory: %s\n' "$capture_dir" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$capture_dir/protocol" "$repository_root/evidence/raw/phase2"
cp -R "$reference_root/packages/protocol/src" "$capture_dir/protocol/src"
cp "$reference_root/packages/relay/src/crypto.ts" "$capture_dir/crypto.ts"
cp "$repository_root/scripts/phase2/wire-baseline.ts" "$capture_dir/wire-baseline.ts"
cp "$repository_root/scripts/phase2/crypto-baseline.ts" "$capture_dir/crypto-baseline.ts"

npm install --prefix "$capture_dir" --no-package-lock --ignore-scripts --no-audit --no-fund \
  tsx@4.21.0 zod@4.4.3 semver@7.7.4 tweetnacl@1.0.3 base64-js@1.5.1 >/dev/null

"$capture_dir/node_modules/.bin/tsx" "$capture_dir/wire-baseline.ts" \
  "$repository_root/evidence/raw/phase2/pinned-wire.json"
"$capture_dir/node_modules/.bin/tsx" "$capture_dir/crypto-baseline.ts" \
  "$repository_root/evidence/raw/phase2/pinned-crypto.json"

shasum -a 256 \
  "$repository_root/evidence/raw/phase2/pinned-wire.json" \
  "$repository_root/evidence/raw/phase2/pinned-crypto.json"
