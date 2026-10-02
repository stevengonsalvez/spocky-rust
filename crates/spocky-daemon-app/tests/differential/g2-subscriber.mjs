// G2 subscribed-client recorder: subscribe to fetch_agents, create a codex
// agent in auto mode whose first turn asks to run a command, allow it,
// send a second prompt whose turn asks again, deny it. Prints the frames
// this client receives (except pings) in arrival order as JSON lines.
const [, , paseoRoot, host, project, allowPrompt, denyPrompt, holdPrompt] = process.argv;
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const client = await connectToDaemon({ target: { kind: "endpoint", host } });
const frames = [];
const skip = new Set(["pong", "status"]);
client.subscribeRawMessages((message) => {
  if (!skip.has(message.type)) frames.push(message);
});
const step = (name) => frames.push({ step: name });
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
console.log(JSON.stringify({ serverInfoFeatures: client.lastServerInfoMessage?.features ?? null }));
for (const frame of frames) if (!frame.step) console.log(JSON.stringify(frame));
console.error(JSON.stringify(frames.filter((frame) => frame.step).map((frame) => frame.step)));
await client.close();
process.exit(0);
