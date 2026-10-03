// G4 disconnect probe, run by the slice harness under the egress sandbox with
// the pinned node. Client A runs in a forked child, creates an agent whose
// turn the stub holds open and starts waiting for it; the parent SIGKILLs the
// child 1.5 s in, so the OS drops A's TCP connection with no WebSocket close
// handshake. Client B then connects, fetches the agent and cancels the held
// turn.
//
// argv: <paseoRoot> --host <host:port> <project> <holdPrompt> [client-a]
//
// stdout line 1 is {"dropped": "<signal that ended A>", "status": "<status B
// fetched>", "errorFrames": <n>, "workspaceId": "<id>"}; the remaining lines
// are every frame client B received, pongs and the server_info status
// included, as raw wire text in arrival order. Per-run ids and instants in
// them are masked by the harness's existing generated_id and wall_clock
// classes, never dropped here. Wire text is taken by hooking
// DaemonClient.prototype.handleJsonPayload, so key order and unknown keys are
// preserved.
//
// What the first line proves, and what it does not: B connects only after A
// is gone, so the daemon has no way to send B an error about A's drop, and
// dropped only shows that the probe killed its own child. errorFrames and
// dropped are sanity checks of the probe itself. The proof is B's raw wire,
// compared across daemons: the agent is still running after A's abrupt drop,
// the cancel works, and B's frames match byte for byte.
import { fork } from "node:child_process";
import { fileURLToPath } from "node:url";

const [, , paseoRoot, hostFlag, host, project, holdPrompt, role] = process.argv;
if (hostFlag !== "--host" || !host || !project || !holdPrompt) {
  console.error("usage: g4-subscriber.mjs <paseoRoot> --host <host> <project> <holdPrompt>");
  process.exit(2);
}
const { connectToDaemon } = await import(`${paseoRoot}/packages/cli/dist/utils/client.js`);
const target = { kind: "endpoint", host };
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

if (role === "client-a") {
  const a = await connectToDaemon({ target });
  const created = await a.createWorkspace({ source: { kind: "directory", path: project } });
  const agent = await a.createAgent({
    provider: "codex",
    cwd: project,
    workspaceId: created.workspace.id,
    modeId: "full-access",
    initialPrompt: holdPrompt,
  });
  a.waitForFinish(agent.id, 120000).catch(() => {});
  process.send({ workspaceId: created.workspace.id, agentId: agent.id });
  setInterval(() => {}, 1000);
  await new Promise(() => {});
}

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

const child = fork(fileURLToPath(import.meta.url), [paseoRoot, "--host", host, project, holdPrompt, "client-a"], {
  stdio: ["ignore", "ignore", "inherit", "ipc"],
});
const exited = new Promise((resolve) => child.once("exit", (code, signal) => resolve(signal ?? `exit ${code}`)));
const info = await new Promise((resolve, reject) => {
  child.once("message", resolve);
  exited.then((how) => reject(new Error(`client A ended before its agent was created: ${how}`)));
  setTimeout(() => reject(new Error("client A did not create its agent in 60 s")), 60000);
});
await sleep(1500);
child.kill("SIGKILL");
const dropped = await exited;
await sleep(500);

const b = await connectToDaemon({ target });
const fetched = await b.fetchAgent({ agentId: info.agentId });
await b.cancelAgent(info.agentId);
await sleep(1500);

const frames = raw.get(b).map((text) => {
  const frame = JSON.parse(text);
  return { text, message: frame.type === "session" ? frame.message : frame };
});
const errorFrames = frames.filter(
  ({ message }) => message.type === "rpc_error" || (message.payload?.error ?? null) !== null,
).length;
console.log(
  JSON.stringify({ dropped, status: fetched?.agent?.status, errorFrames, workspaceId: info.workspaceId }),
);
for (const { text } of frames) console.log(text);
await b.close();
process.exit(0);
