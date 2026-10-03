// Regenerates original-level-vectors.json: how the pinned original's logger
// (packages/server logger.ts, pino 10.3.1) turns a config `log` section into a
// level, and which records pino then writes. For each case:
// - `level` is `config.file?.level ?? config.console.level` of resolveLogConfig,
//   the level createRootLogger passes to pino, both for the worker (options
//   file: false) and for a caller that allows files;
// - `written` lists the levels of the records a logger created by createRootLogger
//   with file: false writes when one record is sent at each of the six levels.
// Only the logger module runs: no daemon is started and nothing listens.
// Usage: node gen-original-level-vectors.cjs <original-build-root> > original-level-vectors.json
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const root = process.argv[2];
const { provenance } = require("./generator-support.cjs");

const cases = [
  { name: "no_log_section", log: undefined },
  { name: "empty_log_section", log: {} },
  { name: "global_level_warn", log: { level: "warn" } },
  { name: "global_level_trace", log: { level: "trace" } },
  { name: "global_level_fatal", log: { level: "fatal" } },
  { name: "console_level_debug", log: { console: { level: "debug" } } },
  { name: "console_overrides_global", log: { level: "warn", console: { level: "debug" } } },
  { name: "console_format_only", log: { console: { format: "json" } } },
  { name: "file_section_without_level", log: { file: {} } },
  { name: "file_inherits_global_level", log: { level: "error", file: { path: "a.log" } } },
  { name: "file_level_beats_console_level", log: { console: { level: "trace" }, file: { level: "error" } } },
  { name: "file_without_level_and_console_level", log: { console: { level: "debug" }, file: {} } },
  { name: "global_trace_with_file_error", log: { level: "trace", file: { level: "error" } } },
];

const driver = `
import { createRootLogger, resolveLogConfig } from ${JSON.stringify(path.join(root, "packages/server/dist/server/server/logger.js"))};
const [home, text, mode] = process.argv.slice(1);
const cases = JSON.parse(text);
if (mode === "resolve") {
  const level = (config) => config.file?.level ?? config.console.level;
  const out = cases.map((entry) => ({
    name: entry.name,
    workerLevel: level(resolveLogConfig({ log: entry.log }, { paseoHome: home, file: false })),
    fileAllowedLevel: level(resolveLogConfig({ log: entry.log }, { paseoHome: home })),
  }));
  process.stdout.write(JSON.stringify(out));
} else {
  for (const entry of cases) {
    const logger = createRootLogger({ log: entry.log }, { paseoHome: home, file: false });
    for (const name of ["trace", "debug", "info", "warn", "error", "fatal"]) {
      logger[name]({}, "case " + entry.name + ":" + name);
    }
  }
}
`;

const home = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-level-vectors-"));
const run = (mode) =>
  execFileSync(process.execPath, ["--input-type=module", "-e", driver, home, JSON.stringify(cases), mode], {
    cwd: home,
    env: { HOME: home, PATH: "/usr/bin:/bin", TZ: "UTC" },
    encoding: "utf8",
  });
let resolved;
let lines;
try {
  resolved = JSON.parse(run("resolve"));
  lines = run("write").split("\n").filter(Boolean).map((line) => JSON.parse(line));
} finally {
  fs.rmSync(home, { recursive: true, force: true });
}
const out = cases.map((entry) => {
  const found = resolved.find((item) => item.name === entry.name);
  const written = lines
    .filter((line) => line.msg.startsWith(`case ${entry.name}:`))
    .map((line) => line.msg.slice(`case ${entry.name}:`.length));
  return { name: entry.name, log: entry.log === undefined ? null : entry.log, workerLevel: found.workerLevel, fileAllowedLevel: found.fileAllowedLevel, written };
});
process.stdout.write(
  JSON.stringify(
    {
      node: process.version,
      original: path.basename(root),
      provenance: provenance(root, ["packages/server/dist/server/server/logger.js"]),
      cases: out,
    },
    null,
    1,
  ),
);
