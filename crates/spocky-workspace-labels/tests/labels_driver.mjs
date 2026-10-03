// Runs labels_scenarios.json against the pinned Paseo build and prints one
// trace line per step. The Rust differential runs the same script and the two
// traces must match. Usage: node labels_driver.mjs <dist> <root> <scenarios.json>
import { promises as fs } from "node:fs";
import path from "node:path";

const [dist, root, scenariosPath] = process.argv.slice(2);
const { createWorkspaceLabelService } = await import(`${dist}/server/workspace-labels/index.js`);
const { FileBackedWorkspaceRegistry, createPersistedWorkspaceRecord } = await import(
  `${dist}/server/workspace-registry.js`
);
const { writeJsonFileAtomic } = await import(`${dist}/server/atomic-file.js`);

const logger = { child: () => logger, error() {}, warn() {}, info() {}, debug() {} };
const scenarios = JSON.parse(await fs.readFile(scenariosPath, "utf8"));
const lines = [];

function failure(error) {
  return { error: { code: error.code ?? null, message: error.message } };
}

for (const scenario of scenarios) {
  const home = path.join(root, scenario.name);
  await fs.mkdir(home, { recursive: true });
  const flags = {};
  const registries = {};
  const services = {};
  const subscriptions = {};
  const publications = {};
  const changes = {};
  const watching = new Set();

  // The first fault whose flag and conditions hold, with `once` flags consumed.
  function firing(faults, context) {
    for (const fault of faults ?? []) {
      if (fault.flag !== undefined && !flags[fault.flag]) continue;
      if (fault.phase !== undefined && context.phase !== fault.phase) continue;
      if (fault.has !== undefined && !context.names?.includes(fault.has)) continue;
      if (fault.nonEmpty && !(context.names?.length > 0)) continue;
      if (fault.empty && context.names?.length !== 0) continue;
      if (fault.minWrite !== undefined && context.write < fault.minWrite) continue;
      if (fault.once) flags[fault.flag] = false;
      return fault;
    }
    return null;
  }

  async function faultedWrite(faults, context, write) {
    const fault = firing(faults, context);
    if (fault && fault.mode === "before") throw new Error(fault.error);
    await write();
    if (fault && fault.mode === "after") throw new Error(fault.error);
  }

  function addRegistry(step) {
    let writes = 0;
    const registry = new FileBackedWorkspaceRegistry(
      path.join(home, "projects", step.file),
      logger,
      {
        writeRecords: async (filePath, records) => {
          writes += 1;
          await faultedWrite(step.fault, { write: writes }, () =>
            writeJsonFileAtomic(filePath, records),
          );
        },
      },
    );
    registries[step.id] = registry;
    publications[step.id] = [];
    registry.subscribeToMutations((mutation) => {
      if (watching.has(step.id)) {
        publications[step.id].push({ kind: mutation.kind, workspaceId: mutation.workspaceId });
      }
    });
    return registry;
  }

  async function addWorkspaces(registry, ids) {
    for (const workspaceId of ids) {
      await registry.upsert(
        createPersistedWorkspaceRecord({
          workspaceId,
          projectId: "prj_one",
          cwd: "/repo",
          kind: "local_checkout",
          displayName: "main",
          createdAt: "2026-08-14T00:00:00.000Z",
          updatedAt: "2026-08-14T00:00:00.000Z",
        }),
      );
    }
  }

  function addService(step) {
    const options = {
      paseoHome: path.join(home, step.home),
      workspaceRegistry: registries[step.registry],
      writeCatalog: async (filePath, labels) =>
        faultedWrite(step.catalog, { names: labels.map((label) => label.name) }, () =>
          writeJsonFileAtomic(filePath, labels),
        ),
      writeTransaction: async (filePath, transaction) =>
        faultedWrite(step.transaction, { phase: transaction.phase }, () =>
          writeJsonFileAtomic(filePath, transaction),
        ),
      removeTransaction: async (filePath) => {
        const fault = firing(step.remove, {});
        if (fault) throw new Error(fault.error);
        await fs.rm(filePath);
      },
    };
    if (step.journalLimit !== undefined) options.journalLimit = step.journalLimit;
    services[step.id] = createWorkspaceLabelService(options);
  }

  async function walk(directory, relative = "") {
    const entries = (await fs.readdir(directory, { withFileTypes: true })).sort((a, b) =>
      a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
    );
    const files = [];
    for (const entry of entries) {
      const entryRelative = relative === "" ? entry.name : `${relative}/${entry.name}`;
      if (entry.isDirectory()) files.push(...(await walk(path.join(directory, entry.name), entryRelative)));
      else files.push([entryRelative, await fs.readFile(path.join(directory, entry.name), "utf8")]);
    }
    return files;
  }

  async function run(step) {
    switch (step.op) {
      case "registry":
        await addWorkspaces(addRegistry(step), step.workspaces);
        return "ok";
      case "service":
        addService(step);
        return "ok";
      case "flag":
        flags[step.name] = step.value;
        return "ok";
      case "watch":
        watching.add(step.registry);
        if (step.throwing) registries[step.registry].subscribeToMutations(async () => {
          throw new Error("subscriber failed");
        });
        return "ok";
      case "clearLog":
        for (const id of Object.keys(publications)) publications[id].length = 0;
        for (const id of Object.keys(changes)) changes[id].length = 0;
        return "ok";
      case "subscribe": {
        let cursor;
        if (typeof step.from === "string") {
          const sync = subscriptions[step.from].snapshot.sync;
          cursor = { generation: sync.generation, afterSeq: sync.headSeq };
        } else if (step.from) {
          cursor = step.from;
        }
        changes[step.id] = [];
        const subscription = await services[step.service].subscribe({
          cursor,
          onChange: (change) => {
            if (step.throwing) throw new Error("subscriber failed");
            changes[step.id].push(change);
          },
        });
        subscriptions[step.id] = subscription;
        return subscription.snapshot;
      }
      case "unsubscribe":
        subscriptions[step.id].unsubscribe();
        return "ok";
      case "initialize":
        await services[step.service].initialize();
        return "ok";
      case "assign":
        return services[step.service].setAssignment({
          workspaceId: step.ws,
          label: { name: step.name, color: step.color },
          assigned: step.assigned,
        });
      case "update": {
        const input = { name: step.name };
        if (step.newName !== undefined) input.newName = step.newName;
        if (step.color !== undefined) input.color = step.color;
        return services[step.service].update(input);
      }
      case "delete":
        return services[step.service].delete(step.name);
      case "count":
        return services[step.service].countAffectedWorkspaces(step.name);
      case "regop": {
        const registry = registries[step.registry];
        if (step.kind === "get") return (await registry.get(step.ws)) ?? null;
        if (step.kind === "archive") return (await registry.archive(step.ws, step.at)) ?? "ok";
        return registry.update(step.ws, (workspace) => ({ ...workspace, title: step.title }));
      }
      case "write": {
        const target = path.join(home, step.path);
        await fs.mkdir(path.dirname(target), { recursive: true });
        await fs.writeFile(target, step.text);
        return "ok";
      }
      case "parallel": {
        const settled = await Promise.allSettled(step.steps.map((inner) => run(inner)));
        return settled.map((outcome) =>
          outcome.status === "fulfilled" ? "ok" : failure(outcome.reason),
        );
      }
      case "dump": {
        const records = {};
        for (const [id, registry] of Object.entries(registries)) records[id] = await registry.list();
        return { records, publications, changes, files: await walk(home) };
      }
      default:
        throw new Error(`unknown op ${step.op}`);
    }
  }

  for (const [index, step] of scenario.steps.entries()) {
    let result;
    try {
      result = await run(step);
    } catch (error) {
      result = failure(error);
    }
    lines.push(`${scenario.name}#${index} ${step.op}: ${JSON.stringify(result) ?? "undefined"}`);
  }
}
process.stdout.write(`${lines.join("\n")}\n`);
