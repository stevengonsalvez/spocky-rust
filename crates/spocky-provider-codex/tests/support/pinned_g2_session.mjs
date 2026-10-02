// Drives one G2 approval scenario through the pinned Paseo Codex session,
// with `codex app-server` replaced by the replay of a recorded session, and
// prints what the session emitted as one JSON line:
// { events, pendingBefore, pendingAfter }.
//
// argv: <pinned agent module> <replay argv as JSON> <cwd> <model> <prompt> <action>
// action: allow | deny | deny_interrupt | interrupt
import process from "node:process";

const [modulePath, replayArgv, cwd, model, prompt, action] = process.argv.slice(2);
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
const session = await client.createSession({ provider: "codex", cwd, modeId: "auto", model });
const events = [];
session.subscribe((event) => events.push(event));

async function waitFor(types) {
  const deadline = Date.now() + 30_000;
  for (;;) {
    const found = events.find((event) => types.includes(event.type));
    if (found) return found;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${types.join(" or ")}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

await session.getRuntimeInfo();
await session.startTurn(prompt);
const requested = await waitFor(["permission_requested"]);
const pendingBefore = session.getPendingPermissions();
const id = requested.request.id;
switch (action) {
  case "allow":
    await session.respondToPermission(id, { behavior: "allow" });
    break;
  case "deny":
    await session.respondToPermission(id, { behavior: "deny", message: "Not now" });
    break;
  case "deny_interrupt":
    await session.respondToPermission(id, {
      behavior: "deny",
      message: "Stop",
      interrupt: true,
    });
    break;
  case "interrupt":
    await session.interrupt();
    break;
  default:
    throw new Error(`unknown action ${action}`);
}
await waitFor(["turn_completed", "turn_canceled", "turn_failed"]);
const pendingAfter = session.getPendingPermissions();
await session.close();
process.stdout.write(`${JSON.stringify({ events, pendingBefore, pendingAfter })}\n`);
process.exit(0);
