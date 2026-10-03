// Runs a corpus of errors through the pinned `isDefinitiveCodexSteerRejection`
// and prints one JSON array of booleans, in corpus order.
//
// The function is not exported by the pinned build, so a patched COPY of the
// agent module exports it; the pinned tree itself is not touched.
//
// A case is `{ message, rpc: { code?, data? } }` (a `CodexAppServerRpcError`;
// a missing key stays `undefined`), or `{ message }` alone (a plain `Error`).
//
// argv: <pinned providers dir> <scratch dir> <corpus JSON>
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import process from "node:process";

import { patchedCopyUrl } from "./pinned_copy.mjs";

const [providersDir, scratch, corpus] = process.argv.slice(2);

const agentUrl = patchedCopyUrl({
  pinnedFile: join(providersDir, "codex-app-server-agent.js"),
  scratch,
  name: "codex-app-server-agent",
  transform: (source) => `${source}\nexport { isDefinitiveCodexSteerRejection };\n`,
});
const { isDefinitiveCodexSteerRejection } = await import(agentUrl);
const { CodexAppServerRpcError } = await import(
  pathToFileURL(join(providersDir, "codex/app-server-transport.js")).href
);

const results = JSON.parse(corpus).map((entry) => {
  const error = entry.rpc
    ? new CodexAppServerRpcError(entry.message, entry.rpc.code, entry.rpc.data)
    : new Error(entry.message);
  return isDefinitiveCodexSteerRejection(error);
});
process.stdout.write(`${JSON.stringify(results)}\n`, () => process.exit(0));
