// Runs the pinned Paseo Codex client with no codex binary on PATH and prints
// what each entry point reports as one JSON line:
// { available, create, resume, catalog, catalogSignalled }.
// A rejected call is recorded as { error: message }.
//
// argv: <pinned agent module> <cwd>
import process from "node:process";

const [modulePath, cwd] = process.argv.slice(2);
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
const client = new CodexAppServerAgentClient(logger);

async function outcome(call) {
  try {
    return { value: await call() };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}

const signalled = {
  signal: new AbortController().signal,
  runActivity: (_name, operation) => operation(),
};
const config = { provider: "codex", cwd, modeId: "full-access" };
const result = {
  available: await outcome(() => client.isAvailable()),
  create: await outcome(async () => (await client.createSession(config)).id),
  resume: await outcome(async () =>
    (await client.resumeSession({ sessionId: "thread-1", metadata: { cwd } })).id,
  ),
  catalog: await outcome(() => client.fetchCatalog({})),
  catalogSignalled: await outcome(() => client.fetchCatalog({}, signalled)),
};
process.stdout.write(`${JSON.stringify(result)}\n`);
process.exit(0);
