// Subscribed-client agent_update recorder: one client subscribes to
// fetch_agents, then creates a workspace and a codex agent running the G1
// prompt in full-access mode, waits for it to finish, and prints every
// fetch_agents_response, agent_update and agent.create.response frame in
// arrival order as the exact wire text, one frame per line.
//
// The client's own message listeners see zod-parsed frames, whose key order
// and extra keys are not the wire's, so the raw JSON text is taken where the
// client's transport hands it to `handleJsonPayload`. A throwaway first
// connection exposes the client class so the hook is in place before the
// recorded connection's handshake.
const [, , paseoRoot, host, project] = process.argv;
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
const keep = new Set(["fetch_agents_response", "agent_update", "agent.create.response"]);
const subscribed = await client.fetchAgents({ subscribe: {} });
const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await client.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "full-access",
  initialPrompt: "Reply with the single word READY.",
});
const finished = await client.waitForFinish(agent.id, 120000);
await new Promise((resolve) => setTimeout(resolve, 1500));
for (const text of raw) {
  const frame = JSON.parse(text);
  const message = frame.type === "session" && frame.message ? frame.message : frame;
  if (keep.has(message.type)) console.log(text);
}
console.error(`status=${finished.status} subscribed=${Boolean(subscribed.subscription)}`);
await client.close();
process.exit(0);
