// Drives one steer scenario through the pinned Paseo Codex session, with
// `codex app-server` replaced by the replay of a recorded session, and prints
// one JSON line: { events, steers, pendingAfter }, where `steers` holds the
// status of each `steerActiveTurn` call in order.
//
// argv: <pinned agent module> <replay argv as JSON> <cwd> <model> <scenario>
//   accepted:        no turn, a running turn (another id, then its own id),
//                    interrupt, then the ended turn
//   clears_approval: a steer with clearPendingPermissions over a pending approval
import process from "node:process";

const [modulePath, replayArgv, cwd, model, scenario] = process.argv.slice(2);
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

const steers = [];
const steer = async (text, expectedTurnId, clearPendingPermissions) => {
  const options = { expectedTurnId, clientMessageId: "steer-1" };
  if (clearPendingPermissions) options.clearPendingPermissions = true;
  steers.push((await session.steerActiveTurn(text, options)).status);
};

await session.getRuntimeInfo();
if (scenario === "accepted") {
  await steer("Also say goodbye", "codex-turn-0", false);
  const { turnId } = await session.startTurn("Say hello");
  await waitFor(["turn_started"]);
  await steer("Also say goodbye", "codex-turn-99", false);
  await steer("Also say goodbye", turnId, false);
  await session.interrupt();
  await waitFor(["turn_canceled"]);
  await steer("Also say goodbye", turnId, false);
} else if (scenario === "clears_approval") {
  const { turnId } = await session.startTurn("Run echo");
  await waitFor(["permission_requested"]);
  await steer("Do not run it", turnId, true);
  await waitFor(["turn_completed", "turn_canceled", "turn_failed"]);
} else {
  throw new Error(`unknown scenario ${scenario}`);
}
const pendingAfter = session.getPendingPermissions();
await session.close();
process.stdout.write(`${JSON.stringify({ events, steers, pendingAfter })}\n`, () => process.exit(0));
