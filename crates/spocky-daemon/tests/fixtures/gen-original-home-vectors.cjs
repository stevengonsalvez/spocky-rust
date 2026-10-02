// Regenerates original-home-vectors.json: the files the pinned original daemon
// (Paseo 5de45e2 under Node 22.20.0, built by scripts/phase3/build-original.sh)
// leaves in a disposable PASEO_HOME after a graceful stop (SIGTERM) and after
// a SIGKILL, on a disposable loopback port, never 6767 or 6768. Same sandbox
// and process handling as gen-original-http-vectors.cjs; only processes this
// script started are signalled, by exact PID.
// Usage: node gen-original-home-vectors.cjs <original-build-root> > original-home-vectors.json
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



function snapshot(home) {
  const files = {};
  for (const name of fs.readdirSync(home).sort()) {
    if (name === "daemon.log") continue; // timestamps and pids, not state
    const file = path.join(home, name);
    const stat = fs.statSync(file);
    if (!stat.isFile()) { files[name] = { directory: true }; continue; }
    files[name] = { mode: (stat.mode & 0o777).toString(8), text: fs.readFileSync(file, "utf8") };
  }
  return files;
}

async function untilFile(file) {
  const deadline = Date.now() + 30000;
  while (!fs.existsSync(file)) {
    if (Date.now() > deadline) throw new Error(`${file} never appeared`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

async function scenario(kill) {
  const handle = await startDaemon(false);
  // Keep the home: take over the cleanup from stopDaemon.
  await untilFile(path.join(handle.home, "paseo.pid"));
  // The lock is rewritten with the bound address once the daemon listens.
  for (let i = 0; i < 100; i++) {
    const lock = JSON.parse(fs.readFileSync(path.join(handle.home, "paseo.pid"), "utf8"));
    if (lock.listen !== null && lock.serverId !== undefined) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  const lockPid = JSON.parse(fs.readFileSync(path.join(handle.home, "paseo.pid"), "utf8")).pid;
  // Claimed while the supervisor is alive, so a PID reused later is never signalled.
  const claim = claimWorker(handle);
  const listening = snapshot(handle.home);
  if (kill) {
    // The supervisor child and the worker it named in the lock, both started by this run.
    handle.child.kill("SIGKILL");
    if (claim && claim.pid !== handle.child.pid && claimHolds(claim) && pidAlive(claim.pid)) {
      process.kill(claim.pid, "SIGKILL");
    }
  } else {
    handle.child.kill("SIGTERM");
  }
  const deadline = Date.now() + 20000;
  while (!handle.exited && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
  await new Promise((r) => setTimeout(r, 500));
  const after = snapshot(handle.home);
  const result = { listening, after, lockPid, port: handle.port };
  fs.rmSync(handle.base, { recursive: true, force: true });
  return result;
}

async function main() {
  const graceful = await scenario(false);
  const killed = await scenario(true);
  process.stdout.write(JSON.stringify({ node: process.version, original: path.basename(root), provenance: provenance(root), graceful, killed }, null, 1));
  process.exit(0);
}
main().catch((error) => { console.error(error); process.exit(1); });
