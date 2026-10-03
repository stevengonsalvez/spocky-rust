// G4 socket-level probe, run by the slice harness under the egress sandbox
// with the pinned node. It speaks the raw WebSocket protocol to the daemon, so
// it covers what the pinned CLI never sends after the hello: invalid and
// unknown requests, the requestId echo, and binary frames.
//
// argv: <paseoRoot> --host <host:port>
//
// Every inbound frame is recorded and printed, from the first one: the
// server_info status that answers the hello goes under the "hello" step, and
// anything the daemon sends on its own in the 500 ms after it goes under
// "idle". Per-run ids and instants in them are masked by the harness's
// existing generated_id and wall_clock classes, never dropped here.
// After the hello is accepted it sends, one at a time, each waiting for its
// reply and then settling:
//   valid        a fetch_workspaces_request; the reply must echo its requestId
//   invalid      a JSON object with no known type but a requestId (invalid_message, echoed)
//   unknown      a session-wrapped request of an unknown type (unknown_schema, echoed)
//   text         text that is not JSON (invalid_message, no requestId)
//   binary-junk  a binary frame whose first byte is no opcode (invalid_message)
//   binary-short a one-byte terminal-input opcode frame, too short to decode (invalid_message)
//   binary-frame a decodable terminal input frame for slot 0, routed with no reply
//   ping         a ping; the pong flushes anything still in flight
//
// stdout line 1 is a JSON summary of what each step produced; every other line
// is either {"step": "<name>"} or one raw inbound text frame, in arrival
// order, unmodified, so key order is preserved for the wire comparison.
// idleFrames counts the frames the daemon sent unprompted after the hello.
const [, , , hostFlag, host] = process.argv;
if (hostFlag !== "--host" || !host) {
  console.error("usage: g4-wire.mjs <paseoRoot> --host <host:port>");
  process.exit(2);
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const ws = new WebSocket(`ws://${host}/ws`);
ws.binaryType = "arraybuffer";
const frames = [];
let closed = null;
ws.onmessage = (event) => {
  frames.push(typeof event.data === "string" ? event.data : Buffer.from(event.data).toString("latin1"));
};
ws.onclose = (event) => {
  closed = { code: event.code, reason: event.reason };
};
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = () => reject(new Error("websocket error before open"));
  setTimeout(() => reject(new Error("websocket did not open in 15 s")), 15000);
});

const parse = (text) => {
  try {
    const frame = JSON.parse(text);
    return frame.type === "session" ? frame.message : frame;
  } catch {
    return null;
  }
};
const waitFor = async (predicate, ms) => {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (predicate()) return true;
    await sleep(25);
  }
  return predicate();
};

ws.send(JSON.stringify({ type: "hello", clientId: "g4-wire-probe", clientType: "cli", protocolVersion: 1 }));
const hello = await waitFor(
  () => frames.some((text) => parse(text)?.payload?.status === "server_info"),
  15000,
);
if (!hello) {
  console.error("no server_info after hello");
  process.exit(3);
}
const helloEnd = frames.findIndex((text) => parse(text)?.payload?.status === "server_info") + 1;
await sleep(500);

const steps = [];
const run = async (name, send, expect) => {
  const before = frames.length;
  send();
  if (expect > 0) await waitFor(() => frames.length >= before + expect, 8000);
  await sleep(400);
  steps.push({ name, replies: frames.slice(before) });
};
// Everything up to and including server_info, then whatever the daemon sent
// on its own in the 500 ms after it, before anything is sent.
steps.push({ name: "hello", replies: frames.slice(0, helloEnd) });
steps.push({ name: "idle", replies: frames.slice(helloEnd) });

await run("valid", () => ws.send(JSON.stringify({ type: "session", message: { type: "fetch_workspaces_request", requestId: "g4-valid-1" } })), 1);
await run("invalid", () => ws.send(JSON.stringify({ type: "nope", requestId: "g4-invalid-1" })), 1);
await run("unknown", () => ws.send(JSON.stringify({ type: "session", message: { type: "no_such_request", requestId: "g4-unknown-1" } })), 1);
await run("text", () => ws.send("not json"), 1);
await run("binary-junk", () => ws.send(new Uint8Array([0xff, 0x01, 0x02, 0x03])), 1);
await run("binary-short", () => ws.send(new Uint8Array([0x02])), 1);
await run("binary-frame", () => ws.send(new Uint8Array([0x02, 0x00, 0x61])), 0);
await run("ping", () => ws.send(JSON.stringify({ type: "ping" })), 1);

const messages = (name) => steps.find((step) => step.name === name).replies.map(parse);
const only = (name) => {
  const list = messages(name);
  return list.length === 1 ? list[0] : null;
};
const rpcError = (message, id, code) =>
  message?.type === "rpc_error" && message.payload?.requestId === id && message.payload?.code === code;
const statusError = (message) =>
  message?.type === "status" && message.payload?.status === "error" && message.payload?.requestId === undefined;
const summary = {
  validEchoed:
    only("valid")?.type === "fetch_workspaces_response" && only("valid").payload?.requestId === "g4-valid-1",
  invalidEchoed: rpcError(only("invalid"), "g4-invalid-1", "invalid_message"),
  unknownEchoed: rpcError(only("unknown"), "g4-unknown-1", "unknown_schema"),
  textRejected: statusError(only("text")),
  binaryJunkRejected: statusError(only("binary-junk")),
  binaryShortRejected: statusError(only("binary-short")),
  binaryFrameSilent: messages("binary-frame").length === 0,
  pong: only("ping")?.type === "pong",
  idleFrames: messages("idle").length,
  closed: closed !== null,
};
console.log(JSON.stringify(summary));
for (const step of steps) {
  console.log(JSON.stringify({ step: step.name }));
  for (const text of step.replies) console.log(text);
}
ws.close(1000);
await sleep(300);
process.exit(0);
