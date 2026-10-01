#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
baseline_root="${PASEO_HUB_BASELINE_ROOT:-$repository_root/../../paseo-rust/.baselines/hub}"
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-simultaneous-ownership.XXXXXX")
cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/spocky-hub-simultaneous-ownership.*)
      gtimeout 30 chmod -R u+w "$fixture_root" 2>/dev/null || true
      gtimeout 30 rm -rf "$fixture_root"
      ;;
    *) printf 'refusing cleanup: %s\n' "$fixture_root" >&2 ;;
  esac
}
trap cleanup EXIT HUP INT TERM

actual_baseline=$(gtimeout 30 git -C "$baseline_root" rev-parse HEAD)
[ "$actual_baseline" = "$expected_baseline" ]
[ -z "$(gtimeout 30 git -C "$baseline_root" status --porcelain)" ]
gtimeout --kill-after=30 300 git -C "$baseline_root" archive -o "$fixture_root/baseline.tar" "$expected_baseline"
gtimeout 30 mkdir "$fixture_root/source" "$fixture_root/database"
gtimeout 60 tar -xf "$fixture_root/baseline.tar" -C "$fixture_root/source"
gtimeout 30 chmod -R a-w "$fixture_root/source"

gtimeout --kill-after=30 300 cargo build --locked --manifest-path "$repository_root/Cargo.toml" \
  -p spocky-hub-pilot --bin hub-simultaneous-ownership-evidence >/dev/null
gtimeout --kill-after=30 90 node "$repository_root/scripts/phase2/hub-simultaneous-ownership-orchestrator.mjs" \
  "$repository_root/target/debug/hub-simultaneous-ownership-evidence" \
  "$fixture_root/database" \
  "$fixture_root/source/src/db/runtime/internal/data-directory-lock.ts" \
  "$expected_baseline"
