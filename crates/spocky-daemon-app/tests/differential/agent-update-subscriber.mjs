// Subscribed-client agent_update recorder: one client subscribes to
// fetch_agents, then creates a workspace and a codex agent running the G1
// prompt in full-access mode, waits for it to finish, and prints every
// fetch_agents_response, agent_update and agent.create.response frame in
// arrival order as JSON lines.
const [, , paseoRoot, host, project] = process.argv;
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const client = await connectToDaemon({ target: { kind: "endpoint", host } });
const frames = [];
const keep = new Set(["fetch_agents_response", "agent_update", "agent.create.response"]);
client.subscribeRawMessages((message) => {
  if (keep.has(message.type)) frames.push(message);
});
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
for (const frame of frames) console.log(JSON.stringify(frame));
console.error(`status=${finished.status} subscribed=${Boolean(subscribed.subscription)}`);
await client.close();
process.exit(0);
