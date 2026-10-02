// G4 disconnect probe, run by the slice harness under the egress sandbox with
// the pinned node. Client A creates an agent whose turn the stub holds open,
// waits for it to finish, and drops its socket 1.5 s into the wait; client B
// then connects, fetches the agent and cancels the held turn.
//
// argv: <paseoRoot> --host <host:port> <project> <holdPrompt>
//
// stdout line 1 is {"aFramesAfterDrop": <n>, "status": "<fetched status>",
// "workspaceId": "<id>"};
// the remaining lines are client B's raw wire text, unmodified except that
// pings, pongs and the server_info status are left out. Wire text is taken by
// hooking DaemonClient.prototype.handleJsonPayload, so key order and unknown
// keys are preserved.
const [, , paseoRoot, hostFlag, host, project, holdPrompt] = process.argv;
if (hostFlag !== "--host" || !host || !project || !holdPrompt) {
  console.error("usage: g4-subscriber.mjs <paseoRoot> --host <host> <project> <holdPrompt>");
  process.exit(2);
}
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const target = { kind: "endpoint", host };
const probe = await connectToDaemon({ target });
const prototype = Object.getPrototypeOf(probe);
await probe.close();
const raw = new Map();
const original = prototype.handleJsonPayload;
prototype.handleJsonPayload = function (payload, length) {
  if (!raw.has(this)) raw.set(this, []);
  raw.get(this).push(payload);
  return original.call(this, payload, length);
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const a = await connectToDaemon({ target });
const created = await a.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await a.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "full-access",
  initialPrompt: holdPrompt,
});
const waiting = a.waitForFinish(agent.id, 120000).catch(() => "dropped");
await sleep(1500);
const before = raw.get(a).length;
await a.close();
await waiting;
await sleep(500);
const b = await connectToDaemon({ target });
const fetched = await b.fetchAgent({ agentId: agent.id });
await b.cancelAgent(agent.id);
await sleep(1500);
console.log(
  JSON.stringify({
    aFramesAfterDrop: raw.get(a).length - before,
    status: fetched?.agent?.status,
    workspaceId: created.workspace.id,
  }),
);
for (const text of raw.get(b)) {
  const frame = JSON.parse(text);
  const message = frame.type === "session" ? frame.message : frame;
  if (message.type !== "pong" && !(message.type === "status" && message.payload?.status === "server_info")) {
    console.log(text);
  }
}
await b.close();
process.exit(0);
