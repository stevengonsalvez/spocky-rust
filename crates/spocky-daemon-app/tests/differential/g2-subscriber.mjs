// G2 subscribed-client recorder: subscribe to fetch_agents, create a codex
// agent in auto mode whose first turn asks to run a command, allow it,
// send a second prompt whose turn asks again, deny it, then cancel a held
// turn. Prints the exact wire text of every frame this client receives
// (except pong heartbeats), statuses included, in arrival order: the
// server_info frame first, then the rest.
//
// The client's own message listeners see zod-parsed frames, whose key order
// and extra keys are not the wire's, so the raw JSON text is taken where the
// client's transport hands it to `handleJsonPayload`. A throwaway first
// connection exposes the client class so the hook is in place before the
// recorded connection's handshake.
const [, , paseoRoot, host, project, allowPrompt, denyPrompt, holdPrompt] = process.argv;
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const target = { kind: "endpoint", host };
const probe = await connectToDaemon({ target });
const prototype = Object.getPrototypeOf(probe);
await probe.close();
const raw = [];
const handleJsonPayload = prototype.handleJsonPayload;
prototype.handleJsonPayload = function (payload, length) {
  if (this !== probe) raw.push(payload);
  return handleJsonPayload.call(this, payload, length);
};
const client = await connectToDaemon({ target });
const steps = [];
const step = (name) => steps.push(name);
await client.fetchAgents({ subscribe: {} });
const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await client.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "auto",
  initialPrompt: allowPrompt,
});
const answer = async (behavior) => {
  const waited = await client.waitForFinish(agent.id, 120000);
  step(`waited:${waited.status}`);
  const fetched = await client.fetchAgent({ agentId: agent.id });
  const pending = fetched?.agent?.pendingPermissions ?? [];
  step(`pending:${pending.length}`);
  for (const permission of pending) {
    await client.respondToPermission(
      agent.id,
      permission.id,
      behavior === "allow" ? { behavior: "allow" } : { behavior: "deny", message: "Denied by G2" },
    );
  }
  const done = await client.waitForFinish(agent.id, 120000);
  step(`finished:${done.status}`);
};
await answer("allow");
await client.sendAgentMessage(agent.id, denyPrompt);
await answer("deny");
if (holdPrompt) {
  // Cancel mid-turn: the held reply keeps the turn in flight.
  await client.sendAgentMessage(agent.id, holdPrompt);
  await new Promise((resolve) => setTimeout(resolve, 3000));
  step("cancel");
  await client.cancelAgent(agent.id);
  const cancelled = await client.waitForFinish(agent.id, 120000);
  step(`cancelled:${cancelled.status}`);
}
await new Promise((resolve) => setTimeout(resolve, 1500));
const inner = (text) => {
  const frame = JSON.parse(text);
  return frame.type === "session" && frame.message ? frame.message : frame;
};
const isServerInfo = (message) =>
  message.type === "status" && message.payload?.status === "server_info";
const serverInfo = raw.filter((text) => isServerInfo(inner(text)));
console.log(serverInfo[0] ?? "null");
for (const text of raw) {
  const message = inner(text);
  if (message.type !== "pong" && !isServerInfo(message)) console.log(text);
}
console.error(JSON.stringify(steps));
await client.close();
process.exit(0);
