#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
fixture_root=$(gtimeout 30 mktemp -d "${TMPDIR:-/tmp}/spocky-hub-retained-path.XXXXXX")
cleanup() {
  gtimeout 30 rm -rf "$fixture_root"
}
trap cleanup EXIT HUP INT TERM

fake_repository="$fixture_root/repository"
gtimeout 30 mkdir -p "$fake_repository/scripts/phase2" "$fake_repository/.baselines/hub"
expected=$(CDPATH='' cd -- "$fake_repository/.baselines/hub" && pwd)

for script in hub-embedded-retained.test.sh hub-embedded-retained-evidence.sh; do
  gtimeout 30 cp "$repository_root/scripts/phase2/$script" "$fake_repository/scripts/phase2/$script"
  actual=$(gtimeout 30 sh "$fake_repository/scripts/phase2/$script" --print-baseline-root)
  if [ "$actual" != "$expected" ]; then
    printf '%s defaulted to %s, expected %s\n' "$script" "$actual" "$expected" >&2
    exit 1
  fi
done

printf 'retained Hub scripts prefer repository-local baseline\n'
