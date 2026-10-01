import { createHash, randomUUID } from "node:crypto";
import {
  closeSync,
  linkSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { mkdir, readFile } from "node:fs/promises";
import { arch, platform } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [packageRoot, migrationsRoot, dataDirectory, maximumText] = process.argv.slice(2);
if (!packageRoot || !migrationsRoot || !dataDirectory || !maximumText) {
  throw new Error("package root, migrations root, data directory, and frame maximum are required");
}
const maximum = Number(maximumText);
if (!Number.isSafeInteger(maximum) || maximum < 1024) throw new Error("invalid frame maximum");
const ownerReadAttempts = 10;
const ownerReadDelayMilliseconds = 10;

let client;
let owner;
let input = Buffer.alloc(0);
let operationChain = Promise.resolve();
let closing = false;
let stallTimer;
let failClose = false;
const jsonValue = Symbol("jsonValue");
const jsonParsers = {
  114: (value) => ({ [jsonValue]: JSON.parse(value) }),
  3802: (value) => ({ [jsonValue]: JSON.parse(value) }),
};

try {
  await mkdir(dataDirectory, { recursive: true });
  owner = await acquireOwner(dataDirectory);
  const packageJson = JSON.parse(await readFile(join(packageRoot, "package.json"), "utf8"));
  const { PGlite } = await import(pathToFileURL(join(packageRoot, "dist/index.js")).href);
  client = new PGlite(dataDirectory);
  await client.waitReady;
  send({
    id: 0,
    ok: true,
    result: {
      nodeVersion: process.version,
      nodeExecutable: process.execPath,
      nodeExecutableSha256: createHash("sha256")
        .update(readFileSync(process.execPath))
        .digest("hex"),
      os: platform(),
      arch: arch(),
      package: packageJson.name,
      packageVersion: packageJson.version,
      packageDependencies: packageJson.dependencies ?? {},
      adapterDependencies: ["node:crypto", "node:fs", "node:fs/promises", "node:os", "node:path", "node:url"],
    },
  });
} catch (error) {
  send({ id: 0, ok: false, error: errorPayload(error) });
  await shutdown(1);
}

process.stdin.on("data", (chunk) => {
  input = Buffer.concat([input, chunk]);
  drainFrames();
});
process.stdin.on("end", () => void shutdown(0));
process.stdin.on("error", () => void shutdown(1));
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => void shutdown(128));
}
process.stdin.resume();

function drainFrames() {
  while (input.length >= 4) {
    const length = input.readUInt32BE(0);
    if (length > maximum) {
      input = Buffer.alloc(0);
      send({
        id: 0,
        ok: false,
        error: { code: "FRAME_TOO_LARGE", message: `frame ${length} exceeds ${maximum}` },
      });
      void shutdown(1);
      return;
    }
    if (input.length < 4 + length) return;
    const body = input.subarray(4, 4 + length);
    input = input.subarray(4 + length);
    let request;
    try {
      request = JSON.parse(body.toString("utf8"));
    } catch (error) {
      send({ id: 0, ok: false, error: errorPayload(error, "INVALID_JSON") });
      continue;
    }
    operationChain = operationChain.then(() => handle(request));
  }
}

async function handle(request) {
  const id = Number(request.id);
  try {
    let result;
    switch (request.operation) {
      case "query":
        result = encodeResult(
          await client.query(request.sql, decodeParams(request.params), { parsers: jsonParsers }),
        );
        break;
      case "execute":
        await client.exec(request.sql);
        result = null;
        break;
      case "transaction":
        result = await client.transaction(async (transaction) => {
          const results = [];
          for (const statement of request.statements ?? []) {
            results.push(
              encodeResult(
                await transaction.query(statement.sql, decodeParams(statement.params), {
                  parsers: jsonParsers,
                }),
              ),
            );
          }
          return results;
        });
        break;
      case "migrate":
        result = await migrate();
        break;
      case "crash":
        process.exit(86);
        return;
      case "executeThenCrash":
        await client.exec(request.sql);
        process.exit(87);
        return;
      case "delay":
        await new Promise((resolve) => setTimeout(resolve, Number(request.milliseconds)));
        result = null;
        break;
      case "stallReads":
        process.stdin.pause();
        stallTimer = setInterval(() => {}, 1_000);
        result = null;
        break;
      case "failClose":
        failClose = true;
        result = null;
        break;
      case "close":
        await closeAndReply(id);
        return;
      default:
        throw Object.assign(new Error(`unknown operation: ${request.operation}`), {
          code: "UNKNOWN_OPERATION",
        });
    }
    send({ id, ok: true, result });
  } catch (error) {
    send({ id, ok: false, error: errorPayload(error) });
  }
}

async function closeAndReply(id) {
  closing = true;
  if (stallTimer) clearInterval(stallTimer);
  let failure;
  try {
    await client?.close();
  } catch (error) {
    failure = error;
  }
  client = undefined;
  try {
    releaseOwner();
  } catch (error) {
    failure ??= error;
  }
  if (!failure && failClose) {
    failure = Object.assign(new Error("injected close failure after durable close"), {
      code: "CLOSE_FAILED",
    });
  }
  if (failure) {
    send({ id, ok: false, error: errorPayload(failure, "CLOSE_FAILED") });
    process.exitCode = 1;
  } else {
    send({ id, ok: true, result: null });
    process.exitCode = 0;
  }
  process.stdin.pause();
}

async function migrate() {
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
      const migration = await readFile(join(migrationsRoot, `${entry.tag}.sql`), "utf8");
      for (const statement of migration.split("--> statement-breakpoint")) {
        if (statement.trim().length > 0) await transaction.exec(statement);
      }
      const hash = createHash("sha256").update(migration).digest("hex");
      await transaction.query(
        "insert into drizzle.__drizzle_migrations (hash, created_at) values ($1, $2)",
        [hash, entry.when],
      );
    }
  });
  const count = await client.query(
    "select count(*)::bigint as count from drizzle.__drizzle_migrations",
  );
  return { applied: pending.length, journalRows: Number(count.rows[0].count) };
}

function decodeParams(params = []) {
  return params.map((value) => {
    switch (value.type) {
      case "null":
        return null;
      case "boolean":
      case "string":
      case "timestamp":
      case "numeric":
        return value.value;
      case "json":
        return JSON.stringify(value.value);
      case "binary":
        return Uint8Array.from(value.value);
      default:
        throw Object.assign(new Error(`unknown value type: ${value.type}`), {
          code: "INVALID_VALUE",
        });
    }
  });
}

function encodeResult(result) {
  const fields = result.fields ?? [];
  const columns = fields.length > 0 ? fields.map((field) => field.name) : Object.keys(result.rows[0] ?? {});
  const fieldTypes = new Map(
    fields.map((field) => [field.name, field.dataTypeID ?? field.dataTypeId ?? field.dataType]),
  );
  return {
    columns,
    rows: result.rows.map((row) =>
      columns.map((column) => encodeValue(row[column], fieldTypes.get(column))),
    ),
    affectedRows: result.rows.length > 0 ? result.rows.length : (result.affectedRows ?? 0),
  };
}

function encodeValue(value, oid) {
  if (typeof value === "object" && value !== null && jsonValue in value) {
    return { type: "json", value: value[jsonValue] };
  }
  if (value === null || value === undefined) return { type: "null" };
  if (oid === 114 || oid === 3802) return { type: "json", value };
  if (value instanceof Uint8Array) return { type: "binary", value: [...value] };
  if (value instanceof Date) return { type: "timestamp", value: value.toISOString() };
  if (oid === 1114 || oid === 1184) {
    return { type: "timestamp", value: new Date(value).toISOString() };
  }
  if ([20, 21, 23, 700, 701, 1700].includes(oid) || typeof value === "bigint") {
    return { type: "numeric", value: String(value) };
  }
  if (typeof value === "number") return { type: "numeric", value: String(value) };
  if (typeof value === "boolean") return { type: "boolean", value };
  if (typeof value === "string") return { type: "string", value };
  return { type: "json", value };
}

function send(response) {
  const bytes = Buffer.from(JSON.stringify(response));
  if (bytes.length > maximum) {
    const fallback = Buffer.from(
      JSON.stringify({
        id: response.id ?? 0,
        ok: false,
        error: { code: "FRAME_TOO_LARGE", message: `response exceeds ${maximum}` },
      }),
    );
    writeFrame(fallback);
    return;
  }
  writeFrame(bytes);
}

function writeFrame(bytes) {
  const header = Buffer.alloc(4);
  header.writeUInt32BE(bytes.length);
  process.stdout.write(header);
  process.stdout.write(bytes);
}

async function acquireOwner(directory) {
  const path = join(directory, ".paseo-hub.lock");
  const token = randomUUID();
  const record = JSON.stringify({ pid: process.pid, token });
  for (;;) {
    try {
      const descriptor = openSync(path, "wx", 0o600);
      writeFileSync(descriptor, record);
      closeSync(descriptor);
      await delay(ownerReadAttempts * ownerReadDelayMilliseconds);
      try {
        if (readFileSync(path, "utf8") === record) return { path, token };
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
      continue;
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
    }
    const observed = await readOwner(path);
    if (observed.owner && processIsRunning(observed.owner.pid)) {
      throw Object.assign(new Error("PGlite data directory is already in use"), {
        code: "DIRECTORY_IN_USE",
      });
    }
    const tombstone = `${path}.reclaim-${token}`;
    try {
      renameSync(path, tombstone);
    } catch (error) {
      if (error?.code === "ENOENT") continue;
      throw error;
    }
    const claimed = readFileSync(tombstone, "utf8");
    if (claimed !== observed.raw) {
      try {
        linkSync(tombstone, path);
        unlinkSync(tombstone);
      } catch (error) {
        if (error?.code !== "EEXIST") throw error;
      }
      continue;
    }
    unlinkSync(tombstone);
  }
}

async function readOwner(path) {
  let raw;
  for (let attempt = 0; attempt < ownerReadAttempts; attempt += 1) {
    try {
      raw = readFileSync(path, "utf8");
      const parsed = JSON.parse(raw);
      if (
        parsed !== null &&
        typeof parsed === "object" &&
        Number.isSafeInteger(parsed.pid) &&
        typeof parsed.token === "string"
      ) {
        return { raw, owner: { pid: parsed.pid, token: parsed.token } };
      }
    } catch (error) {
      if (error?.code === "ENOENT") return { raw: undefined, owner: undefined };
      if (!(error instanceof SyntaxError)) throw error;
    }
    await delay(ownerReadDelayMilliseconds);
  }
  return { raw, owner: undefined };
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function processIsRunning(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

function releaseOwner() {
  if (!owner) return;
  try {
    const current = JSON.parse(readFileSync(owner.path, "utf8"));
    if (current.token === owner.token) unlinkSync(owner.path);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  owner = undefined;
}

function errorPayload(error, fallback = "REMOTE_ERROR") {
  return {
    code: typeof error?.code === "string" ? error.code : fallback,
    message: error instanceof Error ? error.message : String(error),
    details: {
      name: typeof error?.name === "string" ? error.name : null,
      severity: typeof error?.severity === "string" ? error.severity : null,
      detail: typeof error?.detail === "string" ? error.detail : null,
      hint: typeof error?.hint === "string" ? error.hint : null,
      position: typeof error?.position === "string" ? error.position : null,
      schema: typeof error?.schema === "string" ? error.schema : null,
      table: typeof error?.table === "string" ? error.table : null,
      column: typeof error?.column === "string" ? error.column : null,
      constraint: typeof error?.constraint === "string" ? error.constraint : null,
    },
  };
}

async function shutdown(code) {
  if (closing) return;
  closing = true;
  if (stallTimer) clearInterval(stallTimer);
  try {
    await client?.close();
  } catch {}
  try {
    releaseOwner();
  } catch {}
  process.exitCode = code;
  process.stdin.pause();
}
