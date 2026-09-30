import { access, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

type StreamObservation = {
  eventTypes: string[];
  assistantText: string;
  permissionRequests: number;
  permissionResolutions: number;
};

async function importReference(referenceRoot: string, relativePath: string) {
  return import(pathToFileURL(path.join(referenceRoot, relativePath)).href);
}

async function observeStream(
  manager: {
    streamAgent(agentId: string, prompt: string): AsyncGenerator<Record<string, unknown>>;
    respondToPermission(
      agentId: string,
      requestId: string,
      response: { behavior: "allow" },
    ): Promise<unknown>;
  },
  agentId: string,
  prompt: string,
): Promise<StreamObservation> {
  const observation: StreamObservation = {
    eventTypes: [],
    assistantText: "",
    permissionRequests: 0,
    permissionResolutions: 0,
  };
  for await (const event of manager.streamAgent(agentId, prompt)) {
    const type = String(event.type);
    observation.eventTypes.push(type);
    if (type === "timeline") {
      const item = event.item as { type?: string; text?: string } | undefined;
      if (item?.type === "assistant_message") observation.assistantText += item.text ?? "";
    }
    if (type === "permission_requested") {
      observation.permissionRequests += 1;
      const request = event.request as { id: string };
      await manager.respondToPermission(agentId, request.id, { behavior: "allow" });
    }
    if (type === "permission_resolved") observation.permissionResolutions += 1;
  }
  return observation;
}

async function observeCancellation(
  manager: {
    streamAgent(agentId: string, prompt: string): AsyncGenerator<Record<string, unknown>>;
  },
  dependencies: Record<string, unknown>,
  cancelAgentRunCommand: (
    dependencies: Record<string, unknown>,
    agentId: string,
  ) => Promise<{ cancelled: boolean }>,
  agentId: string,
): Promise<{ eventTypes: string[]; cancelled: boolean }> {
  const eventTypes: string[] = [];
  let cancelled = false;
  for await (const event of manager.streamAgent(agentId, "sleep 30")) {
    const type = String(event.type);
    eventTypes.push(type);
    const item = event.item as { type?: string } | undefined;
    if (!cancelled && type === "timeline" && item?.type === "tool_call") {
      cancelled = (await cancelAgentRunCommand(dependencies, agentId)).cancelled;
    }
  }
  return { eventTypes, cancelled };
}

async function main(): Promise<void> {
  const stateRoot = process.env.PASEO_DIFFERENTIAL_STATE;
  const referenceRoot = process.env.PASEO_REFERENCE_RUNTIME;
  if (!stateRoot) throw new Error("PASEO_DIFFERENTIAL_STATE is required");
  if (!referenceRoot) throw new Error("PASEO_REFERENCE_RUNTIME is required");

  const [managerModule, storageModule, clientsModule, loggerModule, loading, lifecycle] =
    await Promise.all([
      importReference(referenceRoot, "packages/server/src/server/agent/agent-manager.ts"),
      importReference(referenceRoot, "packages/server/src/server/agent/agent-storage.ts"),
      importReference(referenceRoot, "packages/server/src/server/test-utils/fake-agent-client.ts"),
      importReference(referenceRoot, "packages/server/src/test-utils/test-logger.ts"),
      importReference(referenceRoot, "packages/server/src/server/agent/agent-loading.ts"),
      importReference(referenceRoot, "packages/server/src/server/agent/lifecycle-command.ts"),
    ]);
  const { AgentManager } = managerModule;
  const { AgentStorage } = storageModule;
  const { createTestAgentClients } = clientsModule;
  const { createTestLogger } = loggerModule;

  const workspace = path.join(stateRoot, "workspace");
  const storageRoot = path.join(stateRoot, "agents");
  await mkdir(workspace, { recursive: true });
  const logger = createTestLogger();
  const storage = new AgentStorage(storageRoot, logger);
  const manager = new AgentManager({
    clients: createTestAgentClients(),
    registry: storage,
    logger,
  });
  const agentId = "00000000-0000-4000-8000-000000000901";
  const phases: Array<Record<string, unknown>> = [];

  try {
    const created = await manager.createAgent(
      { provider: "codex", cwd: workspace, modeId: "default" },
      agentId,
      { workspaceId: "lifecycle-workspace" },
    );
    phases.push({
      phase: "create",
      lifecycle: created.lifecycle,
      live: manager.getAgent(agentId) !== null,
    });

    const streamed = await observeStream(manager, agentId, "Respond with exactly: STREAM_OK");
    phases.push({
      phase: "stream",
      lifecycle: manager.getAgent(agentId)?.lifecycle ?? null,
      eventTypes: streamed.eventTypes,
      assistantText: streamed.assistantText,
    });

    const permission = await observeStream(manager, agentId, 'printf "ok" > permission.txt');
    let fileCreated = true;
    try {
      await access(path.join(workspace, "permission.txt"));
    } catch {
      fileCreated = false;
    }
    phases.push({
      phase: "permission",
      lifecycle: manager.getAgent(agentId)?.lifecycle ?? null,
      eventTypes: permission.eventTypes,
      permissionRequests: permission.permissionRequests,
      permissionResolutions: permission.permissionResolutions,
      fileCreated,
    });

    await manager.setAgentMode(agentId, "bypassPermissions");
    const cancellation = await observeCancellation(
      manager,
      { agentManager: manager, logger },
      lifecycle.cancelAgentRunCommand,
      agentId,
    );
    phases.push({
      phase: "cancel",
      lifecycle: manager.getAgent(agentId)?.lifecycle ?? null,
      eventTypes: cancellation.eventTypes,
      cancelled: cancellation.cancelled,
    });

    const sessionId = manager.getAgent(agentId)?.persistence?.sessionId ?? null;
    await manager.closeAgent(agentId);
    await manager.flush();
    await storage.flush();
    const closed = await storage.get(agentId);
    phases.push({
      phase: "restart",
      live: manager.getAgent(agentId) !== null,
      storedStatus: closed?.lastStatus ?? null,
      archived: Boolean(closed?.archivedAt),
    });

    const resumed = await loading.ensureAgentLoaded(agentId, {
      agentManager: manager,
      agentStorage: storage,
      logger,
    });
    phases.push({
      phase: "resume",
      lifecycle: resumed.lifecycle,
      live: manager.getAgent(agentId) !== null,
      sameSession: resumed.persistence?.sessionId === sessionId,
    });

    const archived = await lifecycle.archiveAgentCommand(
      { agentManager: manager, agentStorage: storage, logger },
      agentId,
    );
    phases.push({
      phase: "archive",
      live: manager.getAgent(agentId) !== null,
      storedStatus: archived.record.lastStatus,
      archived: Boolean(archived.record.archivedAt),
    });

    const recovered = await loading.ensureAgentLoaded(agentId, {
      agentManager: manager,
      agentStorage: storage,
      logger,
    });
    const recoveredRecord = await storage.get(agentId);
    const assistantMessages = manager
      .getTimeline(agentId)
      .filter((item: { type: string }) => item.type === "assistant_message").length;
    phases.push({
      phase: "recovery",
      lifecycle: recovered.lifecycle,
      live: manager.getAgent(agentId) !== null,
      archived: Boolean(recoveredRecord?.archivedAt),
      assistantMessages,
    });
  } finally {
    await manager.closeAgent(agentId).catch(() => undefined);
    await manager.flush().catch(() => undefined);
    await storage.flush().catch(() => undefined);
  }

  const assertions = phases.reduce((count, phase) => count + Object.keys(phase).length - 1, 0);
  const outputRoot = path.join(stateRoot, "output");
  await mkdir(outputRoot, { recursive: true });
  await writeFile(path.join(outputRoot, "structured.json"), JSON.stringify(phases));
  await writeFile(
    path.join(outputRoot, "counts.json"),
    JSON.stringify({ fixtures: phases.length, assertions }),
  );
  process.stdout.write(`lifecycle phases ${phases.length}, assertions ${assertions}\n`);
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exitCode = 1;
});
