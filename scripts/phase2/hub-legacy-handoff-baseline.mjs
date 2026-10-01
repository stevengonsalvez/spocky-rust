import { join } from "node:path";
import { pathToFileURL } from "node:url";

const sourceRoot = process.env.PASEO_HUB_SOURCE_ROOT;
if (!sourceRoot) throw new Error("PASEO_HUB_SOURCE_ROOT is required");
process.chdir(sourceRoot);

const { embeddedDatabaseRuntime } = await import(
  pathToFileURL(join(sourceRoot, "src/db/runtime/index.ts")).href
);
const [operation, dataDirectory] = process.argv.slice(2);
if (!operation || !dataDirectory) throw new Error("operation and data directory are required");

if (operation === "produce") {
  const bundle = await embeddedDatabaseRuntime(dataDirectory);
  await bundle.runtime.migrate();
  await bundle.runtime.query(`
    create table legacy_handoff_probe (
      producer text primary key,
      payload text not null
    )
  `);
  await bundle.runtime.query(
    "insert into legacy_handoff_probe (producer, payload) values ($1, $2)",
    ["pinned-baseline", "baseline-data-preserved"],
  );
  const journal = await journalRows(bundle.runtime);
  const rows = await probeRows(bundle.runtime);
  await bundle.runtime.close();
  process.stdout.write(
    `${JSON.stringify({
      operation: "baseline-produce",
      journalRows: journal,
      probeRows: rows,
    })}\n`,
  );
} else if (operation === "reverse-observe") {
  const bundle = await embeddedDatabaseRuntime(dataDirectory);
  const journalBeforeMigrate = await journalRows(bundle.runtime);
  await bundle.runtime.migrate();
  const journalAfterMigrate = await journalRows(bundle.runtime);
  const rows = await probeRows(bundle.runtime);
  await bundle.runtime.close();
  process.stdout.write(
    `${JSON.stringify({
      operation: "baseline-reverse-observe",
      opened: true,
      journalBeforeMigrate,
      journalAfterMigrate,
      probeRows: rows,
    })}\n`,
  );
} else {
  throw new Error(`unknown operation: ${operation}`);
}

async function journalRows(runtime) {
  const result = await runtime.query(
    "select count(*)::bigint as count from drizzle.__drizzle_migrations",
  );
  return Number(result.rows[0]?.count);
}

async function probeRows(runtime) {
  const result = await runtime.query(
    "select producer, payload from legacy_handoff_probe order by producer",
  );
  return result.rows;
}
