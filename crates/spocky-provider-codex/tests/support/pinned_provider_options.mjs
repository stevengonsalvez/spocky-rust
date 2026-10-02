// Creates one pinned Paseo Codex session with the given `providerOptions`
// (and no `modeId`, so the provider-option fallbacks decide the mode), asks
// for its runtime info, and prints:
//   the info as `JSON.stringify` writes it, or
//   {"error":{"name":...,"message":...}} when createSession or the info call
//   rejects (the pinned constructor validates providerOptions with a strict
//   zod schema and throws a ZodError for a value outside it).
//
// argv: <pinned agent module> <replay argv as JSON> <cwd> <model> <providerOptions JSON | absent>
import process from "node:process";

const [modulePath, replayArgv, cwd, model, options] = process.argv.slice(2);
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
const config = { provider: "codex", cwd, model };
if (options !== "absent") config.providerOptions = JSON.parse(options);

let line;
try {
  const session = await client.createSession(config);
  try {
    line = JSON.stringify(await session.getRuntimeInfo());
  } finally {
    await session.close();
  }
} catch (error) {
  line = JSON.stringify({
    error: { name: error?.name ?? "Error", message: error instanceof Error ? error.message : String(error) },
  });
}
process.stdout.write(`${line}\n`, () => process.exit(0));
