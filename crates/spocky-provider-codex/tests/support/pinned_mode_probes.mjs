// Drives the pinned Paseo Codex client's resolveDefaultModeId through the
// calls the daemon makes and prints each result as one JSON line:
// [{ call, mode } | { call, aborted: true }, ...]. Before each call it writes
// a `# <call>` marker to the shim's argv log, so a test can count the
// `--version` probes each call made.
//
// argv: <pinned agent module> <codex shim> <argv log> <cwd>
import { appendFileSync } from "node:fs";
import process from "node:process";

const [modulePath, shim, argvLog, cwd] = process.argv.slice(2);
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
  command: { mode: "replace", argv: [shim] },
});
const config = { provider: "codex", cwd, modeId: "auto" };
const results = [];

async function step(call, input) {
  appendFileSync(argvLog, `# ${call}\n`);
  try {
    results.push({ call, mode: await client.resolveDefaultModeId(input) });
  } catch (error) {
    if (input.signal?.aborted && error === input.signal.reason) {
      results.push({ call, aborted: true });
    } else {
      throw error;
    }
  }
}

await step("no-signal", { config });
await step("no-signal-again", { config });
await step("signal", { config, signal: new AbortController().signal });
const controller = new AbortController();
controller.abort();
await step("aborted", { config, signal: controller.signal });
process.stdout.write(`${JSON.stringify(results)}\n`);
process.exit(0);
