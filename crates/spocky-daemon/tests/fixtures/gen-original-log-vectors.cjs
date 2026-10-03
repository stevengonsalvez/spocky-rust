// Regenerates original-log-vectors.json: what the pinned original daemon (Paseo
// 5de45e2 under Node 22.20.0, built by scripts/phase3/build-original.sh) writes
// when it cannot listen: the fatal record "Daemon failed to start listening"
// from daemon-worker.ts, as one raw line of the worker's stdout, and the worker's
// stderr, which carries the stack of the error that rejected daemon.start().
// Scenarios: the port is taken (EADDRINUSE), the address is not one of the
// host's (EADDRNOTAVAIL), the port is out of range (RangeError), the host does
// not resolve (ENOTFOUND) and a unix socket cannot be created (EACCES). In a
// listen string "PORT" stands for the port the run chose. Disposable homes and
// loopback ports, never 6767 or 6768; same sandbox as
// gen-original-home-vectors.cjs. Only processes this script started are
// signalled, by exact PID.
// Usage: node gen-original-log-vectors.cjs <original-build-root> > original-log-vectors.json
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const net = require("node:net");
const { spawn } = require("node:child_process");

const root = process.argv[2];
const { provenance } = require("./generator-support.cjs");
const PROFILE =
  '(version 1)(allow default)(deny network-outbound (remote ip "*:*"))(allow network-outbound (remote ip "localhost:*"))';
const FORBIDDEN = new Set([6767, 6768]);

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

async function runFailing(listenTemplate, port) {
  const listen = listenTemplate.replace("PORT", String(port));
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-log-vectors-"));
  const home = path.join(base, "paseo-home");
  fs.mkdirSync(home, { mode: 0o700 });
  for (const dir of ["home", "codex-home", "project"]) fs.mkdirSync(path.join(base, dir));
  fs.writeFileSync(
    path.join(home, "config.json"),
    JSON.stringify({
      daemon: { listen, relay: { enabled: false } },
      features: { dictation: { enabled: false }, voiceMode: { enabled: false } },
    }),
    { mode: 0o600 },
  );
  const nodeBin = path.dirname(process.execPath);
  const child = spawn(
    "/usr/bin/sandbox-exec",
    ["-p", PROFILE, process.execPath, path.join(root, "packages/cli/dist/index.js"), "daemon", "run"],
    {
      cwd: path.join(base, "project"),
      env: {
        HOME: path.join(base, "home"),
        CODEX_HOME: path.join(base, "codex-home"),
        PASEO_HOME: home,
        PATH: `${nodeBin}:/usr/bin:/bin:/usr/sbin:/sbin`,
        TMPDIR: base,
        TZ: "UTC",
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const watchdog = setTimeout(() => child.kill("SIGTERM"), 90000);
  const [code, signal] = await new Promise((resolve) => child.on("exit", (c, s) => resolve([c, s])));
  clearTimeout(watchdog);
  fs.rmSync(base, { recursive: true, force: true });
  const lines = stdout.split("\n").filter((line) => line.startsWith("{"));
  const fatal = lines.filter((line) => JSON.parse(line).level === 60);
  if (fatal.length !== 1) throw new Error(`expected one fatal record, got ${fatal.length}`);
  // The worker's own stderr is everything before the supervisor's first line.
  const workerStderr = stderr.split("[DaemonRunner]")[0];
  return { listen: listenTemplate, port, code, signal, fatalLine: fatal[0], workerStderr };
}

async function main() {
  const holder = net.createServer();
  await new Promise((resolve) => holder.listen(0, "127.0.0.1", resolve));
  const taken = holder.address().port;
  if (FORBIDDEN.has(taken)) throw new Error("holder landed on a forbidden port");
  const cases = [];
  cases.push({ name: "port_in_use", ...(await runFailing("127.0.0.1:PORT", taken)) });
  holder.close();
  cases.push({ name: "address_not_available", ...(await runFailing("192.0.2.1:PORT", await freePort())) });
  cases.push({ name: "port_too_large", ...(await runFailing("127.0.0.1:70000", 70000)) });
  cases.push({ name: "port_negative", ...(await runFailing("127.0.0.1:-1", -1)) });
  cases.push({ name: "host_not_found", ...(await runFailing("nonexistent.invalid:PORT", await freePort())) });
  cases.push({ name: "socket_in_missing_directory", ...(await runFailing("unix:///nonexistent-dir-spocky/x.sock", null)) });
  process.stdout.write(JSON.stringify({ node: process.version, original: path.basename(root), provenance: provenance(root), cases }, null, 1));
  process.exit(0);
}
main().catch((error) => { console.error(error); process.exit(1); });
