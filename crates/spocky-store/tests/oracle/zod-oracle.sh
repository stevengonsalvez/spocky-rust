#!/bin/sh
# Regenerates spocky-store zod fixtures from the pinned Paseo schemas.
#
# Usage: zod-oracle.sh <cases.json> > <fixture.json>
#
# Each case is {name, kind, input}. kind "workspace" or "project" prints what
# the baseline writes after `z.array(schema).parse(JSON.parse(input))`, that
# is `JSON.stringify(records, null, 2)`; kind "agent" prints
# `JSON.stringify(STORED_AGENT_SCHEMA.parse(JSON.parse(input)), null, 2)`.
# output is null where the baseline parse throws.
#
# Schema text is copied verbatim from the pinned reference (commit
# 5de45e208690b0efc51c59a585ae9729325a9204). zod comes from
# ZOD_NODE_MODULES (a node_modules directory installed from the pinned
# package-lock.json, for example the slice harness `npm ci` copy) when set;
# otherwise npm installs the lock's zod version, bounded by a timeout.
# Either way the zod version must equal the pinned lock's version.
set -eu
CASES=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
REF=${PASEO_REFERENCE_ROOT:-/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite}
PINNED=5de45e208690b0efc51c59a585ae9729325a9204
[ "$(git -C "$REF" rev-parse HEAD)" = "$PINNED" ] || { echo "reference is not at $PINNED" >&2; exit 1; }
P=$REF/packages
ZOD=$(grep -A1 '"node_modules/zod"' "$P/../package-lock.json" | sed -n 's/.*"version": "\(.*\)".*/\1/p')
WORK=$(mktemp -d "${TMPDIR:-/tmp}/spocky-zod-oracle.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM
if [ -n "${ZOD_NODE_MODULES:-}" ]; then
  ln -s "$ZOD_NODE_MODULES" "$WORK/node_modules"
else
  (cd "$WORK" && npm init -y >/dev/null \
    && gtimeout --kill-after=10 300 npm install --silent --no-audit --no-fund "zod@$ZOD" >/dev/null)
fi
FOUND=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$WORK/node_modules/zod/package.json")
[ "$FOUND" = "$ZOD" ] || { echo "zod $FOUND does not match pinned $ZOD" >&2; exit 1; }
{
  echo 'import { z } from "zod";'
  echo 'import { readFileSync } from "node:fs";'
  sed -n 1,7p "$P/protocol/src/agent-lifecycle.ts"
  echo 'const AgentStatusSchema = z.enum(AGENT_LIFECYCLE_STATUSES);'
  sed -n 273,279p "$P/protocol/src/messages.ts"
  sed -n 287,311p "$P/protocol/src/messages.ts" | sed 's/^export //'
  sed -n 3,9p "$P/server/src/server/agent/agent-owner.ts" | sed 's/^export //'
  sed -n 13,78p "$P/server/src/server/agent/agent-storage.ts"
  sed -n 15,105p "$P/server/src/server/workspace-registry.ts"
  cat <<'JS'
const parsers = {
  workspace: (value) => JSON.stringify(z.array(PersistedWorkspaceRecordSchema).parse(value), null, 2),
  project: (value) => JSON.stringify(z.array(PersistedProjectRecordSchema).parse(value), null, 2),
  agent: (value) => JSON.stringify(STORED_AGENT_SCHEMA.parse(value), null, 2),
};
const cases = JSON.parse(readFileSync(process.argv[2], "utf8"));
const out = cases.map(({ name, kind, input }) => {
  let output = null;
  try {
    output = parsers[kind](JSON.parse(input));
  } catch {}
  return { name, kind, input, output };
});
process.stdout.write(JSON.stringify(out, null, 2) + "\n");
JS
} > "$WORK/oracle.mts"
cd "$WORK" && node --experimental-strip-types --no-warnings oracle.mts "$CASES"
