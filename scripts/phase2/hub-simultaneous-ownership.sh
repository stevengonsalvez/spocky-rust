#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
expected_baseline=28f6c78833065fd282f9064f92a9aa61875dd359
canonical_baseline="$repository_root/.baselines/hub"
worktree_baseline="$repository_root/../../paseo-rust/.baselines/hub"
if [ -n "${PASEO_HUB_BASELINE_ROOT:-}" ]; then
  if [ ! -d "$PASEO_HUB_BASELINE_ROOT" ]; then
    printf 'configured Hub baseline is missing: %s\n' "$PASEO_HUB_BASELINE_ROOT" >&2
    exit 1
  fi
  baseline_root=$(CDPATH='' cd -- "$PASEO_HUB_BASELINE_ROOT" && pwd -P)
elif [ -d "$canonical_baseline" ] && [ -d "$worktree_baseline" ]; then
  canonical_resolved=$(CDPATH='' cd -- "$canonical_baseline" && pwd -P)
  worktree_resolved=$(CDPATH='' cd -- "$worktree_baseline" && pwd -P)
  if [ "$canonical_resolved" != "$worktree_resolved" ]; then
    printf 'ambiguous Hub baselines: %s and %s\n' "$canonical_resolved" "$worktree_resolved" >&2
    exit 1
  fi
  baseline_root=$canonical_resolved
elif [ -d "$canonical_baseline" ]; then
  baseline_root=$(CDPATH='' cd -- "$canonical_baseline" && pwd -P)
elif [ -d "$worktree_baseline" ]; then
  baseline_root=$(CDPATH='' cd -- "$worktree_baseline" && pwd -P)
else
  printf 'Hub baseline is missing; checked %s and %s\n' \
    "$canonical_baseline" "$worktree_baseline" >&2
  exit 1
fi
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
if [ "${1:-}" = "--print-baseline-root" ]; then
  printf '%s\n' "$baseline_root"
  exit 0
fi
if [ "$#" -ne 0 ]; then
  printf 'usage: %s [--print-baseline-root]\n' "$0" >&2
  exit 2
fi
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
