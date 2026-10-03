// Drives the pinned ClaudeAgentClient with the real SDK against the fake
// stream-json binary and prints every event the session emits.
// argv: <dist> <scenario file> <fake binary>.
import { readFileSync } from "node:fs";

const [dist, scenarioFile, fake] = process.argv.slice(1);
const scenario = JSON.parse(readFileSync(scenarioFile, "utf8"));
const { ClaudeAgentClient } = await import(`${dist}/server/agent/providers/claude/agent.js`);

const quietLogger = () => {
  const logger = {};
  for (const level of ["trace", "debug", "info", "warn", "error", "fatal"]) logger[level] = () => {};
  logger.child = () => logger;
  return logger;
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const client = new ClaudeAgentClient({ logger: quietLogger(), resolveBinary: async () => fake });
const lines = [];
const note = (kind, value) =>
  lines.push(`${kind} ${typeof value === "string" ? value : JSON.stringify(value)}`);
try {
  const session = await client.createSession(scenario.config);
  session.subscribe((event) => note("EVENT", event));
  let turnId;
  try {
    const started = await session.startTurn(scenario.prompt ?? "hello");
    turnId = started.turnId;
    note("RESULT", started);
  } catch (error) {
    note("RESULT", `ERROR ${error instanceof Error ? error.message : String(error)}`);
  }
  for (const step of scenario.steps ?? []) {
    await sleep(step.afterMs ?? 300);
    if (step.steer) {
      const expectedTurnId = step.steer.otherTurn ? "not-the-active-turn" : turnId;
      try {
        note("RESULT", await session.steerActiveTurn(step.steer.prompt, {
          expectedTurnId,
          clearPendingPermissions: step.steer.clear,
        }));
      } catch (error) {
        note("RESULT", `ERROR ${error instanceof Error ? error.message : String(error)}`);
      }
      continue;
    }
    if (step.respond) {
      const pending = session.getPendingPermissions();
      if (pending.length === 0) {
        note("RESULT", "no pending permission");
        continue;
      }
      try {
        note("RESULT", (await session.respondToPermission(pending[0].id, step.respond)) ?? null);
      } catch (error) {
        note("RESULT", `ERROR ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }
  await sleep(scenario.waitMs ?? 800);
  await session.close().catch(() => {});
} catch (error) {
  note("RESULT", `ERROR ${error instanceof Error ? error.message : String(error)}`);
}
process.stdout.write(lines.join("\n") + "\n");
process.exit(0);
