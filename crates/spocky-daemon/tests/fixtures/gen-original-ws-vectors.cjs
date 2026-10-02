// Regenerates original-ws-vectors.json: raw WebSocket exchanges with the pinned
// original daemon (Paseo 5de45e2 under Node 22.20.0, built by
// scripts/phase3/build-original.sh) on disposable loopback ports, never 6767
// or 6768. One daemon has no password, one has a bcrypt password hash for
// "secret"; dictation and voice mode are off in both. Same sandbox and process
// handling as gen-original-http-vectors.cjs; only processes this script started
// are signalled, by exact PID.
//
// Every byte is taken from raw sockets, below the client library's
// `handleJsonPayload`: the cases send frames a client library cannot produce
// (invalid hello, a second hello, oversized and non-UTF-8 frames), and record
// each server frame as its opcode and exact payload. Server-to-client frames
// are unmasked; client frames use an all-zero mask key.
// Usage: node gen-original-ws-vectors.cjs <original-build-root> > original-ws-vectors.json
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const net = require("node:net");
const http = require("node:http");
const { spawn } = require("node:child_process");

const root = process.argv[2];
const { provenance, claimPid, claimHolds } = require("./generator-support.cjs");
const bcrypt = require(path.join(root, "node_modules/bcryptjs"));
const PROFILE =
  '(version 1)(allow default)(deny network-outbound (remote ip "*:*"))(allow network-outbound (remote ip "localhost:*"))';
const FORBIDDEN = new Set([6767, 6768]);
const SERVER_ID = "srv_test";

async function freePort() {
  for (;;) {
    const port = await new Promise((resolve) => {
      const server = net.createServer();
      server.listen(0, "127.0.0.1", () => {
        const { port } = server.address();
        server.close(() => resolve(port));
      });
    });
    if (!FORBIDDEN.has(port)) return port;
  }
}

function claimWorker(handle) {
  // The worker named in the lock, claimed while the supervisor this script
  // started is alive (see generator-support.cjs).
  try {
    const pid = JSON.parse(fs.readFileSync(path.join(handle.home, "paseo.pid"), "utf8")).pid;
    return claimPid(pid, handle.child.pid);
  } catch {
    return null;
  }
}

function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

async function startDaemon(withPassword) {
  const port = await freePort();
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-http-vectors-"));
  const home = path.join(base, "paseo-home");
  const project = path.join(base, "project");
  fs.mkdirSync(home, { mode: 0o700 });
  fs.mkdirSync(project);
  fs.mkdirSync(path.join(base, "home"));
  fs.mkdirSync(path.join(base, "codex-home"));
  fs.writeFileSync(path.join(home, "server-id"), `${SERVER_ID}\n`, { mode: 0o600 });
  const daemon = { listen: `127.0.0.1:${port}`, relay: { enabled: false } };
  if (withPassword) daemon.auth = { password: bcrypt.hashSync("secret", 10) };
  fs.writeFileSync(
    path.join(home, "config.json"),
    JSON.stringify({
      daemon,
      features: { dictation: { enabled: false }, voiceMode: { enabled: false } },
    }),
    { mode: 0o600 },
  );
  const nodeBin = path.dirname(process.execPath);
  const child = spawn(
    "/usr/bin/sandbox-exec",
    ["-p", PROFILE, process.execPath, path.join(root, "packages/cli/dist/index.js"), "daemon", "run"],
    {
      cwd: project,
      env: {
        HOME: path.join(base, "home"),
        CODEX_HOME: path.join(base, "codex-home"),
        PASEO_HOME: home,
        PATH: `${nodeBin}:/usr/bin:/bin:/usr/sbin:/sbin`,
        TMPDIR: base,
        TZ: "UTC",
      },
      stdio: ["ignore", "ignore", "ignore"],
    },
  );
  const handle = { port, base, home, child, exited: false };
  child.on("exit", () => { handle.exited = true; });
  const deadline = Date.now() + 90000;
  for (;;) {
    if (handle.exited) throw new Error("the original daemon exited during startup");
    if (Date.now() > deadline) throw new Error("the original daemon did not come up");
    const ok = await new Promise((resolve) => {
      const request = http.get({ host: "127.0.0.1", port, path: "/api/health", headers: { Host: "localhost" } }, (res) => {
        res.resume();
        resolve(res.statusCode === 200 || res.statusCode === 401);
      });
      request.on("error", () => resolve(false));
    });
    if (ok) {
      handle.workerClaim = claimWorker(handle);
      return handle;
    }
    await new Promise((r) => setTimeout(r, 300));
  }
}

async function stopDaemon(handle) {
  if (!handle.exited) handle.child.kill("SIGTERM");
  const deadline = Date.now() + 15000;
  while (!handle.exited && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
  if (!handle.exited) handle.child.kill("SIGKILL");
  // The worker claimed at startup, if it is still that same process.
  const claim = handle.workerClaim;
  if (claim && claim.pid !== handle.child.pid && claimHolds(claim) && pidAlive(claim.pid)) {
    try { process.kill(claim.pid, "SIGTERM"); } catch {}
  }
  fs.rmSync(handle.base, { recursive: true, force: true });
}


const KEY = "dGhlIHNhbXBsZSBub25jZQ==";

function upgradeRequest(port, headers = {}, target = "/ws") {
  const all = {
    Host: "localhost",
    Upgrade: "websocket",
    Connection: "Upgrade",
    "Sec-WebSocket-Key": KEY,
    "Sec-WebSocket-Version": "13",
    ...headers,
  };
  return `GET ${target} HTTP/1.1\r\n` + Object.entries(all).filter(([, v]) => v !== null).map(([k, v]) => `${k}: ${v}\r\n`).join("") + "\r\n";
}

function encodeFrame(opcode, payload) {
  const body = Buffer.isBuffer(payload) ? payload : Buffer.from(payload, "utf8");
  let head;
  if (body.length < 126) head = Buffer.from([0x80 | opcode, 0x80 | body.length]);
  else if (body.length < 65536) {
    head = Buffer.alloc(4);
    head[0] = 0x80 | opcode; head[1] = 0x80 | 126; head.writeUInt16BE(body.length, 2);
  } else {
    head = Buffer.alloc(10);
    head[0] = 0x80 | opcode; head[1] = 0x80 | 127; head.writeBigUInt64BE(BigInt(body.length), 2);
  }
  return Buffer.concat([head, Buffer.alloc(4), body]);
}

class Conn {
  constructor(port) {
    this.events = [];
    this.buffer = Buffer.alloc(0);
    this.upgraded = false;
    this.closed = false;
    this.socket = net.connect(port, "127.0.0.1");
    this.socket.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
    // How the server ended the stream: a FIN is an `end` event, a reset an ECONNRESET error.
    this.end = null;
    this.socket.on("end", () => { this.end ??= "fin"; });
    this.socket.on("close", () => { this.closed = true; this.flushRest(); });
    this.socket.on("error", (error) => { if (error.code === "ECONNRESET") this.end ??= "rst"; });
    this.ready = new Promise((resolve) => this.socket.once("connect", resolve));
  }
  drain() {
    if (!this.upgraded) {
      const text = this.buffer.toString("latin1");
      const end = text.indexOf("\r\n\r\n");
      if (end < 0) return;
      const head = text.slice(0, end + 4);
      if (head.startsWith("HTTP/1.1 101")) {
        this.events.push({ t: "head", text: head });
        this.buffer = this.buffer.subarray(end + 4);
        this.upgraded = true;
      } else {
        // An aborted handshake: the whole response is the event, recorded when the socket closes.
        return;
      }
    }
    for (;;) {
      if (this.buffer.length < 2) return;
      const opcode = this.buffer[0] & 0x0f;
      let length = this.buffer[1] & 0x7f;
      let offset = 2;
      if (length === 126) { if (this.buffer.length < 4) return; length = this.buffer.readUInt16BE(2); offset = 4; }
      else if (length === 127) { if (this.buffer.length < 10) return; length = Number(this.buffer.readBigUInt64BE(2)); offset = 10; }
      if (this.buffer.length < offset + length) return;
      const payload = this.buffer.subarray(offset, offset + length);
      this.buffer = this.buffer.subarray(offset + length);
      if (opcode === 0x8) {
        this.events.push({ t: "close", code: payload.length >= 2 ? payload.readUInt16BE(0) : null, reason: payload.subarray(2).toString("utf8") });
      } else if (opcode === 0x1) {
        this.events.push({ t: "text", text: payload.toString("utf8") });
      } else if (opcode === 0x9 || opcode === 0xa) {
        this.events.push({ t: opcode === 0x9 ? "ping" : "pong", hex: payload.toString("hex") });
      } else {
        this.events.push({ t: "frame", opcode, hex: payload.toString("hex") });
      }
    }
  }
  flushRest() {
    if (!this.upgraded && this.buffer.length > 0) {
      this.events.push({ t: "http", text: this.buffer.toString("latin1") });
      this.buffer = Buffer.alloc(0);
    }
  }
  write(bytes) { this.socket.write(bytes); }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function run(port, steps, settle = 500) {
  const conns = new Map();
  for (const step of steps) {
    const [action, name, ...args] = step;
    if (action === "wait") { await sleep(name); continue; }
    if (action === "connect") {
      const conn = new Conn(port);
      await conn.ready;
      conns.set(name, conn);
      conn.write(upgradeRequest(port, args[0] ?? {}, args[1] ?? "/ws"));
      await sleep(250);
      continue;
    }
    const conn = conns.get(name);
    if (action === "text") conn.write(encodeFrame(0x1, args[0]));
    else if (action === "texts") conn.write(Buffer.concat(args[0].map((text) => encodeFrame(0x1, text))));
    else if (action === "binary") conn.write(encodeFrame(0x2, Buffer.from(args[0], "hex")));
    else if (action === "ping") conn.write(encodeFrame(0x9, Buffer.from(args[0], "hex")));
    else if (action === "close") {
      const payload = Buffer.alloc(2 + Buffer.byteLength(args[1]));
      payload.writeUInt16BE(args[0], 0);
      payload.write(args[1], 2);
      conn.write(encodeFrame(0x8, payload));
    } else if (action === "raw") conn.write(Buffer.from(args[0], "hex"));
    else if (action === "destroy") conn.socket.destroy();
    else if (action === "await_close") {
      const deadline = Date.now() + args[0];
      while (!conn.closed && Date.now() < deadline) await sleep(50);
    }
    await sleep(250);
  }
  await sleep(settle);
  const events = {};
  for (const [name, conn] of conns) {
    // Whether the socket was closed before this script closed it.
    const closed = conn.closed;
    const end = conn.end;
    if (!closed) conn.socket.destroy();
    await sleep(20);
    events[name] = { closed, end, frames: conn.events.filter((event) => event.t !== "closed") };
  }
  return events;
}

const hello = (clientId, extra = {}) => JSON.stringify({ type: "hello", clientId, clientType: "cli", protocolVersion: 1, ...extra });
const oversizedHeader = (() => {
  const header = Buffer.alloc(14);
  header[0] = 0x81; header[1] = 0x80 | 127;
  header.writeBigUInt64BE(100n * 1024n * 1024n + 1n, 2);
  return header.toString("hex");
})();

// The original checks a password with an asynchronous bcrypt compare and holds
// `pending.authenticating` for its whole length, so every frame that reaches it
// meanwhile is answered as a message before hello. The "in one chunk" cases show
// that; the "sent later" case waits for the compare to finish first.
function cases() {
  const open = [
    // Frames written in the same chunk as the hello arrive while the hello is
    // still being judged (websocket-server.ts sets pending.authenticating before
    // the awaited admission), so the original rejects them.
    ["hello_and_ping_in_one_chunk", [["connect", "a"], ["texts", "a", [hello("c1"), '{"type":"ping"}']]]],
    ["hello_and_second_hello_in_one_chunk", [["connect", "a"], ["texts", "a", [hello("c1"), hello("c1")]]]],
    ["hello_and_recording_state_in_one_chunk", [["connect", "a"], ["texts", "a", [hello("c1"), '{"type":"recording_state","isRecording":true}']]]],
    ["hello_then_ping_pong_and_recording_state", [["connect", "a"], ["text", "a", hello("c1")], ["text", "a", '{"type":"ping"}'], ["text", "a", '{"type":"recording_state","isRecording":true}'], ["text", "a", '{"type":"ping"}']]],
    ["hello_with_app_version_and_capabilities", [["connect", "a"], ["text", "a", hello("c1", { appVersion: "1.2.3", capabilities: { hello_rejection: true } })]]],
    ["ping_before_hello", [["connect", "a"], ["text", "a", '{"type":"ping"}']]],
    ["recording_state_before_hello", [["connect", "a"], ["text", "a", '{"type":"recording_state","isRecording":false}']]],
    ["valid_session_before_hello", [["connect", "a"], ["text", "a", '{"type":"session","message":{"type":"fetch_agents_request","requestId":"r1"}}']]],
    ["invalid_session_before_hello", [["connect", "a"], ["text", "a", '{"type":"session","message":{"type":"fetch_agents_request"}}']]],
    ["invalid_json_before_hello", [["connect", "a"], ["text", "a", "{not json"]]],
    ["unknown_type_before_hello", [["connect", "a"], ["text", "a", '{"type":"nope"}']]],
    ["not_an_object_before_hello", [["connect", "a"], ["text", "a", "5"]]],
    ["binary_frame_before_hello", [["connect", "a"], ["binary", "a", "00ff"]]],
    ["hello_missing_fields", [["connect", "a"], ["text", "a", '{"type":"hello"}']]],
    ["hello_empty_client_id", [["connect", "a"], ["text", "a", hello("")]]],
    ["hello_bad_client_type", [["connect", "a"], ["text", "a", '{"type":"hello","clientId":"c","clientType":"toaster","protocolVersion":1}']]],
    ["hello_protocol_version_2", [["connect", "a"], ["text", "a", hello("c", { protocolVersion: 2 })]]],
    ["hello_protocol_version_2_with_rejection_capability", [["connect", "a"], ["text", "a", hello("c", { protocolVersion: 2, capabilities: { hello_rejection: true } })]]],
    ["hello_protocol_version_fraction", [["connect", "a"], ["text", "a", hello("c", { protocolVersion: 1.5 })]]],
    ["hello_plugin_client_id", [["connect", "a"], ["text", "a", hello("plugin:thing")]]],
    ["hello_client_id_with_spaces", [["connect", "a"], ["text", "a", hello("  padded  ")]]],
    ["second_hello_on_active_socket", [["connect", "a"], ["text", "a", hello("c1")], ["text", "a", hello("c1")]]],
    ["invalid_json_after_hello", [["connect", "a"], ["text", "a", hello("c1")], ["text", "a", "{not json"], ["text", "a", '{"type":"ping"}']]],
    ["unknown_type_after_hello", [["connect", "a"], ["text", "a", hello("c1")], ["text", "a", '{"type":"nope"}'], ["text", "a", '{"type":"ping"}']]],
    // The rest of an oversized frame and later frames are drained, not answered with a reset.
    ["oversized_frame_with_body_bytes", [["connect", "a"], ["text", "a", hello("c1")], ["raw", "a", oversizedHeader + "00000000" + "00".repeat(8192)], ["text", "a", '{"type":"ping"}']]],
    ["invalid_utf8_then_more_frames", [["connect", "a"], ["text", "a", hello("c1")], ["raw", "a", "8182" + "00000000" + "c328"], ["text", "a", '{"type":"ping"}'], ["text", "a", '{"type":"ping"}']]],
    ["oversized_frame_before_hello", [["connect", "a"], ["raw", "a", oversizedHeader + "00000000"]]],
    ["oversized_frame_after_hello", [["connect", "a"], ["text", "a", hello("c1")], ["raw", "a", oversizedHeader + "00000000"]]],
    ["invalid_utf8_after_hello", [["connect", "a"], ["text", "a", hello("c1")], ["raw", "a", "8182" + "00000000" + "c328"]]],
    ["client_close_frame_after_hello", [["connect", "a"], ["text", "a", hello("c1")], ["close", "a", 1000, "bye"], ["await_close", "a", 2000]]],
    ["client_close_frame_before_hello", [["connect", "a"], ["close", "a", 1001, ""], ["await_close", "a", 2000]]],
    ["client_ping_control_frame", [["connect", "a"], ["ping", "a", "aabb"], ["text", "a", hello("c1")], ["ping", "a", "ccdd"]]],
    ["reconnect_within_grace", [["connect", "a"], ["text", "a", hello("c1")], ["destroy", "a"], ["wait", 400], ["connect", "b"], ["text", "b", hello("c1")], ["text", "b", '{"type":"ping"}']]],
    ["second_socket_same_client_while_first_is_open", [["connect", "a"], ["text", "a", hello("c1")], ["connect", "b"], ["text", "b", hello("c1")], ["text", "b", '{"type":"ping"}'], ["text", "a", '{"type":"ping"}']]],
    ["two_clients_independent", [["connect", "a"], ["text", "a", hello("c1")], ["connect", "b"], ["text", "b", hello("c2")], ["text", "a", '{"type":"ping"}'], ["text", "b", '{"type":"ping"}']]],
    ["hello_timeout", [["connect", "a"], ["await_close", "a", 20000]]],
    ["upgrade_wrong_path", [["connect", "a", {}, "/other"]]],
    ["upgrade_missing_key", [["connect", "a", { "Sec-WebSocket-Key": null }]]],
    ["upgrade_bad_version", [["connect", "a", { "Sec-WebSocket-Version": "8" }]]],
    ["upgrade_disallowed_host", [["connect", "a", { Host: "evil.example" }]]],
    ["upgrade_foreign_origin", [["connect", "a", { Origin: "http://evil.example" }]]],
    ["upgrade_with_subprotocol", [["connect", "a", { "Sec-WebSocket-Protocol": "chat, other" }], ["text", "a", hello("c1")]]],
  ];
  const secured = [
    ["hello_without_auth", [["connect", "a"], ["text", "a", hello("c1")]]],
    ["hello_without_auth_with_rejection_capability", [["connect", "a"], ["text", "a", hello("c1", { capabilities: { hello_rejection: true } })]]],
    ["hello_wrong_password", [["connect", "a"], ["text", "a", hello("c1", { auth: { kind: "password", password: "wrong" } })]]],
    // A ping in the same chunk as the hello is rejected while the bcrypt compare runs.
    ["hello_right_password_and_ping_in_one_chunk", [["connect", "a"], ["texts", "a", [hello("c1", { auth: { kind: "password", password: "secret" } }), '{"type":"ping"}']]]],
    ["hello_right_password_and_ping_sent_later", [["connect", "a"], ["text", "a", hello("c1", { auth: { kind: "password", password: "secret" } })], ["wait", 1500], ["text", "a", '{"type":"ping"}']]],
    // A wrong password with a frame in the same chunk: the frame closes the pending
    // connection first, and the later rejection is suppressed, so nothing but the
    // close is sent (no hello.rejected frame).
    ["hello_wrong_password_and_ping_in_one_chunk", [["connect", "a"], ["texts", "a", [hello("c1", { auth: { kind: "password", password: "wrong" } }), '{"type":"ping"}']]]],
    ["hello_local_credential_kind_without_credential", [["connect", "a"], ["text", "a", hello("c1", { auth: { kind: "localCredential", token: "x".repeat(43) } })]]],
    ["hello_bad_auth_shape", [["connect", "a"], ["text", "a", hello("c1", { auth: { kind: "magic" } })]]],
    ["protocol_mismatch_without_auth", [["connect", "a"], ["text", "a", hello("c1", { protocolVersion: 2 })]]],
    ["bearer_subprotocol_right", [["connect", "a", { "Sec-WebSocket-Protocol": "paseo.bearer.secret" }], ["text", "a", hello("c1")], ["text", "a", '{"type":"ping"}']]],
    // A credential on the upgrade request admits the hello without awaiting anything,
    // so a frame in the same chunk is not rejected.
    ["bearer_subprotocol_hello_and_ping_in_one_chunk", [["connect", "a", { "Sec-WebSocket-Protocol": "paseo.bearer.secret" }], ["texts", "a", [hello("c1"), '{"type":"ping"}']]]],
    ["bearer_subprotocol_wrong", [["connect", "a", { "Sec-WebSocket-Protocol": "paseo.bearer.wrong" }], ["text", "a", hello("c1")]]],
    ["authorization_header_right", [["connect", "a", { Authorization: "Bearer secret" }], ["text", "a", hello("c1")]]],
    ["authorization_header_wrong", [["connect", "a", { Authorization: "Bearer wrong" }], ["text", "a", hello("c1")]]],
    ["ping_before_hello_secured", [["connect", "a"], ["text", "a", '{"type":"ping"}']]],
  ];
  return { open, secured };
}

async function main() {
  const open = await startDaemon(false);
  let secured = null;
  try {
    secured = await startDaemon(true);
    // The socket listener opens a moment after the HTTP one: wait until a hello is served.
    for (const daemon of [open, secured]) {
      for (let i = 0; i < 100; i++) {
        const conn = new Conn(daemon.port);
        await conn.ready;
        conn.write(upgradeRequest(daemon.port));
        await sleep(300);
        const ready = conn.upgraded;
        conn.socket.destroy();
        if (ready) break;
      }
    }
    const status = await new Promise((resolve) => {
      http.get({ host: "127.0.0.1", port: open.port, path: "/api/status", headers: { Host: "localhost" } }, (res) => {
        let body = ""; res.on("data", (c) => (body += c)); res.on("end", () => resolve(JSON.parse(body)));
      });
    });
    const { open: openCases, secured: securedCases } = cases();
    const out = [];
    for (const [name, steps] of openCases) out.push({ name, secure: false, steps, events: await run(open.port, steps) });
    // The bcrypt compare of a password hello can take well over a second under load.
    for (const [name, steps] of securedCases) out.push({ name, secure: true, steps, events: await run(secured.port, steps, 3000) });
    process.stdout.write(JSON.stringify({
      node: process.version,
      original: path.basename(root),
      provenance: provenance(root),
      serverId: status.serverId,
      hostname: status.hostname,
      version: status.version,
      cases: out,
    }, null, 1));
  } finally {
    await stopDaemon(open);
    if (secured) await stopDaemon(secured);
  }
  process.exit(0);
}
main().catch((error) => { console.error(error); process.exit(1); });
