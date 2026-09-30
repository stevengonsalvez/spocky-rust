import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

async function main(): Promise<void> {
  const stateRoot = process.env.PASEO_DIFFERENTIAL_STATE;
  const referenceRoot = process.env.PASEO_REFERENCE_RUNTIME;
  if (!stateRoot) throw new Error("PASEO_DIFFERENTIAL_STATE is required");
  if (!referenceRoot) throw new Error("PASEO_REFERENCE_RUNTIME is required");

  const storageModule = await import(
    pathToFileURL(
      path.join(referenceRoot, "packages/server/src/server/agent/agent-storage.ts"),
    ).href,
  );
  const { AgentStorage, parseStoredAgentRecord } = storageModule;
  const logger = {
    child() {
      return this;
    },
    error() {},
    warn() {},
  };

  const source = await readFile(path.join(stateRoot, "input/record.json"), "utf8");
  const record = parseStoredAgentRecord(JSON.parse(source));
  const storeRoot = path.join(stateRoot, "store");
  const store = new AgentStorage(storeRoot, logger);
  await store.upsert(record);
  await store.flush();

  const restarted = new AgentStorage(storeRoot, logger);
  await restarted.initialize();
  const reloaded = await restarted.get(record.id);
  if (!reloaded) throw new Error("record missing after restart");

  const outputRoot = path.join(stateRoot, "output");
  await mkdir(outputRoot, { recursive: true });
  await writeFile(path.join(outputRoot, "structured.json"), JSON.stringify(reloaded));
  await writeFile(path.join(outputRoot, "recovery.json"), '{"restart":"loaded"}');
  await writeFile(path.join(outputRoot, "counts.json"), '{"fixtures":1,"assertions":5}');
  process.stdout.write(`stored ${record.id}\n`);
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exitCode = 1;
});
