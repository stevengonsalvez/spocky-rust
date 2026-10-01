import { join } from "node:path";
import { pathToFileURL } from "node:url";

const sourceRoot = process.env.PASEO_HUB_SOURCE_ROOT;
if (!sourceRoot) throw new Error("PASEO_HUB_SOURCE_ROOT is required");
process.chdir(sourceRoot);
const { embeddedDatabaseRuntime } = await import(pathToFileURL(join(sourceRoot, "src/db/runtime/index.ts")).href);
const [mode, dataDirectory] = process.argv.slice(2);
const bundle = await embeddedDatabaseRuntime(dataDirectory);
const count = async () => Number((await bundle.runtime.query("select count(*)::bigint as count from drizzle.__drizzle_migrations")).rows[0].count);
const before = await count().catch(() => 0);
const migration = await bundle.runtime.migrate();
const after = await count();
await bundle.runtime.query("create table if not exists schema_downgrade_probe (producer text primary key, payload text not null)");
const producer = mode === "produce" ? "baseline-newer" : "baseline-older";
await bundle.runtime.query("insert into schema_downgrade_probe (producer, payload) values ($1, $2) on conflict (producer) do nothing", [producer, "preserved"]);
const rows = (await bundle.runtime.query("select producer, payload from schema_downgrade_probe order by producer")).rows;
await bundle.runtime.close();
process.stdout.write(`${JSON.stringify({ mode, before, after, migration, rows })}\n`);
