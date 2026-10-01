// Enumerates the import contract of the pinned PGlite Wasm modules and counts
// which imports the retained-host workload actually calls. Original side only:
// it runs the distributed JavaScript glue unchanged and wraps each imported
// function with a counter before instantiation.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [packageRoot, migrationsRoot] = process.argv.slice(2);
if (!packageRoot || !migrationsRoot) {
  throw new Error("usage: pglite-rust-host-import-trace.mjs <package root> <migrations root>");
}

const modules = {};
for (const name of ["pglite.wasm", "initdb.wasm"]) {
  const bytes = readFileSync(join(packageRoot, "dist", name));
  const module = new WebAssembly.Module(bytes);
  modules[bytes.length] = {
    name,
    bytes: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    imports: WebAssembly.Module.imports(module),
    exportCount: WebAssembly.Module.exports(module).length,
    calls: {},
    instances: 0,
    dynamicLibraries: [],
    dynamicSymbols: [],
  };
}

const originalInstantiate = WebAssembly.instantiate;
WebAssembly.instantiate = async function instantiate(source, imports) {
  const length = source instanceof WebAssembly.Module ? null : source.byteLength;
  const entry =
    length === null
      ? Object.values(modules).find((candidate) => candidate.compiled === source)
      : modules[length];
  if (!entry) return originalInstantiate.call(this, source, imports);
  entry.instances += 1;
  const wrapped = {};
  const memory = imports.env.memory;
  const cString = (pointer) => {
    const heap = new Uint8Array(memory.buffer);
    let end = pointer;
    while (heap[end] !== 0) end += 1;
    return Buffer.from(heap.subarray(pointer, end)).toString("utf8");
  };
  for (const [moduleName, namespace] of Object.entries(imports)) {
    if (moduleName === "GOT.mem" || moduleName === "GOT.func") {
      wrapped[moduleName] = namespace;
      continue;
    }
    wrapped[moduleName] = {};
    for (const descriptor of entry.imports.filter((item) => item.module === moduleName)) {
      const value = namespace[descriptor.name];
      if (descriptor.kind === "function" && typeof value === "function") {
        const key = `${moduleName}.${descriptor.name}`;
        entry.calls[key] ??= 0;
        const counted = function (...args) {
          entry.calls[key] += 1;
          // Emscripten dynlink.c: struct dso keeps its path at offset 36.
          if (key === "env._dlopen_js") entry.dynamicLibraries.push(cString(args[0] + 36));
          if (key === "env._dlsym_js") entry.dynamicSymbols.push(cString(args[1]));
          return value.apply(this, args);
        };
        wrapped[moduleName][descriptor.name] = counted;
      } else {
        wrapped[moduleName][descriptor.name] = value;
      }
    }
  }
  const result = await originalInstantiate.call(this, source, wrapped);
  if (result.module) entry.compiled = result.module;
  return result;
};

const { PGlite } = await import(pathToFileURL(join(packageRoot, "dist/index.js")).href);
const dataDirectory = await mkdtemp(join(tmpdir(), "spocky-pglite-trace-"));
const workload = [];
try {
  let client = new PGlite(dataDirectory);
  await client.waitReady;
  workload.push("initdb-and-open");
  const migration = await migrate(client);
  workload.push(`migrate:${migration.applied}:${migration.journalRows}`);
  await client.query(
    "select $1::text, $2::bytea, $3::timestamptz, $4::numeric, $5::jsonb, $6::boolean",
    ["hello", Uint8Array.from([0, 1, 255]), "2026-10-01T00:00:00.123Z", "1.5", '{"a":1}', true],
  );
  workload.push("typed-query");
  await client.exec("create table trace_probe (id integer primary key, value text)");
  await client
    .transaction(async (transaction) => {
      await transaction.query("insert into trace_probe values (1, 'kept-out')");
      throw new Error("rollback");
    })
    .catch(() => undefined);
  workload.push("transaction-rollback");
  await client.query("select * from missing_table").catch(() => undefined);
  workload.push("structured-error");
  await client.query("insert into trace_probe values (2, 'durable')");
  await client.close();
  workload.push("close");
  client = new PGlite(dataDirectory);
  await client.waitReady;
  const reopened = await client.query("select value from trace_probe order by id");
  workload.push(`reopen:${reopened.rows.map((row) => row.value).join(",")}`);
  await client.close();
  workload.push("close");
} finally {
  await rm(dataDirectory, { recursive: true, force: true });
}

const report = { package: "@electric-sql/pglite", workload, modules: [] };
for (const entry of Object.values(modules)) {
  const byModule = {};
  for (const item of entry.imports) {
    byModule[item.module] ??= { function: 0, global: 0, memory: 0, table: 0 };
    byModule[item.module][item.kind] += 1;
  }
  report.modules.push({
    name: entry.name,
    bytes: entry.bytes,
    sha256: entry.sha256,
    exportCount: entry.exportCount,
    instances: entry.instances,
    importCount: entry.imports.length,
    dynamicLibraries: entry.dynamicLibraries,
    dynamicSymbols: entry.dynamicSymbols,
    byModule,
    imports: entry.imports.map((item) => ({
      module: item.module,
      name: item.name,
      kind: item.kind,
      calls: item.kind === "function" ? (entry.calls[`${item.module}.${item.name}`] ?? 0) : null,
    })),
  });
}
process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);

async function migrate(client) {
  const journal = JSON.parse(await readFile(join(migrationsRoot, "meta/_journal.json"), "utf8"));
  await client.exec(`
    create schema if not exists drizzle;
    create table if not exists drizzle.__drizzle_migrations (
      id serial primary key,
      hash text not null,
      created_at bigint
    );
  `);
  const applied = await client.query(
    "select created_at from drizzle.__drizzle_migrations order by created_at desc limit 1",
  );
  const lastCreatedAt = Number(applied.rows[0]?.created_at ?? 0);
  const pending = journal.entries.filter((entry) => entry.when > lastCreatedAt);
  await client.transaction(async (transaction) => {
    for (const entry of pending) {
      const sql = await readFile(join(migrationsRoot, `${entry.tag}.sql`), "utf8");
      for (const statement of sql.split("--> statement-breakpoint")) {
        if (statement.trim().length > 0) await transaction.exec(statement);
      }
      const hash = createHash("sha256").update(sql).digest("hex");
      await transaction.query(
        "insert into drizzle.__drizzle_migrations (hash, created_at) values ($1, $2)",
        [hash, entry.when],
      );
    }
  });
  const count = await client.query("select count(*)::bigint as count from drizzle.__drizzle_migrations");
  return { applied: pending.length, journalRows: Number(count.rows[0].count) };
}
