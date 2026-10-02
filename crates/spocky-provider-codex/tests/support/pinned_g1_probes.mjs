// Drives the pinned Paseo `CodexAppServerAgentClient` through the G1 launch
// sequence the daemon runs, writing a phase marker to the launcher's argv log
// before each step, so the Rust provider's log can be compared with it.
//
// argv: <pinned agent module> <launcher> <argv log> <cwd> <model>
// The env (HOME, CODEX_HOME, proxies) comes from the caller.
import { appendFileSync } from "node:fs";

const [modulePath, launcher, argvLog, cwd, model] = process.argv.slice(2);
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
  command: { mode: "replace", argv: [launcher] },
});
const phase = (name) => appendFileSync(argvLog, `# ${name}\n`);
const signalled = {
  signal: new AbortController().signal,
  runActivity: (_name, operation) => operation(),
};

phase("is-available");
await client.isAvailable();
phase("catalog-unsignalled");
await client.fetchCatalog({});
phase("catalog-signalled");
await client.fetchCatalog({}, signalled);
phase("is-available");
await client.isAvailable();
phase("create-session");
const session = await client.createSession({
  provider: "codex",
  cwd,
  modeId: "full-access",
  model,
});
phase("close");
await session.close();
process.exit(0);
