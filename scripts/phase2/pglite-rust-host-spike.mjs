// Original side of the PGlite Rust host spike. Runs the pinned JavaScript
// host: fresh initdb, SELECT 1, a PL/pgSQL BEGIN ... EXCEPTION block that
// must longjmp, a marker row, close. Counts _emscripten_throw_longjmp calls,
// longjmps caught by invoke_* wrappers, and dlopen and
// dlsym calls, and prints the data directory tree for comparison with the
// Rust host.
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [packageRoot, dataDirectory] = process.argv.slice(2);
if (!packageRoot || !dataDirectory) {
  throw new Error("usage: pglite-rust-host-spike.mjs <package root> <new data directory>");
}

const counters = { longjmp: 0, invokeCaughtLongjmp: 0, exit: 0, dlopen: 0, dlsym: 0 };
let pendingLongjmp = false;
const counted = (name, original) =>
  (...args) => {
    counters[name] += 1;
    return original(...args);
  };
const originalInstantiate = WebAssembly.instantiate;
WebAssembly.instantiate = async function instantiate(source, imports) {
  if (imports?.env?._emscripten_throw_longjmp) {
    const env = { ...imports.env };
    const throwLongjmp = env._emscripten_throw_longjmp;
    env._emscripten_throw_longjmp = (...args) => {
      counters.longjmp += 1;
      pendingLongjmp = true;
      return throwLongjmp(...args);
    };
    const exit = env.exit;
    env.exit = (...args) => {
      counters.exit += 1;
      return exit(...args);
    };
    if (env._dlopen_js) env._dlopen_js = counted("dlopen", env._dlopen_js);
    if (env._dlsym_js) env._dlsym_js = counted("dlsym", env._dlsym_js);
    // An invoke_* wrapper that returns normally after a longjmp was thrown
    // is the one that caught it (setThrew(1, 0) in the glue): the throw
    // unwinds to the innermost invoke on the stack.
    for (const name of Object.keys(env).filter((key) => key.startsWith("invoke_"))) {
      const invoke = env[name];
      env[name] = (...args) => {
        const result = invoke(...args);
        if (pendingLongjmp) {
          pendingLongjmp = false;
          counters.invokeCaughtLongjmp += 1;
        }
        return result;
      };
    }
    imports = { ...imports, env, wasi_snapshot_preview1: imports.wasi_snapshot_preview1 };
  }
  return originalInstantiate.call(this, source, imports);
};

const { PGlite } = await import(pathToFileURL(join(packageRoot, "dist/index.js")).href);
const steps = [];
const client = new PGlite(dataDirectory);
await client.waitReady;
steps.push({ step: "open", ok: true });
const one = await client.query("select 1 as one");
steps.push({ step: "select1", rows: one.rows });
const before = counters.longjmp;
await client.exec(`
  do $$
  begin
    perform 1 / 0;
  exception when division_by_zero then
    raise notice 'caught %', sqlstate;
  end
  $$;
`);
steps.push({ step: "plpgsqlException", longjmpCalls: counters.longjmp - before });
try {
  await client.query("select * from missing_table");
  steps.push({ step: "structuredError", error: "query unexpectedly succeeded" });
} catch (error) {
  steps.push({
    step: "structuredError",
    code: error.code,
    message: error.message,
    severity: error.severity,
  });
}
await client.exec("create table spike_marker (id integer primary key, note text)");
await client.query("insert into spike_marker values ($1, $2)", [1, "spike marker"]);
const settings = await client.query(
  "select current_setting('TimeZone') as timezone, current_setting('server_version') as version",
);
steps.push({ step: "settings", rows: settings.rows });
await client.close();
steps.push({ step: "close", ok: true });

function walk(root, relative = "") {
  const entries = [];
  for (const name of readdirSync(join(root, relative)).sort()) {
    const path = join(relative, name);
    const stat = lstatSync(join(root, path));
    const entry = {
      path,
      type: stat.isDirectory() ? "dir" : stat.isFile() ? "file" : "other",
      size: stat.isFile() ? stat.size : null,
      mode: (stat.mode & 0o7777).toString(8),
      mtimeMs: Math.floor(stat.mtimeMs),
    };
    if (stat.isFile()) {
      entry.sha256 = createHash("sha256").update(readFileSync(join(root, path))).digest("hex");
    }
    entries.push(entry);
    if (stat.isDirectory()) entries.push(...walk(root, path));
  }
  return entries;
}

process.stdout.write(
  `${JSON.stringify({ host: "node", nodeVersion: process.version, steps, counters, tree: walk(dataDirectory) }, null, 2)}\n`,
);
