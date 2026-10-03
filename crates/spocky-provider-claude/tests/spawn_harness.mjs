// Drives the pinned ClaudeAgentClient with the real SDK against a recorder
// binary, so the recorder sees the argv, environment and stdin the pinned
// build produces. argv: <dist> <scenario file> <recorder path>.
import { readFileSync } from "node:fs";

const [dist, scenarioFile, recorder] = process.argv.slice(1);
const scenario = JSON.parse(readFileSync(scenarioFile, "utf8"));
const { ClaudeAgentClient } = await import(`${dist}/server/agent/providers/claude/agent.js`);

const quietLogger = () => {
  const logger = {};
  for (const level of ["trace", "debug", "info", "warn", "error", "fatal"]) logger[level] = () => {};
  logger.child = () => logger;
  return logger;
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const client = new ClaudeAgentClient({
  logger: quietLogger(),
  resolveBinary: async () => recorder,
});
try {
  const session = scenario.resume
    ? await client.resumeSession(scenario.resume, scenario.overrides)
    : await client.createSession(scenario.config);
  session.subscribe(() => {});
  await session.startTurn(scenario.prompt ?? "hello").catch(() => {});
  await sleep(scenario.waitMs ?? 1500);
  await session.close().catch(() => {});
} catch (error) {
  process.stdout.write(`ERROR ${error instanceof Error ? error.message : String(error)}\n`);
}
process.exit(0);
