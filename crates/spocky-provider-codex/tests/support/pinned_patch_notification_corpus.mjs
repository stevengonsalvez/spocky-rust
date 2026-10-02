// Runs a corpus of legacy `codex/event/patch_apply_*` payloads through the
// pinned `mapCodexPatchNotificationToToolCall` and prints one JSON line per
// case: null when Paseo returns null, else
//   { envelope, final: { status, error } }
// where `envelope` is what `mapCodexToolCallEnvelope` hands
// `toToolCallFromNormalizedEnvelope` (before the shared edit-detail branch)
// and `final` is the status and error of the timeline item Paseo emits.
//
// The envelope is not exposed by the pinned build, so patched COPIES of the
// tool-call mapper (capturing it) and of the agent module (importing that
// copy) are used; the pinned tree itself is not touched.
//
// argv: <pinned providers dir> <scratch dir> <corpus json>
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import process from "node:process";

import { patchedCopyUrl } from "./pinned_copy.mjs";

const [providersDir, scratch, corpusPath] = process.argv.slice(2);

const mapperUrl = patchedCopyUrl({
  pinnedFile: join(providersDir, "codex/tool-call-mapper.js"),
  scratch,
  name: "tool-call-mapper",
  transform: (source) => {
    const head = "function toToolCallFromNormalizedEnvelope(envelope) {";
    if (!source.includes(head)) throw new Error("toToolCallFromNormalizedEnvelope not found");
    return source.replace(head, `${head}\n    globalThis.__envelope = envelope;`);
  },
});
const agentUrl = patchedCopyUrl({
  pinnedFile: join(providersDir, "codex-app-server-agent.js"),
  scratch,
  name: "codex-app-server-agent",
  rewrites: { "./codex/tool-call-mapper.js": mapperUrl },
});
const { mapCodexPatchNotificationToToolCall } = await import(agentUrl);

const lines = [];
for (const params of JSON.parse(readFileSync(corpusPath, "utf8")).cases) {
  globalThis.__envelope = undefined;
  const mapped = mapCodexPatchNotificationToToolCall(params.input);
  lines.push(
    mapped === null
      ? "null"
      : JSON.stringify({
          envelope: globalThis.__envelope,
          final: { status: mapped.status, error: mapped.error },
        }),
  );
}
process.stdout.write(`${lines.join("\n")}\n`, () => process.exit(0));
