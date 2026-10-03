// Unarchive recorder: against a home whose only agent is archived, subscribe
// to fetch_agents, send the agent a message, wait for the turn to finish and
// read the agent back. Prints the exact wire text of the frames it receives
// for those requests and the agent_update frames, in arrival order.
//
// The client's own message listeners see zod-parsed frames, whose key order
// and extra keys are not the wire's, so the raw JSON text is taken where the
// client's transport hands it to `handleJsonPayload`. A throwaway first
// connection exposes the client class so the hook is in place before the
// recorded connection's handshake.
const [, , paseoRoot, host] = process.argv;
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
  "send_agent_message_response",
  "wait_for_finish_response",
  "agent_update",
  "rpc_error",
]);
const outcomes = [];
const attempt = async (name, call) => {
  try {
    const result = await call();
    outcomes.push(`${name}:ok`);
    return result;
  } catch (error) {
    outcomes.push(`${name}:error:${error?.message}`);
    return undefined;
  }
};
await attempt("subscribe", () => client.fetchAgents({ subscribe: {}, filter: { includeArchived: true } }));
const listed = await attempt("agents", () => client.fetchAgents({ filter: { includeArchived: true } }));
const agentId = listed?.entries?.[0]?.agent?.id;
outcomes.push(`archived-before:${Boolean(listed?.entries?.[0]?.agent?.archivedAt)}`);
await attempt("send", () => client.sendAgentMessage(agentId, "Reply with the single word AGAIN."));
await attempt("wait", () => client.waitForFinish(agentId, 120000));
const fetched = await attempt("agent", () => client.fetchAgent(agentId));
outcomes.push(`archived-after:${Boolean(fetched?.agent?.archivedAt)}`);
await new Promise((resolve) => setTimeout(resolve, 1500));
for (const text of raw) {
  const frame = JSON.parse(text);
  const message = frame.type === "session" && frame.message ? frame.message : frame;
  if (keep.has(message.type)) console.log(text);
}
console.error(JSON.stringify(outcomes));
await client.close();
process.exit(0);
