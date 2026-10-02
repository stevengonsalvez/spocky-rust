// Regenerates original-hello-vectors.json: the frames the pinned original daemon
// answers a hello with (Paseo 5de45e2 under Node 22.20.0 on a disposable
// loopback port, never 6767 or 6768; same sandbox and process handling as
// gen-original-http-vectors.cjs). Usage:
// node gen-original-hello-vectors.cjs <original-build-root> > original-hello-vectors.json
//
// The daemon runs from a disposable PASEO_HOME with dictation and voice mode
// disabled in config, outbound network is denied by sandbox-exec, and only the
// processes this script started are signalled, by exact PID.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const net = require("node:net");
const http = require("node:http");
const { spawn } = require("node:child_process");

const root = process.argv[2];
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
    if (ok) return handle;
    await new Promise((r) => setTimeout(r, 300));
  }
}

async function stopDaemon(handle) {
  let lockPid = null;
  try { lockPid = JSON.parse(fs.readFileSync(path.join(handle.home, "paseo.pid"), "utf8")).pid; } catch {}
  if (!handle.exited) handle.child.kill("SIGTERM");
  const deadline = Date.now() + 15000;
  while (!handle.exited && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
  if (!handle.exited) handle.child.kill("SIGKILL");
  // The daemon worker named in the lock is a child of the process started above.
  if (lockPid && lockPid !== handle.child.pid && pidAlive(lockPid)) {
    try { process.kill(lockPid, "SIGTERM"); } catch {}
  }
  fs.rmSync(handle.base, { recursive: true, force: true });
}


const WebSocket = require(path.join(root, "node_modules/ws"));

async function helloFrames(port, hello, waitMs) {
  for (let attempt = 0; attempt < 100; attempt++) {
    try { return await helloOnce(port, hello, waitMs); }
    catch (error) {
      if (!String(error.message).includes("503")) throw error;
      await new Promise((r) => setTimeout(r, 300));
    }
  }
  throw new Error("the original daemon never accepted sockets");
}

async function helloOnce(port, hello, waitMs) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`, { headers: { Host: "localhost" } });
    const frames = [];
    ws.on("open", () => ws.send(JSON.stringify(hello)));
    ws.on("message", (data) => frames.push(data.toString()));
    ws.on("close", (code, reason) => resolve({ frames, close: [code, reason.toString()] }));
    ws.on("error", reject);
    setTimeout(() => { ws.close(); }, waitMs);
  });
}

async function main() {
  const daemon = await startDaemon(false);
  try {
    const out = [];
    for (const [name, hello] of [
      ["plain_hello", { type: "hello", clientId: "probe-1", clientType: "cli", protocolVersion: 1 }],
    ]) {
      out.push({ name, hello, ...(await helloFrames(daemon.port, hello, 4000)) });
    }
    process.stdout.write(JSON.stringify({ node: process.version, original: path.basename(root), port: daemon.port, results: out }, null, 1));
  } finally {
    await stopDaemon(daemon);
  }
  process.exit(0);
}
main().catch((error) => { console.error(error); process.exit(1); });
