// Regenerates original-redact-vectors.json: records written by the pinned
// original's own createRootLogger (packages/server logger.ts, pino 10.3.1 with
// REDACT_PATHS and remove: true, console level trace, JSON format), run under
// Node 22.20.0. Each case names the child bindings, the level, the fields object
// and the message; the file keeps the raw output line, with the key order and the
// duplicate keys pino writes. Only the logger module runs: no daemon is started
// and nothing listens.
// Usage: node gen-original-redact-vectors.cjs <original-build-root> > original-redact-vectors.json
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const root = process.argv[2];
const { provenance } = require("./generator-support.cjs");

const SEC = "sec-websocket-protocol";
const SEC_CAP = "Sec-WebSocket-Protocol";

// Field objects are written as JSON text so their key order is the one pino sees;
// integer-like keys are left out because JavaScript reorders them.
const cases = [
  { name: "top_level_keys_are_removed", level: "info", fields: { authorization: "a", Authorization: "b", [SEC]: "c", [SEC_CAP]: "d", keep: 1 } },
  { name: "other_spellings_are_kept", level: "info", fields: { AUTHORIZATION: "a", authorizationX: "b", "Sec-Websocket-Protocol": "c", Xauthorization: "d" } },
  { name: "headers_paths", level: "info", fields: { headers: { authorization: "a", Authorization: "b", [SEC]: "c", [SEC_CAP]: "d", "x-keep": "k" } } },
  { name: "req_headers_paths", level: "info", fields: { req: { headers: { authorization: "a", Authorization: "b", [SEC]: "c", [SEC_CAP]: "d", "x-keep": "k" }, method: "GET" } } },
  { name: "headers_that_are_not_objects", level: "info", fields: { headers: "str" } },
  { name: "headers_null", level: "info", fields: { headers: null } },
  { name: "headers_array", level: "info", fields: { headers: [{ authorization: "a" }, 1] } },
  { name: "req_not_an_object", level: "info", fields: { req: "str", other: true } },
  { name: "req_headers_null", level: "info", fields: { req: { headers: null } } },
  { name: "null_value_is_removed", level: "info", fields: { authorization: null, keep: 1 } },
  { name: "object_value_is_removed", level: "info", fields: { authorization: { a: 1 }, keep: [1, 2] } },
  { name: "deeper_levels_are_kept", level: "info", fields: { headers: { authorization: "a", nested: { authorization: "keep" } }, other: { authorization: "keep2" } } },
  { name: "unrelated_nested_headers_kept", level: "info", fields: { res: { headers: { authorization: "keep" } }, headers2: { authorization: "keep" } } },
  { name: "child_top_level_binding", level: "info", bindings: [{ authorization: "b", name: "x" }], fields: { k: 1 } },
  { name: "child_nested_binding", level: "info", bindings: [{ headers: { authorization: "b", k: 1 } }], fields: {} },
  { name: "child_req_binding", level: "info", bindings: [{ req: { headers: { Authorization: "b" } } }], fields: { n: 2 } },
  { name: "chained_children_in_order", level: "warn", bindings: [{ module: "bootstrap" }, { module: "speech-runtime", [SEC]: "p" }], fields: { module: "field", keep: "yes" } },
  { name: "binding_and_field_with_one_name", level: "info", bindings: [{ name: "a" }], fields: { name: "b" } },
  { name: "bindings_and_fields_both_redacted", level: "error", bindings: [{ [SEC_CAP]: "x", id: 1 }], fields: { authorization: "y", headers: { [SEC]: "z", ok: false } } },
  { name: "trace_level", level: "trace", fields: { a: 1 } },
  { name: "warn_level", level: "warn", fields: { a: [1, { authorization: "kept in array" }] } },
  { name: "error_level", level: "error", fields: {} },
  { name: "fatal_level", level: "fatal", fields: { headers: {} } },
].map((entry) => ({ bindings: [], msg: `case ${entry.name}`, ...entry }));

const driver = `
import { createRootLogger } from ${JSON.stringify(path.join(root, "packages/server/dist/server/server/logger.js"))};
const [home, text] = process.argv.slice(1);
const cases = JSON.parse(text);
const root = createRootLogger({ log: { console: { level: "trace", format: "json" } } }, { paseoHome: home, file: false });
for (const entry of cases) {
  let logger = root;
  for (const binding of entry.bindings) logger = logger.child(binding);
  logger[entry.level](entry.fields, entry.msg);
}
`;

const home = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-redact-vectors-"));
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
