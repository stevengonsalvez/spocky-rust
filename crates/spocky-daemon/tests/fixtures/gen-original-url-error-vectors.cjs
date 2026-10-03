// Regenerates original-url-error-vectors.json: the fatal record "Daemon failed to
// start listening" with the error `new URL(...)` throws, serialised by the pinned
// original's pino (10.3.1, the logger module's own createRootLogger) under Node
// 22.20.0. createAgentMcpBaseUrl builds the agent MCP URL this way in the
// 'listening' handler, and the worker logs a rejection of start() at fatal. Each
// case is one raw output line; the caller frames of the stack are Node's and the
// caller's, so the test compares the stack up to its first line only.
// Usage: node gen-original-url-error-vectors.cjs <original-build-root> > original-url-error-vectors.json
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const root = process.argv[2];
const { provenance } = require("./generator-support.cjs");

const cases = [
  { name: "unclosed_ipv6_host", args: ["http://[::1/mcp/agents"] },
  { name: "relative_input_with_an_invalid_base", args: ["/mcp/agents", "not a url"] },
  { name: "empty_input", args: [""] },
  { name: "empty_host", args: ["http://"] },
];

const driver = `
import { createRootLogger } from ${JSON.stringify(path.join(root, "packages/server/dist/server/server/logger.js"))};
const [home, text] = process.argv.slice(1);
const cases = JSON.parse(text);
const logger = createRootLogger({ log: { console: { level: "trace", format: "json" } } }, { paseoHome: home, file: false });
for (const entry of cases) {
  try {
    new URL(...entry.args);
    logger.info({}, "case " + entry.name + " did not throw");
  } catch (err) {
    logger.fatal({ err }, "Daemon failed to start listening");
  }
}
`;
const home = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-url-error-vectors-"));
let stdout;
try {
  stdout = execFileSync(process.execPath, ["--input-type=module", "-e", driver, home, JSON.stringify(cases)], {
    cwd: home,
    env: { HOME: home, PATH: "/usr/bin:/bin", TZ: "UTC" },
    encoding: "utf8",
  });
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}
const lines = stdout.split("\n").filter(Boolean);
if (lines.length !== cases.length) throw new Error(`${lines.length} lines for ${cases.length} cases`);
for (const line of lines) if (JSON.parse(line).level !== 60) throw new Error(`a case did not throw: ${line}`);
process.stdout.write(
  JSON.stringify(
    {
      node: process.version,
      original: path.basename(root),
      provenance: provenance(root, ["packages/server/dist/server/server/logger.js"]),
      cases: cases.map((entry, index) => ({ ...entry, line: lines[index] })),
    },
    null,
    1,
  ),
);
