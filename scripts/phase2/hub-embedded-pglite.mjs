import { spawn } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const sourceRoot = process.env.PASEO_HUB_SOURCE_ROOT;
const tsx = process.env.PASEO_HUB_TSX;
if (!sourceRoot) throw new Error("PASEO_HUB_SOURCE_ROOT is required");
if (!tsx) throw new Error("PASEO_HUB_TSX is required");
process.chdir(sourceRoot);
const { embeddedDatabaseRuntime } = await import(
  pathToFileURL(join(sourceRoot, "src/db/runtime/index.ts")).href
);

const [operation, dataDirectory] = process.argv.slice(2);
if (!operation || !dataDirectory) throw new Error("operation and data directory are required");

if (operation === "probe-open") {
  try {
    const bundle = await embeddedDatabaseRuntime(dataDirectory);
    await bundle.runtime.close();
  } catch (error) {
    if (String(error).includes("already in use")) process.exit(23);
    throw error;
  }
} else if (operation === "capture") {
  await capture(dataDirectory);
} else {
  throw new Error(`unknown operation: ${operation}`);
}

async function capture(directory) {
  let bundle = await embeddedDatabaseRuntime(directory);
  await bundle.runtime.migrate();
  await bundle.runtime.query(`create table differential_probe (value text not null unique)`);
  await bundle.runtime.query(`insert into differential_probe (value) values ('kept')`);
  await bundle.runtime.close();

  bundle = await embeddedDatabaseRuntime(directory);
  await bundle.runtime.migrate();
  const restarted = await bundle.runtime.query(`select value from differential_probe`);
  const child = await spawnChild(directory);
  const rollbackResult = await bundle.runtime.transaction(async (transaction) => {
    await transaction.query(`insert into differential_probe (value) values ('rolled-back')`);
    return transaction.rollback("rolled-back");
  });
  const rolledBackRows = await bundle.runtime.query(
    `select value from differential_probe where value = 'rolled-back'`,
  );

  const events = [];
  let releaseFirst;
  let firstEntered;
  const entered = new Promise((resolve) => (firstEntered = resolve));
  const release = new Promise((resolve) => (releaseFirst = resolve));
  const first = bundle.locks.withLock("shared", async () => {
    events.push("first:start");
    firstEntered();
    await release;
    events.push("first:end");
  });
  await entered;
  const second = bundle.locks.withLock("shared", async () => {
    events.push("second:start", "second:end");
  });
  await new Promise((resolve) => setTimeout(resolve, 25));
  releaseFirst();
  await Promise.all([first, second]);
  const tables = await bundle.runtime.query(`
    select table_schema || '.' || table_name as name
    from information_schema.tables
    where table_schema in ('public', 'drizzle')
    order by table_schema, table_name
  `);
  const constraints = await bundle.runtime.query(`
    select constraint_name as name
    from information_schema.table_constraints
    where table_schema = 'public'
      and constraint_type <> 'PRIMARY KEY'
    order by constraint_name
  `);
  const indexes = await bundle.runtime.query(`
    select indexname as name
    from pg_indexes
    where schemaname = 'public'
      and indexname not like '%_pkey'
    order by indexname
  `);
  const schemaTables = tables.rows
    .map((row) => row.name)
    .filter((name) => name.startsWith("public.") && name !== "public.differential_probe");
  const schemaConstraints = [...new Set([
    ...constraints.rows.map((row) => row.name),
    ...indexes.rows.map((row) => row.name),
  ])].sort();
  const migrationJournal = await bundle.runtime.query(`
    select hash, created_at as "createdAt"
    from drizzle.__drizzle_migrations
    order by created_at, id
  `);
  const lockOwnerKeys = Object.keys(
    JSON.parse(await readFile(join(directory, ".paseo-hub.lock"), "utf8")),
  ).sort();
  await bundle.runtime.close();

  const lockPath = join(directory, ".paseo-hub.lock");
  await writeFile(lockPath, JSON.stringify({ pid: 2_147_483_647, token: "stale" }));
  const staleOwnerRecovery = await recoversOwner(directory);
  await writeFile(lockPath, `{"pid":123`);
  const incompleteOwnerRecovery = await recoversOwner(directory);

  bundle = await embeddedDatabaseRuntime(directory);
  await bundle.runtime.migrate();
  const reopenedJournal = await bundle.runtime.query(`
    select hash, created_at as "createdAt"
    from drizzle.__drizzle_migrations
    order by created_at, id
  `);
  await bundle.runtime.close();

  process.stdout.write(`${JSON.stringify({
    operations: {
      restart: restarted.rows.length === 1 && restarted.rows[0]?.value === "kept",
      crossProcessRejection: child.exitCode === 23,
      transactionRollback: rollbackResult === "rolled-back" && rolledBackRows.rows.length === 0,
      sameKeySerialization: events,
      staleOwnerRecovery: staleOwnerRecovery && incompleteOwnerRecovery,
    },
    observations: {
      tables: tables.rows.map((row) => row.name),
      constraints: constraints.rows.map((row) => row.name),
      schemaTables,
      schemaConstraints,
      schemaSource: "installed database catalog",
      migrationJournal: migrationJournal.rows,
      migrationReopenStable:
        JSON.stringify(migrationJournal.rows) === JSON.stringify(reopenedJournal.rows),
      lockOwnerKeys,
    },
    boundary: {
      engine: "PGlite",
      schema: "baseline relational",
      dialect: "PostgreSQL",
      migrations: "baseline journal",
    },
  })}\n`);
}

async function recoversOwner(directory) {
  try {
    const recovered = await embeddedDatabaseRuntime(directory);
    await recovered.runtime.close();
    return true;
  } catch {
    return false;
  }
}

function spawnChild(directory) {
  const child = spawn(tsx, [process.argv[1], "probe-open", directory], {
    env: process.env,
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => (stderr += chunk));
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (exitCode) => resolve({ exitCode, stderr }));
  });
}
