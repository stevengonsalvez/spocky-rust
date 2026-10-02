// Handler recorder: create a codex agent that runs the G1 prompt to the end,
// then ask the read handlers (fetch_agents, fetch_agent, fetch_agent_timeline,
// wait_for_finish) for the finished agent and for an agent that does not
// exist. Prints the exact wire text of every response frame of those types
// and of rpc_error, in arrival order.
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
const keep = new Set([
  "fetch_agents_response",
  "fetch_agent_response",
  "fetch_agent_timeline_response",
  "wait_for_finish_response",
  "rpc_error",
]);
const outcomes = [];
const attempt = async (name, call) => {
  try {
    await call();
    outcomes.push(`${name}:ok`);
  } catch (error) {
    outcomes.push(`${name}:error:${error?.message}`);
  }
};
const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await client.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "full-access",
  initialPrompt: "Reply with the single word READY.",
});
const missing = "00000000-0000-4000-8000-000000000000";
await attempt("wait", () => client.waitForFinish(agent.id, 120000));
await attempt("agents", () => client.fetchAgents());
await attempt("agents-filtered", () =>
  client.fetchAgents({ filter: { labels: { nothing: "matches" } } }),
);
await attempt("agent", () => client.fetchAgent(agent.id));
await attempt("agent-missing", () => client.fetchAgent(missing));
await attempt("timeline", () => client.fetchAgentTimeline(agent.id));
await attempt("timeline-tail", () =>
  client.fetchAgentTimeline(agent.id, { direction: "tail", limit: 2 }),
);
await attempt("timeline-missing", () => client.fetchAgentTimeline(missing));
await attempt("wait-idle", () => client.waitForFinish(agent.id, 5000));
await attempt("wait-missing", () => client.waitForFinish(missing, 5000));
await new Promise((resolve) => setTimeout(resolve, 1000));
for (const text of raw) {
  const frame = JSON.parse(text);
  const message = frame.type === "session" && frame.message ? frame.message : frame;
  if (keep.has(message.type)) console.log(text);
}
console.error(JSON.stringify(outcomes));
await client.close();
process.exit(0);
