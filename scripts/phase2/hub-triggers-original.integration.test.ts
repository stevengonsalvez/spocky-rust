import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { test } from "vitest";
import { DatabaseUnavailableError } from "../src/db/errors.js";
import { createMemoryDatabase } from "../src/db/memory.js";
import type { Database, ManualEventPersistence } from "../src/db/types.js";
import { durableExecutionId } from "../src/daemons/lifecycle.js";
import { createManualRunProvider } from "../src/triggers/manual/provider.js";
import { createManualTriggerSource, handleManualTriggerRequest } from "../src/triggers/manual/source.js";
import { createWebhookSource } from "../src/triggers/github/webhook.js";
import { createPublicApi } from "../src/public-api/index.js";
import type { PublicOperations } from "../src/public-operations/index.js";

test("captures offline trigger, lease, execution, and GitHub webhook behavior", async () => {
  const database = createMemoryDatabase({ now: () => new Date("2026-08-06T12:00:00.000Z") });
  const orgA = await prepareProject(database, "org-a");
  const orgB = await prepareProject(database, "org-b");
  const firstReceipt = await database.persistManualEvent(manualInput(orgA, "shared-delivery"));
  const replayReceipt = await database.persistManualEvent(manualInput(orgA, "shared-delivery"));
  const otherReceipt = await database.persistManualEvent(manualInput(orgB, "shared-delivery"));
  const firstEvent = acceptedEvent(firstReceipt);
  const otherEvent = acceptedEvent(otherReceipt);
  const runInput = {
    organizationId: orgA.organizationId,
    projectId: orgA.projectId,
    configurationRevisionId: orgA.revisionId,
    providerEventReceiptId: firstEvent.providerEventReceiptId,
    configuredTriggerName: "deploy",
    prompt: "deploy",
    inputs: {},
    triggerContext: {},
    outputContext: {},
    deadlineAt: new Date(10_000),
    stepIds: ["deploy-step"],
    createdAt: new Date(0),
  };
  const firstRun = await database.createAcceptedTriggerRun(runInput);
  const replayRun = await database.createAcceptedTriggerRun(runInput);
  const stepRunId = await onlyStepRunId(database, firstRun.run.id);
  const stableExecutionId = durableExecutionId({
    triggerRunId: firstRun.run.id,
    configurationRevisionId: orgA.revisionId,
    triggerName: "deploy",
    workflowStepRunId: stepRunId,
  });
  const firstLease = await database.claimWorkflowWakeup(new Date(1_000), 500);
  assert.ok(firstLease);
  const firstExecution = await database.createWorkflowStepExecution({
    triggerRunId: firstRun.run.id,
    stepId: "deploy-step",
    ordinal: 0,
    executionId: stableExecutionId,
    execution: executionInput(orgA, stableExecutionId, 1_000),
  });
  const blockedBeforeExpiry =
    (await database.claimWorkflowWakeup(new Date(1_499), 500)) === undefined;
  const recoveryLease = await database.claimWorkflowWakeup(new Date(1_501), 500);
  assert.ok(recoveryLease);
  const recoveredExecution = await database.createWorkflowStepExecution({
    triggerRunId: firstRun.run.id,
    stepId: "deploy-step",
    ordinal: 0,
    executionId: stableExecutionId,
    execution: executionInput(orgA, stableExecutionId, 1_501),
  });
  await database.releaseWorkflowWakeup(firstRun.run.id, new Date(1_502), firstLease.leaseExpiresAt!);
  const staleReleaseRejected =
    (await database.claimWorkflowWakeup(new Date(1_502), 500)) === undefined;
  await database.releaseWorkflowWakeup(
    firstRun.run.id,
    new Date(1_502),
    recoveryLease.leaseExpiresAt!,
  );
  const currentReleaseAccepted =
    (await database.claimWorkflowWakeup(new Date(1_502), 500)) !== undefined;
  // fan-out is created after the lease checks so a second wakeup cannot satisfy them
  const fanOutRun = await database.createAcceptedTriggerRun({
    ...runInput,
    configuredTriggerName: "rollback",
  });
  const fanOutReplay = await database.createAcceptedTriggerRun({
    ...runInput,
    configuredTriggerName: "rollback",
  });
  const otherRun = await database.createAcceptedTriggerRun({
    ...runInput,
    organizationId: orgB.organizationId,
    projectId: orgB.projectId,
    configurationRevisionId: orgB.revisionId,
    providerEventReceiptId: otherEvent.providerEventReceiptId,
  });

  const running = await database.transitionAgentExecution(
    firstExecution.execution!.id,
    "running",
  );
  const succeeded = await database.transitionAgentExecution(
    firstExecution.execution!.id,
    "succeeded",
    { result: { status: "succeeded" } },
  );
  const conflicting = await database.transitionAgentExecution(
    firstExecution.execution!.id,
    "failed",
    { result: { status: "failed" } },
  );
  const runSucceeded = await database.succeedTriggerRun(firstRun.run.id);
  const runSucceededAgain = await database.succeedTriggerRun(firstRun.run.id);
  const finalRun = await database.findTriggerRunById(firstRun.run.id);
  const runIds = new Set([firstRun.run.id, fanOutRun.run.id, otherRun.run.id]);

  const output = {
    schemaVersion: 1,
    manual: {
      firstCreated: firstRun.created,
      replayCreated: replayRun.created,
      sameReceipt: receiptId(replayReceipt) === firstEvent.providerEventReceiptId,
      crossOrgDistinct:
        otherEvent.providerEventReceiptId !== firstEvent.providerEventReceiptId,
      receiptCount: new Set([
        firstEvent.providerEventReceiptId,
        receiptId(replayReceipt),
        otherEvent.providerEventReceiptId,
      ]).size,
      runCount: runIds.size,
      fanOutDistinct: fanOutRun.run.id !== firstRun.run.id,
      fanOutCreated: fanOutRun.created,
      fanOutReplayCreated: fanOutReplay.created,
      fanOutReplaySameRun: fanOutReplay.run.id === fanOutRun.run.id,
    },
    lease: {
      blockedBeforeExpiry,
      recoveredAfterExpiry: recoveryLease.triggerRunId === firstRun.run.id,
      leasedBeforeClaim: recoveryLease.leasedBeforeClaim,
      sameExecution: recoveredExecution.execution?.id === firstExecution.execution?.id,
      executionIdIsDurable: firstExecution.execution?.id === stableExecutionId,
      executionCount: new Set([firstExecution.execution?.id, recoveredExecution.execution?.id])
        .size,
      staleReleaseRejected,
      currentReleaseAccepted,
    },
    execution: {
      initial: firstExecution.execution!.status,
      runningTransition: running.transitioned,
      firstTerminal: succeeded.execution.status,
      conflictingTerminalTransition: conflicting.transitioned,
      finalStatus: conflicting.execution.status,
      completedAtKept: conflicting.execution.completedAt?.getTime() ===
        succeeded.execution.completedAt?.getTime(),
      idleDeadlineSet: firstExecution.execution!.idleDeadlineAt?.getTime() === 10_000,
      idleDeadlineCleared: conflicting.execution.idleDeadlineAt === null,
      runStatus: finalRun?.status,
      runSucceededTransition: runSucceeded?.transitioned,
      runSucceededAgainTransition: runSucceededAgain?.transitioned,
    },
    durableExecutionId: {
      fixed: durableExecutionId({
        triggerRunId: "run-1",
        configurationRevisionId: "revision-1",
        triggerName: "deploy",
        workflowStepRunId: "step-run-1",
      }),
      noStep: durableExecutionId({
        triggerRunId: "run-1",
        configurationRevisionId: "revision-1",
        triggerName: "deploy",
        workflowStepRunId: undefined,
      }),
      otherTrigger: durableExecutionId({
        triggerRunId: "run-1",
        configurationRevisionId: "revision-1",
        triggerName: "rollback",
        workflowStepRunId: "step-run-1",
      }),
    },
    manualRequests: await manualRequestTrace(),
    manualRunMatch: await manualRunMatchTrace(),
    publicManualRun: await publicManualRunTrace(),
    github: await githubTrace(),
  };
  assert.equal(output.manual.receiptCount, 2);
  assert.equal(output.manual.runCount, 3);
  const outputPath = process.env["SPOCKY_HUB_TRIGGERS_OUTPUT"];
  if (!outputPath) throw new Error("SPOCKY_HUB_TRIGGERS_OUTPUT is required");
  await writeFile(outputPath, `${JSON.stringify(output, null, 2)}\n`);
});

interface PreparedProject {
  organizationId: string;
  projectId: string;
  revisionId: string;
}

async function prepareProject(
  database: Database,
  organizationId: string,
): Promise<PreparedProject> {
  const project = await database.createProject({
    organizationId,
    name: organizationId,
    slug: organizationId,
    createdByUserId: "user-1",
  });
  const revision = await database.insertProjectConfigurationRevision({
    projectId: project.id,
    sourceKind: "manual",
    sourceEvidence: { kind: "test" },
    normalizedConfiguration: { environments: [], triggers: [] },
    contentHash: `configuration-${organizationId}`,
  });
  await database.activateProjectConfigurationRevision(project.id, revision.id);
  return { organizationId, projectId: project.id, revisionId: revision.id };
}

function manualInput(project: PreparedProject, deliveryId: string) {
  return {
    organizationId: project.organizationId,
    projectId: project.projectId,
    source: "manual.run",
    deliveryId,
    payload: {},
    receivedAt: new Date(0),
  };
}

function executionInput(project: PreparedProject, id: string, startedAt: number) {
  return {
    id,
    organizationId: project.organizationId,
    projectId: project.projectId,
    machineId: null,
    triggerContext: {},
    outputContext: {},
    configurationRevisionId: project.revisionId,
    deadlineAt: new Date(10_000),
    idleDeadlineAt: new Date(10_000),
    startedAt: new Date(startedAt),
  };
}

async function onlyStepRunId(database: Database, triggerRunId: string): Promise<string> {
  const steps = await database.listWorkflowStepRunsForTriggerRun(triggerRunId);
  assert.equal(steps.length, 1);
  return steps[0]!.id;
}

function acceptedEvent(result: ManualEventPersistence) {
  if (result.status !== "accepted") throw new Error("expected accepted event");
  return result.event;
}

function receiptId(result: ManualEventPersistence): string {
  return result.status === "accepted"
    ? result.event.providerEventReceiptId
    : result.providerEventReceiptId;
}

async function manualRequestTrace() {
  const database = createMemoryDatabase();
  const orgA = await prepareProject(database, "org_1");
  const orgB = await prepareProject(database, "org_2");
  const handled: unknown[] = [];
  const recording = createManualTriggerSource(database);
  await recording.start(async (event) => {
    handled.push(event);
  });
  const idle = createManualTriggerSource(database);
  const deliver = async (source: typeof recording, body: string) => {
    try {
      const response = await handleManualTriggerRequest(
        new Request("http://localhost/test/trigger", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body,
        }),
        source,
        "trigger",
      );
      return { status: response.status, body: await response.text() };
    } catch (error) {
      return { status: 0, body: `threw: ${(error as Error).message}` };
    }
  };
  const delivery = (project: PreparedProject, overrides: Record<string, unknown>) =>
    JSON.stringify({
      organizationId: project.organizationId,
      projectId: project.projectId,
      source: "discord.mention",
      deliveryId: "manual-1",
      payload: { guildId: "guild-1" },
      ...overrides,
    });
  const cases: Record<string, { status: number; body: string }> = {};
  const record = async (name: string, source: typeof recording, body: string) => {
    cases[name] = await deliver(source, body);
  };
  const stripIds = (value: string) =>
    value.replaceAll(orgA.projectId, "<project-a>").replaceAll(orgB.projectId, "<project-b>");

  await record("accepted", recording, delivery(orgA, {}));
  const acceptedEvidence = handled.map((event) => {
    const { connectionId, resourceId, source, deliveryId } = event as Record<string, unknown>;
    return { connectionId, resourceId, source, deliveryId };
  });
  await record("duplicateSameOrganization", recording, delivery(orgA, {}));
  const handledAfterDuplicate = handled.length;
  await record("sameDeliveryOtherOrganization", recording, delivery(orgB, {}));
  const handledAfterOtherOrganization = handled.length;
  await record("noHandlerDropped", idle, delivery(orgA, { deliveryId: "manual-idle" }));
  await record("withConnectionEvidence", recording, delivery(orgA, {
    deliveryId: "manual-evidence",
    connectionId: "7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e",
    resourceId: "resource-1",
  }));
  const lastEvent = handled.at(-1) as Record<string, unknown>;
  const connectionEvidence = {
    connectionId: lastEvent["connectionId"],
    resourceId: lastEvent["resourceId"],
  };
  const receiptOf = async (organizationId: string, deliveryId: string) => {
    const receipt = await database.findProviderEventReceiptByDeliveryId(deliveryId, organizationId);
    return receipt === undefined
      ? null
      : {
          provider: receipt.provider,
          source: receipt.source,
          droppedReason: receipt.droppedReason,
          connectionId: receipt.connectionId,
          resourceId: receipt.resourceId,
        };
  };
  const receipts = {
    accepted: await receiptOf("org_1", "manual-1"),
    idle: await receiptOf("org_1", "manual-idle"),
    otherOrganization: await receiptOf("org_2", "manual-1"),
    distinctReceiptIds:
      (await database.findProviderEventReceiptByDeliveryId("manual-1", "org_1"))?.id !==
      (await database.findProviderEventReceiptByDeliveryId("manual-1", "org_2"))?.id,
  };
  await record("nonNamespacedSource", recording, delivery(orgA, { source: "manual" }));
  await record("emptySource", recording, delivery(orgA, { source: "" }));
  await record("emptyDeliveryId", recording, delivery(orgA, { deliveryId: "" }));
  await record("emptyOrganizationId", recording, delivery(orgA, { organizationId: "" }));
  await record("projectIdNotUuid", recording, delivery(orgA, { projectId: "project-1" }));
  await record("invalidReceivedAt", recording, delivery(orgA, { receivedAt: "not-a-date" }));
  await record("receivedAtNumber", recording, delivery(orgA, { receivedAt: 1 }));
  await record("validReceivedAt", recording, delivery(orgA, {
    deliveryId: "manual-received-at",
    receivedAt: "2026-08-06T12:00:00.000Z",
  }));
  await record("missingOrganizationId", recording, JSON.stringify({
    projectId: orgA.projectId,
    source: "discord.mention",
    deliveryId: "manual-missing",
  }));
  await record("connectionIdNotUuid", recording, delivery(orgA, { connectionId: "connection-1" }));
  await record("connectionIdNumber", recording, delivery(orgA, { connectionId: 7 }));
  await record("connectionIdNull", recording, delivery(orgA, {
    deliveryId: "manual-null-connection",
    connectionId: null,
    resourceId: null,
  }));
  await record("connectionIdNilUuid", recording, delivery(orgA, {
    deliveryId: "manual-nil-connection",
    connectionId: "00000000-0000-0000-0000-000000000000",
  }));
  await record("connectionIdVersionNine", recording, delivery(orgA, {
    connectionId: "7f1b0c1e-2d3a-9b5c-8d6e-9f0a1b2c3d4e",
  }));
  await record("resourceIdNumber", recording, delivery(orgA, { resourceId: 7 }));
  await record("sourceMissing", recording, JSON.stringify({
    organizationId: orgA.organizationId,
    projectId: orgA.projectId,
    deliveryId: "manual-no-source",
  }));
  await record("sourceNumber", recording, delivery(orgA, { source: 7 }));
  await record("deliveryIdNumber", recording, delivery(orgA, { deliveryId: 7 }));
  await record("projectIdMissing", recording, JSON.stringify({
    organizationId: orgA.organizationId,
    source: "discord.mention",
    deliveryId: "manual-no-project",
  }));
  await record("receivedAtNull", recording, delivery(orgA, { receivedAt: null }));
  await record("receivedAtDateOnly", recording, delivery(orgA, {
    deliveryId: "manual-date-only",
    receivedAt: "2026-08-06",
  }));
  await record("receivedAtOffset", recording, delivery(orgA, {
    deliveryId: "manual-offset",
    receivedAt: "2026-08-06T12:00:00+01:00",
  }));
  await record("receivedAtImpossibleDate", recording, delivery(orgA, { receivedAt: "2026-02-30" }));
  await record("multipleInvalidFields", recording, delivery(orgA, {
    organizationId: "",
    projectId: "project-1",
    source: "manual",
  }));
  await record("payloadOmitted", recording, JSON.stringify({
    organizationId: orgA.organizationId,
    projectId: orgA.projectId,
    source: "discord.mention",
    deliveryId: "manual-no-payload",
  }));
  await record("payloadNull", recording, delivery(orgA, {
    deliveryId: "manual-null-payload",
    payload: null,
  }));
  await record("utf8BomBody", recording, `\uFEFF${delivery(orgA, { deliveryId: "manual-bom" })}`);
  await record("unknownProject", recording, delivery(orgA, {
    deliveryId: "manual-unknown-project",
    projectId: "7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e",
  }));
  await record("invalidJson", recording, "{not json");
  await record("arrayBody", recording, "[]");
  for (const entry of Object.values(cases)) entry.body = stripIds(entry.body);
  const receivedAtGrid: Record<string, number> = {};
  for (const [index, receivedAt] of RECEIVED_AT_GRID.entries()) {
    const result = await deliver(
      recording,
      delivery(orgA, { deliveryId: `manual-grid-${index}`, receivedAt }),
    );
    receivedAtGrid[receivedAt] = result.status;
  }
  return {
    cases,
    receivedAtGrid,
    handledAfterDuplicate,
    handledAfterOtherOrganization,
    receipts,
    acceptedEvidence,
    connectionEvidence,
  };
}

// ISO-8601 grammar subset. Legacy forms the baseline also accepts (for example
// "Aug 6 2026", "2026/08/06", "2026-08-06 12:00", "2026-8-6", " 2026-08-06")
// are a documented gap and deliberately absent from this grid.
export const RECEIVED_AT_GRID = [
  "2026", "2026-08", "2026-08-06", "2026-08-06T12:00", "2026-08-06T12:00:00",
  "2026-08-06T12:00Z", "2026-08-06T12:00:00z", "2026-08-06T12:00:00+0100",
  "2026-08-06T24:00:00Z", "2026-08-06T12:00:00.1234Z", "2026-02-30",
  "+002026-08-06T00:00:00Z", "-000001-01-01T00:00:00Z", "2026-08-06T12:00:00+23:59",
  "2026-08-06T12:00:00-00:00", "9999-12-31T23:59:59.999Z", "0000-01-01",
  "+275760-09-13T00:00:00.000Z",
  "2026-08-06T12Z", "2026-08-06T12:00:00+01", "2026-08-06T24:00:01Z",
  "2026-08-06T25:00:00Z", "2026-08-06T12:60:00Z", "2026-08-06T12:00:60Z",
  "2026-08-06T12:00:00.Z", "2026-02-32", "2026-13-01", "2026-00-10", "2026-08-00",
  "-000000-01-01T00:00:00Z", "275760-09-13T00:00:00Z", "+275760-09-13T00:00:00.001Z",
  "2026-08-06T12:00:00 Z", "2026-08-06T12:00:00+24:00", "26-08-06", "", "not-a-date",
];

async function manualRunMatchTrace() {
  const current = "11111111-1111-4111-8111-111111111111";
  const stale = "22222222-2222-4222-8222-222222222222";
  const revision = (triggers: unknown[]) => ({
    revision: { id: current },
    configuration: { environments: [], triggers },
  });
  const trigger = (name: string, fromUsers: string[]) => ({
    name,
    on: "manual.run",
    steps: [],
    inputs: {},
    filters: { from_users: fromUsers },
  });
  const configurations = new Map<string, ReturnType<typeof revision>>([
    [
      current,
      revision([
        trigger("deploy", ["*"]),
        trigger("rollback", ["alice"]),
        { name: "open", on: "manual.run", steps: [], inputs: {} },
        { name: "cron", on: "schedule.tick", steps: [], inputs: {}, filters: { from_users: ["*"] } },
      ]),
    ],
  ]);
  const provider = createManualRunProvider(() => ({
    getRevision: async (id: string) => configurations.get(id),
  }) as never);
  const run = async (
    revisionId: string,
    payload: Record<string, unknown>,
    deliveryId = "delivery-1",
  ) => {
    try {
      const matches = await provider.match({
        organizationId: "org-a",
        projectId: "project-a",
        configurationRevisionId: revisionId,
        providerEventReceiptId: "receipt-1",
        source: "manual.run",
        deliveryId,
        receivedAt: new Date(0),
        payload,
        connectionId: null,
        resourceId: null,
      } as never);
      if (typeof matches === "string") return { outcome: "dropped", reason: matches };
      return {
        outcome: "matched",
        triggers: matches.map((match) => match.triggerName),
        deliveryId: (matches[0]?.triggerContext as { deliveryId: string }).deliveryId,
        configurationRevisionIsCurrent: matches[0]?.configurationRevisionId === current,
      };
    } catch (error) {
      return { outcome: "rejected", code: (error as { code?: string }).code ?? String(error) };
    }
  };
  return {
    matched: await run(current, { trigger: "deploy", actor: "anyone", input: "" }),
    publicDeliveryKey: await run(current, {
      trigger: "deploy",
      actor: "anyone",
      input: "",
      publicDeliveryKey: "public-key",
    }),
    expectedCurrent: await run(current, {
      trigger: "deploy",
      actor: "anyone",
      input: "",
      expectedVersionId: current,
    }),
    expectedStale: await run(current, {
      trigger: "deploy",
      actor: "anyone",
      input: "",
      expectedVersionId: stale,
    }),
    revisionMissing: await run(stale, { trigger: "deploy", actor: "anyone", input: "" }),
    triggerMissing: await run(current, { trigger: "absent", actor: "anyone", input: "" }),
    actorForbidden: await run(current, { trigger: "rollback", actor: "mallory", input: "" }),
    actorAllowed: await run(current, { trigger: "rollback", actor: "alice", input: "" }),
    noUserFilter: await run(current, { trigger: "open", actor: "alice", input: "" }),
    wrongEventTrigger: await run(current, { trigger: "cron", actor: "alice", input: "" }),
  };
}

async function publicManualRunTrace() {
  const dispatched = {
    status: "dispatched",
    deliveryKey: "delivery-1",
    providerEventReceiptId: "845e9d26-7977-45e1-bc69-d80a7b55a9cc",
    triggerRunId: "f83dc934-02a0-4849-8de7-699110be24ed",
    configuredTriggerName: "deploy",
    workflowStatus: "running",
  };
  const results: Record<string, unknown> = {
    dispatched,
    project_not_found: { status: "project_not_found" },
    actor_forbidden: { status: "actor_forbidden" },
    daemon_offline: { status: "daemon_offline" },
    expected_configuration_not_current: { status: "expected_configuration_not_current" },
    configuration_not_found: { status: "configuration_not_found" },
    trigger_not_found: { status: "trigger_not_found" },
    dispatch_conflict: { status: "dispatch_conflict" },
    invalid_input: {
      status: "invalid_input",
      providerEventReceiptId: "845e9d26-7977-45e1-bc69-d80a7b55a9cc",
      triggerRunId: "f83dc934-02a0-4849-8de7-699110be24ed",
      configuredTriggerName: "deploy",
      issues: [],
    },
    infrastructure_unavailable: { status: "infrastructure_unavailable" },
  };
  const operations = (result: unknown) =>
    ({ dispatchManualRun: () => Promise.resolve(result) }) as unknown as PublicOperations;
  const authenticator = (outcome: "authorized" | "unauthorized" | "forbidden" | "unavailable") => ({
    authorize(_request: Request, requiredScope: string) {
      if (outcome === "unavailable") return Promise.reject(new DatabaseUnavailableError());
      if (outcome !== "authorized") return Promise.resolve({ status: outcome });
      return Promise.resolve({
        status: "authorized",
        access: {
          kind: "apiKey",
          credentialId: "key-1",
          organizationId: "organization-1",
          scopes: [requiredScope],
        },
      });
    },
  });
  const manualBody = JSON.stringify({
    projectSlug: "project",
    trigger: "deploy",
    actor: "alice",
    deliveryKey: "delivery-1",
    input: {},
  });
  const call = async (
    outcome: "authorized" | "unauthorized" | "forbidden" | "unavailable",
    result: unknown,
    options: { body?: string; contentType?: string | null } = {},
  ) => {
    const api = createPublicApi(
      { status: "enabled", authenticator: authenticator(outcome) as never },
      operations(result),
    );
    const headers = new Headers({ authorization: "Bearer valid" });
    if (options.contentType !== null) {
      headers.set("content-type", options.contentType ?? "application/json");
    }
    const response = await api.handle(
      new Request("https://hub.test/api/v1/manual-runs", {
        method: "POST",
        headers,
        body: options.body ?? manualBody,
      }),
    );
    const parsed = (await response.json()) as Record<string, unknown>;
    return {
      status: response.status,
      code: parsed["code"] ?? null,
      contentType: response.headers.get("content-type"),
      wwwAuthenticate: response.headers.get("www-authenticate"),
      body: response.status === 200 ? parsed : null,
    };
  };
  const mapped: Record<string, unknown> = {};
  for (const [name, result] of Object.entries(results)) {
    mapped[name] = await call("authorized", result);
  }
  return {
    results: mapped,
    unauthorized: await call("unauthorized", dispatched),
    forbidden: await call("forbidden", dispatched),
    authenticationUnavailable: await call("unavailable", dispatched),
    invalidJson: await call("authorized", dispatched, { body: "{not json" }),
    wrongContentType: await call("authorized", dispatched, { contentType: "text/plain" }),
    missingContentType: await call("authorized", dispatched, { contentType: null }),
  };
}

interface WebhookCase {
  name: string;
  secret?: string | undefined;
  handlers?: number;
  eventsPerAcceptance?: number;
  acceptFailure?: "unavailable" | "generic";
  deliveries: Array<{
    deliveryId?: string | null;
    eventType?: string | null;
    signature?: "valid" | "none" | "wrong-secret" | "truncated" | string;
    body: string | Uint8Array;
  }>;
}

const GITHUB_SECRET = "github-secret";
const REPOSITORY_BODY = {
  action: "created",
  installation: { id: 42 },
  repository: { id: 9001, full_name: "acme/widgets" },
};

function padded(bytes: number): string {
  const base = JSON.stringify({ ...REPOSITORY_BODY, pad: "" });
  return JSON.stringify({ ...REPOSITORY_BODY, pad: "x".repeat(bytes - base.length) });
}

function webhookCases(): WebhookCase[] {
  const valid = JSON.stringify(REPOSITORY_BODY);
  const ok = (name: string, body: string | Uint8Array, extra: Partial<WebhookCase> = {}): WebhookCase => ({
    name,
    ...extra,
    deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", body }],
  });
  const withHeaders = (
    name: string,
    headers: { deliveryId?: string | null; eventType?: string | null },
  ): WebhookCase => ({
    name,
    deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", ...headers, body: valid }],
  });
  return [
    ok("validDispatch", valid),
    ok("unconfiguredSecret", valid, { secret: undefined }),
    { name: "missingSignature", deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", signature: "none", body: valid }] },
    { name: "wrongSecretSignature", deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", signature: "wrong-secret", body: valid }] },
    { name: "truncatedSignature", deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", signature: "truncated", body: valid }] },
    { name: "signatureWithoutPrefix", deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", signature: "raw-hex", body: valid }] },
    { name: "tamperedBody", deliveries: [{ deliveryId: "d-1", eventType: "issue_comment", signature: "of:" + valid, body: `${valid} ` }] },
    ok("longSecret", valid, { secret: "s".repeat(100) }),
    ok("emptySecret", valid, { secret: "" }),
    ok("bodyAtLimit", padded(1_048_576)),
    ok("bodyOverLimit", padded(1_048_577)),
    withHeaders("missingDeliveryHeader", { deliveryId: null }),
    withHeaders("emptyDeliveryHeader", { deliveryId: "" }),
    withHeaders("missingEventHeader", { eventType: null }),
    withHeaders("emptyEventHeader", { eventType: "" }),
    withHeaders("deliveryHeaderAtLimit", { deliveryId: "d".repeat(128) }),
    withHeaders("deliveryHeaderOverLimit", { deliveryId: "d".repeat(129) }),
    withHeaders("eventHeaderOverLimit", { eventType: "e".repeat(129) }),
    ok("malformedJson", "{not json"),
    ok("invalidUtf8", new Uint8Array([0x7b, 0x22, 0xff, 0x22, 0x7d])),
    ok("utf8Bom", new Uint8Array([0xef, 0xbb, 0xbf, ...new TextEncoder().encode(valid)])),
    ok("arrayBody", "[]"),
    ok("nullBody", "null"),
    ok("stringBody", '"text"'),
    ok("numberBody", "7"),
    ok("missingInstallation", JSON.stringify({ repository: REPOSITORY_BODY.repository })),
    ok("installationIdString", JSON.stringify({ ...REPOSITORY_BODY, installation: { id: "42" } })),
    ok("installationWithoutId", JSON.stringify({ ...REPOSITORY_BODY, installation: {} })),
    ok("installationIdFractional", JSON.stringify({ ...REPOSITORY_BODY, installation: { id: 1.5 } })),
    ok("duplicateInstallationKey", '{"installation":{"id":"x"},"installation":{"id":7},"repository":{"id":1,"full_name":"a/b"}}'),
    ok("lifecycleInstallation", JSON.stringify({ installation: { id: 42 }, action: "created" }), {}),
    ok("lifecycleInstallationRepositories", JSON.stringify({ installation: { id: 42 }, action: "added" })),
    ok("noRepository", JSON.stringify({ installation: { id: 42 } })),
    ok("emptyRepositoryName", JSON.stringify({ installation: { id: 42 }, repository: { id: 1, full_name: "" } })),
    ok("repositoryWithoutId", JSON.stringify({ installation: { id: 42 }, repository: { full_name: "a/b" } })),
    ok("repositoryIdString", JSON.stringify({ installation: { id: 42 }, repository: { id: "1", full_name: "a/b" } })),
    ok("noHandlers", valid, { handlers: 0 }),
    ok("twoHandlers", valid, { handlers: 2 }),
    ok("fanOutTwoEvents", valid, { eventsPerAcceptance: 2 }),
    ok("storageUnavailable", valid, { acceptFailure: "unavailable" }),
    ok("storageFailure", valid, { acceptFailure: "generic" }),
    {
      name: "replayAndDistinct",
      deliveries: [
        { deliveryId: "d-1", eventType: "issue_comment", body: valid },
        { deliveryId: "d-1", eventType: "issue_comment", body: valid },
        { deliveryId: "d-2", eventType: "issue_comment", body: valid },
      ],
    },
  ].map((entry) => {
    if (entry.name === "lifecycleInstallation") {
      return { ...entry, deliveries: [{ ...entry.deliveries[0]!, eventType: "installation" }] };
    }
    if (entry.name === "lifecycleInstallationRepositories") {
      return { ...entry, deliveries: [{ ...entry.deliveries[0]!, eventType: "installation_repositories" }] };
    }
    return entry;
  });
}

function sign(secret: string, body: string | Uint8Array): string {
  return `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`;
}

function signatureFor(
  spec: WebhookCase["deliveries"][number]["signature"],
  secret: string,
  body: string | Uint8Array,
): string | null {
  switch (spec) {
    case undefined:
    case "valid":
      return sign(secret, body);
    case "none":
      return null;
    case "wrong-secret":
      return sign("other-secret", body);
    case "truncated":
      return sign(secret, body).slice(0, -2);
    case "raw-hex":
      return sign(secret, body).slice("sha256=".length);
    default:
      if (spec.startsWith("of:")) return sign(secret, spec.slice(3));
      throw new Error(`unknown signature spec: ${spec}`);
  }
}

async function githubTrace() {
  const results: Record<string, unknown> = {};
  for (const spec of webhookCases()) {
    const hasSecret = Object.hasOwn(spec, "secret") ? spec.secret : GITHUB_SECRET;
    const secret = hasSecret ?? undefined;
    const seen = new Map<string, string>();
    const accepts: unknown[] = [];
    const lifecycles: unknown[] = [];
    let dispatchCount = 0;
    const endpoint = createWebhookSource(secret, {
      async accept(input) {
        accepts.push({
          source: input.source,
          dropReason: input.dropReason ?? null,
          installationId: input.installationId,
          repositoryId: input.repositoryId ?? null,
          repo: input.repo ?? null,
          signatureHash: input.signatureHash,
        });
        if (spec.acceptFailure === "unavailable") throw new DatabaseUnavailableError();
        if (spec.acceptFailure === "generic") throw new Error("boom");
        const existing = seen.get(input.deliveryId);
        if (existing) return { status: "duplicate", receiptId: existing };
        const receipt = `receipt-${seen.size + 1}`;
        seen.set(input.deliveryId, receipt);
        if (input.dropReason !== undefined) {
          return { status: "dropped", receiptId: receipt, reason: input.dropReason };
        }
        const events = Array.from({ length: spec.eventsPerAcceptance ?? 1 }, (_, index) => ({
          providerEventReceiptId: `${receipt}-${index}`,
          organizationId: "org-github",
          projectId: `project-${index}`,
          configurationRevisionId: "revision-github",
          source: input.source,
          deliveryId: input.deliveryId,
          receivedAt: input.receivedAt,
          payload: input.payload,
          connectionId: null,
          resourceId: String(input.repositoryId),
        }));
        return { status: "accepted", receiptId: receipt, events };
      },
      async applyLifecycle(input) {
        lifecycles.push({
          event: input.event,
          source: input.source,
          installationId: input.installationId,
          signatureHash: input.signatureHash,
        });
      },
    });
    for (let index = 0; index < (spec.handlers ?? 1); index += 1) {
      await endpoint.start(async () => {
        dispatchCount += 1;
      });
    }
    const responses: Array<{ status: number; body: string }> = [];
    for (const delivery of spec.deliveries) {
      const headers = new Headers({ "content-type": "application/json" });
      if (delivery.deliveryId !== null && delivery.deliveryId !== undefined) {
        headers.set("x-github-delivery", delivery.deliveryId);
      }
      if (delivery.eventType !== null && delivery.eventType !== undefined) {
        headers.set("x-github-event", delivery.eventType);
      }
      const signature = signatureFor(delivery.signature, secret ?? GITHUB_SECRET, delivery.body);
      if (signature !== null) headers.set("x-hub-signature-256", signature);
      const response = await endpoint.handle(
        new Request("http://localhost/webhook", { method: "POST", headers, body: delivery.body }),
      );
      responses.push({ status: response.status, body: await response.text() });
    }
    results[spec.name] = { responses, accepts, lifecycles, dispatchCount };
  }
  return {
    signatureHashSample: createHash("sha256").update(sign(GITHUB_SECRET, "{}")).digest("hex"),
    cases: results,
  };
}
