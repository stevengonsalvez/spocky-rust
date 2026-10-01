#!/bin/sh
# Regenerates spocky-store zod fixtures from the pinned Paseo schemas.
#
# Usage: zod-oracle.sh <cases.json> > <fixture.json>
#
# Copies the schema source verbatim from the pinned reference (commit
# 5de45e208690b0efc51c59a585ae9729325a9204), installs the zod version pinned in
# its package-lock.json into a disposable directory, and for each case prints
# what the baseline writes after `z.array(schema).parse(JSON.parse(input))`:
# `JSON.stringify(records, null, 2)`, or null when the load fails.
set -eu
CASES=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
REF=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
PINNED=5de45e208690b0efc51c59a585ae9729325a9204
[ "$(git -C "$REF" rev-parse HEAD)" = "$PINNED" ] || { echo "reference is not at $PINNED" >&2; exit 1; }
P=$REF/packages
ZOD=$(grep -A1 '"node_modules/zod"' "$P/../package-lock.json" | sed -n 's/.*"version": "\(.*\)".*/\1/p')
WORK=$(mktemp -d "${TMPDIR:-/tmp}/spocky-zod-oracle.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM
(cd "$WORK" && npm init -y >/dev/null && npm install --silent --no-audit --no-fund "zod@$ZOD" >/dev/null)
{
  echo 'import { z } from "zod";'
  echo 'import { readFileSync } from "node:fs";'
  sed -n 15,105p "$P/server/src/server/workspace-registry.ts"
  cat <<'JS'
const schemas = { workspace: PersistedWorkspaceRecordSchema, project: PersistedProjectRecordSchema };
const cases = JSON.parse(readFileSync(process.argv[2], "utf8"));
const out = cases.map(({ name, kind, input }) => {
  let output = null;
  try {
    output = JSON.stringify(z.array(schemas[kind]).parse(JSON.parse(input)), null, 2);
  } catch {}
  return { name, kind, input, output };
});
process.stdout.write(JSON.stringify(out, null, 2) + "\n");
JS
} > "$WORK/oracle.mts"
cd "$WORK" && node --experimental-strip-types --no-warnings oracle.mts "$CASES"
