// Regenerates origin-vectors.json from the pinned isWebSocketSameOrigin.
// Usage: node gen-origin-vectors.mjs <paseo-rewrite-root> > origin-vectors.json
// Runs on Node with TypeScript type stripping (Node 22.18+).
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const source = readFileSync(
  path.join(process.argv[2], "packages/server/src/server/websocket-server.ts"),
  "utf8",
);
const begin = source.indexOf("interface HostAuthority");
const finish = source.indexOf("function selectWebSocketProtocol");
const origins = ["http://localhost:6767", "http://127.0.0.1:6767", "https://localhost", "http://[::1]:6767", "http://LOCALHOST:6767", "http://127.1:6767", "http://0x7f.1:6767", "http://2130706433:6767", "http://a.localhost:80", "http://localhost:80", "http://localhost:080", "paseo://app", "null", "http://localhost:6767/path", "http://user@localhost:6767", "http://127.0.0.1.:6767", "http://[::ffff:127.0.0.1]:6767", "http://evil.com:6767", "file:///x", "http://localhost:65535", "http://localhost:0"];
const hosts = ["localhost:6767", "127.0.0.1:6767", "[::1]:6767", "[::1]", "localhost", "LOCALHOST:6767", "localhost:06767", "a.localhost:80", "127.0.0.1", "evil.com:6767", "::1", "0:0:0:0:0:0:0:1", "[0:0:0:0:0:0:0:1]:6767", "localhost:", "[::1]:", "127.0.0.1:65535", "127.0.0.1:0", "127.1:6767"];
const dir = mkdtempSync(path.join(tmpdir(), "origin-vectors-"));
const file = path.join(dir, "same-origin.ts");
writeFileSync(file, `${source.slice(begin, finish)}\n`);
const { isWebSocketSameOrigin } = await import(file);
const rows = [];
for (const origin of origins) for (const host of hosts) rows.push([origin, host, isWebSocketSameOrigin(origin, host)]);
process.stdout.write(JSON.stringify(rows));
