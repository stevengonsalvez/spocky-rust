// Replays one recorded `codex app-server` stdio session (from
// tests/fixtures/g2_approvals.json) to whichever client launched it, and logs
// every line the client sends, so the Rust provider and the pinned Paseo
// client can be fed identical Codex output and their output compared.
//
// argv: <fixture> <scenario> <root> <client log> [app-server args ignored]
//
// The recorded server output is replayed in order. A recorded response waits
// for the client request of the same method (queued, so concurrent requests
// match in any order) and takes that request's id. After a recorded server
// request, the replay waits for the client's response only when the client's
// next recorded line was that response; when the client went on with a
// request instead (`turn/interrupt` while an approval is pending), the next
// recorded response already waits for it. Notifications are sent as soon as
// everything before them was. A client request the recording
// cannot answer gets a JSON-RPC error at once, so no client hangs on it.
import { appendFileSync, readFileSync } from "node:fs";
import { createInterface } from "node:readline";

const [fixturePath, scenario, root, clientLog] = process.argv.slice(2);
const recorded = JSON.parse(readFileSync(fixturePath, "utf8")).scenarios[scenario];
if (!recorded) throw new Error(`no recorded scenario ${scenario}`);
const fill = (line) => line.split("{root}").join(root);

const clientLines = recorded.in.map((line) => JSON.parse(fill(line)));
const requestIndex = new Map();
clientLines.forEach((message, index) => {
  if (message.method !== undefined && message.id !== undefined) requestIndex.set(message.id, index);
});

// The client line the recording shows right after `index`, skipping its
// notifications.
function nextClientLine(index) {
  return clientLines.slice(index + 1).find((message) => message.id !== undefined);
}

let lastAnswered = -1;
const steps = recorded.out.map((raw) => {
  const line = fill(raw);
  const message = JSON.parse(line);
  if (message.method === undefined) {
    const index = requestIndex.get(message.id);
    lastAnswered = Math.max(lastAnswered, index);
    return { kind: "respond", method: clientLines[index].method, line, message };
  }
  if (message.id !== undefined) {
    const next = nextClientLine(lastAnswered);
    const awaits = next !== undefined && next.method === undefined && next.id === message.id;
    return { kind: "request", line, id: message.id, awaits };
  }
  return { kind: "notify", line };
});

const queued = [];
const answered = new Set();
let next = 0;
let awaiting = null;

const write = (line) => process.stdout.write(`${line}\n`);

function stillExpected(method) {
  return steps.slice(next).some((step) => step.kind === "respond" && step.method === method);
}

function pump() {
  while (next < steps.length) {
    if (awaiting !== null) {
      if (!answered.has(awaiting)) return;
      awaiting = null;
    }
    const step = steps[next];
    if (step.kind === "respond") {
      const index = queued.findIndex((request) => request.method === step.method);
      if (index < 0) return;
      const [request] = queued.splice(index, 1);
      write(
        request.id === step.message.id
          ? step.line
          : JSON.stringify({ ...step.message, id: request.id }),
      );
    } else {
      write(step.line);
      if (step.kind === "request" && step.awaits) awaiting = step.id;
    }
    next += 1;
  }
}

function reject(request) {
  write(
    JSON.stringify({
      id: request.id,
      error: { code: -32601, message: `not in the recording: ${request.method}` },
    }),
  );
}

createInterface({ input: process.stdin })
  .on("line", (line) => {
    appendFileSync(clientLog, `${line}\n`);
    const message = JSON.parse(line);
    if (message.method !== undefined && message.id !== undefined) {
      if (stillExpected(message.method)) queued.push(message);
      else reject(message);
    } else if (message.method === undefined) {
      answered.add(message.id);
    }
    pump();
  })
  .on("close", () => process.exit(0));

pump();
