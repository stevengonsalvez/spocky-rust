// Drives the pinned Paseo Codex client's native archive step against the
// replay of a recorded session and prints the outcome as one JSON line:
//   {"ok":true}, or {"error":{"message":...}} when it rejects.
//
// argv: <pinned agent module> <replay argv as JSON> <archive | restore> <thread id>
import process from "node:process";

const [modulePath, replayArgv, state, threadId] = process.argv.slice(2);
const { CodexAppServerAgentClient } = await import(modulePath);

const noop = () => {};
const logger = {
  trace: noop,
  debug: noop,
  info: noop,
  warn: noop,
  error: noop,
  child() {
    return logger;
  },
};
const client = new CodexAppServerAgentClient(logger, {
  command: { mode: "replace", argv: JSON.parse(replayArgv) },
});
const handle = { provider: "codex", sessionId: threadId };

let line;
try {
  if (state === "archive") await client.archiveNativeSession(handle);
  else await client.unarchiveNativeSession(handle);
  line = JSON.stringify({ ok: true });
} catch (error) {
  line = JSON.stringify({
    error: { message: error instanceof Error ? error.message : String(error) },
  });
}
process.stdout.write(`${line}\n`, () => process.exit(0));
