#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
runner="$repository_root/scripts/phase2/relay-residual-differential.sh"

plan=$($runner --print-plan)
printf '%s\n' "$plan" | grep -F 'Relay baseline: 3fc41c96c8c63f3a7109e832899cc57d473c4531'
printf '%s\n' "$plan" | grep -F 'handshakes: escaped keys and values, accepted and rejected'
printf '%s\n' "$plan" | grep -F 'fragments: 33554417, 33554418, 33554419 assembled bytes'
printf '%s\n' "$plan" | grep -F 'comparison: byte-for-byte raw output'

$runner --preflight-only

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/relay-residual-compare.XXXXXX")
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
printf 'case\tforward\tpayload\n' >"$fixture_root/baseline.tsv"
cp "$fixture_root/baseline.tsv" "$fixture_root/rust.tsv"
$runner --compare-files "$fixture_root/baseline.tsv" "$fixture_root/rust.tsv"

printf 'case\tclose\t1008\n' >"$fixture_root/rust.tsv"
if $runner --compare-files "$fixture_root/baseline.tsv" "$fixture_root/rust.tsv" 2>/dev/null; then
  printf 'relay residual comparison accepted different outputs\n' >&2
  exit 1
fi
