import { createInterface } from "node:readline";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const sourceRoot = process.env.PASEO_HUB_SOURCE_ROOT;
if (!sourceRoot) throw new Error("PASEO_HUB_SOURCE_ROOT is required");
process.chdir(sourceRoot);

const { embeddedDatabaseRuntime } = await import(
  pathToFileURL(join(sourceRoot, "src/db/runtime/index.ts")).href
);
const [operation, dataDirectory] = process.argv.slice(2);
if (!operation || !dataDirectory) {
  throw new Error("operation and data directory are required");
}

if (operation === "hold") {
  const bundle = await embeddedDatabaseRuntime(dataDirectory);
  await bundle.runtime.migrate();
  await bundle.runtime.query(`
    create table if not exists mixed_ownership_probe (
      producer text primary key,
      payload text not null
    )
  `);
  await bundle.runtime.query(
    `insert into mixed_ownership_probe (producer, payload)
     values ($1, $2)
     on conflict (producer) do update set payload = excluded.payload`,
    ["pinned-baseline", "pinned-baseline-live-owner"],
  );
  const journalRows = await journalCount(bundle.runtime);
  const marker = await markerPayload(bundle.runtime);
  writeJson({
    operation: "baseline-hold",
    event: "ready",
    ownerPid: process.pid,
    journalRows,
    marker,
  });

  const lines = createInterface({ input: process.stdin });
  const command = await new Promise((resolve) => lines.once("line", resolve));
  lines.close();
  if (command !== "close") {
    throw new Error(`unexpected hold command: ${command}`);
  }
  await bundle.runtime.close();
  writeJson({ operation: "baseline-hold", event: "closed" });
} else if (operation === "try-open") {
  try {
    const bundle = await embeddedDatabaseRuntime(dataDirectory);
    await bundle.runtime.close();
    writeJson({ operation: "baseline-try-open", opened: true });
  } catch (error) {
    writeJson({
      operation: "baseline-try-open",
      opened: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
} else {
  throw new Error(`unknown operation: ${operation}`);
}

async function journalCount(runtime) {
  const result = await runtime.query(
    "select count(*)::bigint as count from drizzle.__drizzle_migrations",
  );
  return Number(result.rows[0]?.count);
}

async function markerPayload(runtime) {
  const result = await runtime.query(
    "select payload from mixed_ownership_probe where producer = $1",
    ["pinned-baseline"],
  );
  return result.rows[0]?.payload;
}

function writeJson(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}
