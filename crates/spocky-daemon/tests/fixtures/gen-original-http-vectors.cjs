// Regenerates original-http-vectors.json: raw HTTP exchanges with the pinned
// original daemon (Paseo 5de45e2, `packages/cli/dist/index.js daemon run`, built
// by scripts/phase3/build-original.sh) running under Node 22.20.0 on a
// disposable loopback port, never 6767 or 6768. One daemon has no password,
// one has a bcrypt password hash for "secret". Each daemon runs from its own
// disposable PASEO_HOME, outbound network is denied by sandbox-exec, and only
// the processes this script started are signalled, by exact PID.
//
// Usage: node gen-original-http-vectors.cjs <original-build-root> > original-http-vectors.json
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

const req = (method, target, headers = {}, version = "1.1", body = "") =>
  `${method} ${target} HTTP/${version}\r\n` +
  Object.entries({ Host: "localhost", ...headers }).map(([k, v]) => `${k}: ${v}\r\n`).join("") +
  "\r\n" + body;

async function exchange(port, requests) {
  const socket = net.connect(port, "127.0.0.1");
  let data = Buffer.alloc(0);
  socket.on("data", (chunk) => { data = Buffer.concat([data, chunk]); });
  await new Promise((resolve) => socket.once("connect", resolve));
  let closed = false;
  socket.on("close", () => { closed = true; });
  const responses = [];
  for (const raw of requests) {
    const method = raw.split(" ")[0];
    socket.write(raw);
    responses.push(await new Promise((resolve, reject) => {
      const started = Date.now();
      const poll = () => {
        const text = data.toString("latin1");
        const end = text.indexOf("\r\n\r\n");
        if (end >= 0) {
          const head = text.slice(0, end);
          const status = Number(head.split(" ")[1]);
          const length = /content-length: (\d+)/i.exec(head);
          const bodyless = method === "HEAD" || status === 304 || status === 204;
          const need = end + 4 + (bodyless || !length ? 0 : Number(length[1]));
          if (data.length >= need) {
            data = data.subarray(need);
            return resolve(text.slice(0, need));
          }
        }
        if (Date.now() - started > 5000) return reject(new Error(`timeout for ${JSON.stringify(raw)}`));
        setTimeout(poll, 5);
      };
      poll();
    }));
  }
  await new Promise((resolve) => setTimeout(resolve, 400));
  socket.destroy();
  return { responses, closed };
}

function cases(port, etag) {
  const origin = `http://localhost:${port}`;
  return [
    ["get_health", false, [req("GET", "/api/health")]],
    ["get_health_connection_close", false, [req("GET", "/api/health", { Connection: "close" })]],
    ["get_health_http10", false, [req("GET", "/api/health", {}, "1.0")]],
    ["get_health_http10_keep_alive", false, [req("GET", "/api/health", { Connection: "keep-alive" }, "1.0")]],
    ["two_requests_one_socket", false, [req("GET", "/api/status"), req("GET", "/api/status", { Connection: "close" })]],
    ["three_requests_one_socket", false, [req("GET", "/api/status"), req("GET", "/nope"), req("HEAD", "/api/status")]],
    ["upper_case_path", false, [req("GET", "/API/STATUS")]],
    ["trailing_slash", false, [req("GET", "/api/status/")]],
    ["double_trailing_slash", false, [req("GET", "/api/status//")]],
    ["mixed_case_trailing_slash", false, [req("GET", "/Api/Health/")]],
    ["query_string", false, [req("GET", "/api/status?x=1")]],
    ["if_none_match_hit", false, [req("GET", "/api/status", { "If-None-Match": etag })]],
    ["if_none_match_star", false, [req("GET", "/api/status", { "If-None-Match": "*" })]],
    ["if_none_match_miss", false, [req("GET", "/api/status", { "If-None-Match": 'W/"0-x"' })]],
    ["if_none_match_list", false, [req("GET", "/api/status", { "If-None-Match": `W/"0-x", ${etag}` })]],
    ["if_none_match_strong_form", false, [req("GET", "/api/status", { "If-None-Match": etag.slice(2) })]],
    ["if_none_match_and_modified_since", false, [req("GET", "/api/status", { "If-None-Match": etag, "If-Modified-Since": "Wed, 21 Oct 2015 07:28:00 GMT" })]],
    ["if_none_match_no_cache", false, [req("GET", "/api/status", { "If-None-Match": etag, "Cache-Control": "no-cache" })]],
    ["head_if_none_match_hit", false, [req("HEAD", "/api/status", { "If-None-Match": etag })]],
    ["head_status", false, [req("HEAD", "/api/status")]],
    ["head_unknown", false, [req("HEAD", "/nope")]],
    ["head_health", false, [req("HEAD", "/api/health")]],
    ["get_unknown", false, [req("GET", "/nope")]],
    ["get_public_missing", false, [req("GET", "/public/x.txt")]],
    ["post_status", false, [req("POST", "/api/status", { "Content-Length": "0" })]],
    ["post_status_json_body", false, [req("POST", "/api/status", { "Content-Type": "application/json", "Content-Length": "2" }, "1.1", "{}")]],
    ["options_allowed_origin", false, [req("OPTIONS", "/anything", { Origin: "paseo://app" })]],
    ["options_same_port_origin", false, [req("OPTIONS", "/anything", { Origin: origin })]],
    ["get_allowed_origin", false, [req("GET", "/api/health", { Origin: origin })]],
    ["get_foreign_origin", false, [req("GET", "/api/health", { Origin: "http://evil.example" })]],
    ["forbidden_host", false, [req("GET", "/api/health", { Host: "evil.example" })]],
    ["forbidden_host_close", false, [req("GET", "/api/health", { Host: "evil.example", Connection: "close" })]],
    ["auth_health_open", true, [req("GET", "/api/health")]],
    ["auth_upper_case_health_needs_token", true, [req("GET", "/API/HEALTH")]],
    ["auth_health_trailing_slash_needs_token", true, [req("GET", "/api/health/")]],
    ["auth_status_without_token", true, [req("GET", "/api/status")]],
    ["auth_status_with_token", true, [req("GET", "/api/status", { Authorization: "Bearer secret" })]],
    ["auth_upper_case_status_with_token", true, [req("GET", "/API/STATUS", { Authorization: "Bearer secret" })]],
    ["auth_wrong_token", true, [req("GET", "/api/status", { Authorization: "Bearer wrong" })]],
    ["auth_unknown_without_token", true, [req("GET", "/nope")]],
    ["auth_options_without_token", true, [req("OPTIONS", "/nope")]],
  ];
}

async function main() {
  const open = await startDaemon(false);
  let secured = null;
  try {
    secured = await startDaemon(true);
    const probe = await exchange(open.port, [req("GET", "/api/status")]);
    const etag = /ETag: (W\/"[^"]+")/.exec(probe.responses[0])[1];
    const status = JSON.parse(probe.responses[0].split("\r\n\r\n")[1]);
    const out = [];
    for (const [name, secure, requests] of cases(open.port, etag)) {
      const daemon = secure ? secured : open;
      // Each case is addressed to its own daemon's port in Origin headers.
      const adjusted = requests.map((raw) => raw.replaceAll(`localhost:${open.port}`, `localhost:${daemon.port}`));
      const { responses, closed } = await exchange(daemon.port, adjusted);
      out.push({ name, secure, requests: adjusted, responses, closed });
    }
    process.stdout.write(JSON.stringify({
      node: process.version,
      original: path.basename(root),
      serverId: status.serverId,
      hostname: status.hostname,
      version: status.version,
      ports: { open: open.port, secured: secured.port },
      etag,
      cases: out,
    }, null, 1));
  } finally {
    await stopDaemon(open);
    if (secured) await stopDaemon(secured);
  }
  process.exit(0);
}
main().catch((error) => { console.error(error); process.exit(1); });
