// Regenerates express-vectors.json: raw HTTP exchanges with the pinned Express
// app (express 4.22.1 on Node 22.20.0) shaped like the vertical slice of the
// bootstrap.ts middleware chain (Host check, CORS, bearer auth, /api/health,
// /api/status, the default 404). The exchanges are executed, not written by hand.
// Usage: node gen-express-vectors.cjs <paseo-runtime-root> > express-vectors.json
const path = require("node:path");
const http = require("node:http");
const net = require("node:net");
const root = process.argv[2];
const express = require(path.join(root, "node_modules/express"));

const LISTEN = "127.0.0.1:41999";
const PASSWORD = "secret";
const allowedOrigins = new Set(["paseo://app", `http://127.0.0.1:41999`, `http://localhost:41999`]);

function build(withPassword) {
  const app = express();
  app.set("trust proxy", ["loopback"]);
  app.use((req, res, next) => {
    const host = typeof req.headers.host === "string" ? req.headers.host : undefined;
    const name = host === undefined ? undefined : host.split(":")[0];
    if (name !== "localhost" && name !== "127.0.0.1") {
      res.status(403).json({ error: "Invalid Host header" });
      return;
    }
    next();
  });
  app.use((req, res, next) => {
    const origin = req.headers.origin;
    if (origin && (allowedOrigins.has("*") || allowedOrigins.has(origin))) {
      res.setHeader("Access-Control-Allow-Origin", origin);
      res.setHeader("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS");
      res.setHeader("Access-Control-Allow-Headers", "Content-Type, Authorization");
      res.setHeader("Access-Control-Allow-Credentials", "true");
    }
    if (req.method === "OPTIONS") {
      res.status(204).end();
      return;
    }
    next();
  });
  if (withPassword) {
    app.use((req, res, next) => {
      if (req.method === "OPTIONS" || req.path === "/api/health") return next();
      if (req.headers.authorization === `Bearer ${PASSWORD}`) return next();
      res.status(401).json({ error: "Unauthorized" });
    });
  }
  app.use(express.json());
  app.get("/api/health", (_req, res) => {
    res.json({ status: "ok", timestamp: new Date().toISOString() });
  });
  app.get("/api/status", (_req, res) => {
    res.json({ status: "server_info", serverId: "srv_test", hostname: "box", version: "0.10.0", listen: LISTEN });
  });
  return http.createServer(app);
}

const req = (method, target, headers = {}, version = "1.1") =>
  `${method} ${target} HTTP/${version}\r\n` +
  Object.entries({ Host: "localhost", ...headers }).map(([k, v]) => `${k}: ${v}\r\n`).join("") +
  "\r\n";

function readResponse(socket, bufferRef, method) {
  return new Promise((resolve, reject) => {
    const tryParse = () => {
      const text = bufferRef.data.toString("latin1");
      const end = text.indexOf("\r\n\r\n");
      if (end < 0) return false;
      const head = text.slice(0, end);
      const status = Number(head.split(" ")[1]);
      const length = /content-length: (\d+)/i.exec(head);
      const bodyless = method === "HEAD" || status === 304 || status === 204;
      const need = end + 4 + (bodyless || !length ? 0 : Number(length[1]));
      if (bufferRef.data.length < need) return false;
      const raw = text.slice(0, need);
      bufferRef.data = bufferRef.data.subarray(need);
      resolve(raw);
      return true;
    };
    if (tryParse()) return;
    const onData = () => { if (tryParse()) socket.off("data", onData); };
    socket.on("data", onData);
    setTimeout(() => reject(new Error("response timeout")), 3000);
  });
}

async function exchange(port, requests) {
  const socket = net.connect(port, "127.0.0.1");
  const ref = { data: Buffer.alloc(0) };
  socket.on("data", (chunk) => { ref.data = Buffer.concat([ref.data, chunk]); });
  await new Promise((r) => socket.once("connect", r));
  let closed = false;
  socket.on("close", () => { closed = true; });
  const responses = [];
  for (const raw of requests) {
    const method = raw.split(" ")[0];
    socket.write(raw);
    responses.push(await new Promise((resolve, reject) => {
      const started = Date.now();
      const poll = () => {
        const text = ref.data.toString("latin1");
        const end = text.indexOf("\r\n\r\n");
        if (end >= 0) {
          const head = text.slice(0, end);
          const status = Number(head.split(" ")[1]);
          const length = /content-length: (\d+)/i.exec(head);
          const bodyless = method === "HEAD" || status === 304 || status === 204;
          const need = end + 4 + (bodyless || !length ? 0 : Number(length[1]));
          if (ref.data.length >= need) {
            ref.data = ref.data.subarray(need);
            return resolve(text.slice(0, need));
          }
        }
        if (Date.now() - started > 3000) return reject(new Error("timeout for " + raw));
        setTimeout(poll, 5);
      };
      poll();
    }));
  }
  await new Promise((r) => setTimeout(r, 400));
  socket.destroy();
  return { responses, closed };
}

async function main() {
  const open = build(false);
  const secured = build(true);
  await Promise.all([open, secured].map((s) => new Promise((r) => s.listen(0, "127.0.0.1", r))));
  const probe = await exchange(open.address().port, [req("GET", "/api/status")]);
  const etag = /ETag: (W\/"[^"]+")/.exec(probe.responses[0])[1];
  const cases = [
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
    ["options_allowed_origin", false, [req("OPTIONS", "/anything", { Origin: "paseo://app" })]],
    ["get_allowed_origin", false, [req("GET", "/api/health", { Origin: "http://localhost:41999" })]],
    ["get_foreign_origin", false, [req("GET", "/api/health", { Origin: "http://evil.example" })]],
    ["forbidden_host", false, [req("GET", "/api/health", { Host: "evil.example" })]],
    ["forbidden_host_close", false, [req("GET", "/api/health", { Host: "evil.example", Connection: "close" })]],
    ["auth_health_open", true, [req("GET", "/api/health")]],
    ["auth_upper_case_health_needs_token", true, [req("GET", "/API/HEALTH")]],
    ["auth_health_trailing_slash_needs_token", true, [req("GET", "/api/health/")]],
    ["auth_status_without_token", true, [req("GET", "/api/status")]],
    ["auth_status_with_token", true, [req("GET", "/api/status", { Authorization: "Bearer secret" })]],
    ["auth_upper_case_status_with_token", true, [req("GET", "/API/STATUS", { Authorization: "Bearer secret" })]],
    ["auth_unknown_without_token", true, [req("GET", "/nope")]],
    ["auth_options_without_token", true, [req("OPTIONS", "/nope")]],
  ];
  const out = [];
  for (const [name, secure, requests] of cases) {
    const server = secure ? secured : open;
    const { responses, closed } = await exchange(server.address().port, requests);
    out.push({ name, secure, requests, responses, closed });
  }
  process.stdout.write(JSON.stringify({ node: process.version, express: require(path.join(root, "node_modules/express/package.json")).version, etag, cases: out }, null, 1));
  process.exit(0);
}
main().catch((e) => { console.error(e); process.exit(1); });
