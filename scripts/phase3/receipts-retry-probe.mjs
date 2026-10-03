// G4 send-retry probe, run by the slice harness under the egress sandbox with
// the pinned node against a daemon on a disposable home. One client creates an
// agent, sends a message with a fixed messageId, then retries it. Every retry
// of the same messageId must reuse the stored receipt and start no second turn,
// so the scripted stub is consumed by exactly three turns: the initial prompt,
// the first send, and the concurrent pair of fresh-messageId sends.
//
// argv: <paseoRoot> --host <host:port> <project> <initialPrompt> <sendPrompt>
//       <otherPrompt> <racePrompt>
//
// Steps, each with the pinned client's own accepted/error outcome:
//   created        createAgent(initialPrompt), waitForFinish
//   first          sendAgentMessage(sendPrompt, { messageId: "retry-1" }), waitForFinish
//   retry          the same call again: accepted, no new turn
//   retry-other    a second connection repeats it: accepted, no new turn
//   conflict       same messageId, otherPrompt: rejected (request key conflict)
//   race           two concurrent sends of racePrompt with messageId "retry-2",
//                  one per socket: both accepted, one turn
//
// stdout line 1 is {"outcomes": [{"step", "ok", "error"}...], "workspaceId"};
// the rest is the raw wire text, one frame per line in arrival order, every
// frame from connect on, the server_info status included, as three labelled
// blocks, one per connection: "# recording client", "# retry-other connection"
// and "# race second connection". Only the bare heartbeat pongs are left out
// (see HEARTBEAT_PONG).
// Per-run ids and instants in them are masked by the harness's existing
// generated_id and wall_clock classes, never dropped here. Wire text is taken
// by hooking DaemonClient.prototype.handleJsonPayload, so key order and
// unknown keys are preserved. The race sends go over two sockets (the
// recording client and a second connection) so the per-key lock across
// connections is tested.
const [, , paseoRoot, hostFlag, host, project, initialPrompt, sendPrompt, otherPrompt, racePrompt] =
  process.argv;
if (hostFlag !== "--host" || !host || !project || !initialPrompt || !sendPrompt || !otherPrompt || !racePrompt) {
  console.error(
    "usage: receipts-retry-probe.mjs <paseoRoot> --host <host> <project> <initial> <send> <other> <race>",
  );
  process.exit(2);
}
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const target = { kind: "endpoint", host };
const probe = await connectToDaemon({ target });
const prototype = Object.getPrototypeOf(probe);
await probe.close();
// Keyed by instance from the first payload: connectToDaemon resolves inside
// the server_info handler, so a map filled after it returns would miss it.
const recorded = new Map();
const handleJsonPayload = prototype.handleJsonPayload;
prototype.handleJsonPayload = function (payload, length) {
  if (!recorded.has(this)) recorded.set(this, []);
  recorded.get(this).push(payload);
  return handleJsonPayload.call(this, payload, length);
};
const record = (connection) => recorded.get(connection);
// Class client-heartbeat-pong: the client's 10 s liveness heartbeat is
// answered by a bare pong. How many
// arrive, and between which other frames, follows the wall clock (two runs of
// the same daemon put one in different places), so these are left out of the
// ordered wire. Only a frame exactly equal to this text is removed; a pong
// with any other text stays in it. The g4-wire
// fixture compares the daemon's answer to a ping.
const HEARTBEAT_PONG = '{"type":"pong"}';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const outcomes = [];
const attempt = async (step, run) => {
  try {
    await run();
    outcomes.push({ step, ok: true, error: null });
  } catch (error) {
    outcomes.push({ step, ok: false, error: error instanceof Error ? error.message : String(error) });
  }
};
const client = await connectToDaemon({ target });
const clientFrames = () => record(client);
const created = await client.createWorkspace({ source: { kind: "directory", path: project } });
const agent = await client.createAgent({
  provider: "codex",
  cwd: project,
  workspaceId: created.workspace.id,
  modeId: "full-access",
  initialPrompt,
});
await attempt("created", () => client.waitForFinish(agent.id, 60000));
await attempt("first", async () => {
  await client.sendAgentMessage(agent.id, sendPrompt, { messageId: "retry-1" });
  await client.waitForFinish(agent.id, 60000);
});
await attempt("retry", () => client.sendAgentMessage(agent.id, sendPrompt, { messageId: "retry-1" }));
const other = await connectToDaemon({ target });
const otherFrames = () => record(other);
await attempt("retry-other", () => other.sendAgentMessage(agent.id, sendPrompt, { messageId: "retry-1" }));
await sleep(1000);
await other.close();
await attempt("conflict", () => client.sendAgentMessage(agent.id, otherPrompt, { messageId: "retry-1" }));
let secondFrames = () => [];
await attempt("race", async () => {
  const second = await connectToDaemon({ target });
  secondFrames = () => record(second);
  try {
    const sends = await Promise.allSettled([
      client.sendAgentMessage(agent.id, racePrompt, { messageId: "retry-2" }),
      second.sendAgentMessage(agent.id, racePrompt, { messageId: "retry-2" }),
    ]);
    const failed = sends.find((send) => send.status === "rejected");
    if (failed) throw failed.reason;
    await client.waitForFinish(agent.id, 60000);
    await sleep(1000);
  } finally {
    await second.close();
  }
});
await sleep(1500);
console.log(JSON.stringify({ outcomes, workspaceId: created.workspace.id }));
const print = (label, frames) => {
  console.log(label);
  for (const text of frames) if (text !== HEARTBEAT_PONG) console.log(text);
};
print("# recording client", clientFrames());
print("# retry-other connection", otherFrames());
print("# race second connection", secondFrames());
await client.close();
process.exit(0);
