import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { test, vi } from "vitest";

const root = process.env.PASEO_REFERENCE_ROOT;
if (!root) throw new Error("PASEO_REFERENCE_ROOT is required");
const workerUrl = pathToFileURL(
  path.join(root, "packages/server/src/server/plugins/plugin-process.ts"),
).href;

test("captures pinned hook usage and provider worker behavior", async () => {
  const { createPluginWorker } = await import(workerUrl);
  const messages: unknown[] = [];
  const handlers = new Set<(message: unknown) => void>();
  const eventOrder: string[] = [];
  let providerListener = (_event: unknown) => {};
  const channel = {
    send(message: unknown, callback?: () => void) {
      messages.push(structuredClone(message));
      callback?.();
    },
    onMessage(handler: (message: unknown) => void) {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
    disconnect() {},
  };
  const worker = createPluginWorker({
    channel,
    contribute(server: any) {
      server.handle({ name: "event.order" }, async () => eventOrder);
      server.before("workspace.create", ({ request }: any) => ({ ...request, title: "first" }));
      server.before("workspace.create", ({ request }: any) => ({
        ...request,
        title: `${request.title}:second`,
      }));
      server.on("workspace.created", () => {
        eventOrder.push("failed");
        throw new Error("observer failed");
      });
      server.on("workspace.created", () => eventOrder.push("continued"));
      server.before(
        "agent.session_open",
        (_input: unknown, { signal }: { signal: AbortSignal }) =>
          new Promise((_resolve, reject) =>
            signal.addEventListener("abort", () => reject(new Error("hook canceled")), {
              once: true,
            }),
          ),
      );
      server.registerUsageSource({
        id: "credits",
        label: "Credits",
        input: {
          async parseAsync(value: any) {
            if (typeof value?.account !== "string") throw new Error("invalid account payload");
            return value;
          },
        },
        async identify(input: any) {
          return { key: input.account, label: "Work" };
        },
        async fetch(input: any) {
          return {
            status: "available",
            windows: [{ id: "daily", label: input.account, usedPct: 25 }],
          };
        },
        async discover() {
          return [{ account: "discovered" }];
        },
      });
      server.registerProvider({
        id: "direct",
        label: "Direct",
        async getCatalogCacheKey(options: any) {
          return `${options.scope}:shared`;
        },
        async connect(request: any) {
          if (request.capabilities.includes("fail")) throw new Error("connect failed");
          return {
            version: 1,
            capabilities: ["session.list"],
            async send(input: any) {
              if (input.type === "catalog") throw new Error("send rejected");
              providerListener({ type: "request.completed", requestId: input.requestId });
            },
            onEvent(listener: (event: unknown) => void) {
              providerListener = listener;
              return () => {
                providerListener = () => {};
              };
            },
            async close() {},
          };
        },
      });
      return () => {};
    },
  });

  const send = (message: unknown) => {
    for (const handler of handlers) handler(message);
  };
  const take = async (type: string, id?: string): Promise<any> => {
    await vi.waitFor(
      () => {
        const found = messages.some(
          (message: any) =>
            message.type === type &&
            (id === undefined || message.requestId === id || message.connectionId === id),
        );
        if (!found) throw new Error(`missing ${type}:${id ?? ""} in ${JSON.stringify(messages)}`);
      },
      { timeout: 2000 },
    );
    const index = messages.findIndex(
      (message: any) =>
        message.type === type &&
        (id === undefined || message.requestId === id || message.connectionId === id),
    );
    return messages.splice(index, 1)[0];
  };

  send({
    type: "initialize",
    pluginId: "selected",
    bundle: "",
    appVersion: "0.8.0",
    pluginDirectory: "/plugin",
  });
  const ready = await take("ready");
  messages.length = 0;

  send({
    type: "hook",
    requestId: "before-success",
    kind: "before",
    name: "workspace.create",
    input: { source: { kind: "directory", path: "/project" } },
  });
  const beforeSuccess = await take("result", "before-success");

  const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
  send({
    type: "hook",
    requestId: "event-failure",
    kind: "event",
    name: "workspace.created",
    input: { workspace: { id: "workspace" } },
  });
  const eventFailure = await take("result", "event-failure");
  consoleError.mockRestore();

  send({
    type: "hook",
    requestId: "hook-timeout",
    kind: "before",
    name: "agent.session_open",
    input: {
      agentId: "agent",
      workspaceId: null,
      provider: "direct",
      cwd: "/project",
      reason: "resume",
      purpose: "interactive",
      env: {},
    },
  });
  send({ type: "hook.cancel", requestId: "hook-timeout" });
  const hookCanceled = await take("error", "hook-timeout");

  send({
    type: "usage.identify",
    requestId: "usage-identify",
    sourceId: "credits",
    input: { account: "work" },
  });
  const usageIdentify = await take("result", "usage-identify");
  send({
    type: "usage.fetch",
    requestId: "usage-fetch",
    sourceId: "credits",
    input: { account: "work" },
  });
  const usageFetch = await take("result", "usage-fetch");
  send({ type: "usage.discover", requestId: "usage-discover", sourceId: "credits" });
  const usageDiscover = await take("result", "usage-discover");
  send({
    type: "usage.fetch",
    requestId: "usage-malformed",
    sourceId: "credits",
    input: { account: 7 },
  });
  const usageMalformed = await take("error", "usage-malformed");

  send({
    type: "provider.catalog_key",
    requestId: "catalog-key",
    providerId: "direct",
    options: { scope: "global" },
  });
  const catalogKey = await take("result", "catalog-key");
  send({
    type: "provider.connect",
    providerId: "direct",
    connectionId: "connection",
    request: { versions: [1], capabilities: [] },
  });
  const connected = await take("provider.connected", "connection");
  send({
    type: "provider.send",
    connectionId: "connection",
    acceptanceId: "accepted",
    input: { type: "sessions", requestId: "sessions" },
  });
  const providerEvent = await take("provider.event", "connection");
  const accepted = await take("provider.accepted", "connection");
  send({
    type: "provider.send",
    connectionId: "connection",
    acceptanceId: "rejected",
    input: { type: "catalog", requestId: "catalog" },
  });
  const rejected = await take("provider.rejected", "connection");
  send({ type: "provider.close", connectionId: "connection" });
  const closed = await take("provider.closed", "connection");
  send({
    type: "provider.connect",
    providerId: "direct",
    connectionId: "connection-failed",
    request: { versions: [1], capabilities: ["fail"] },
  });
  const connectFailed = await take("provider.connect_failed", "connection-failed");

  await worker.shutdown();
  const capture = {
    ready,
    beforeSuccess,
    eventFailure,
    eventOrder,
    hookCanceled,
    usageIdentify,
    usageFetch,
    usageDiscover,
    usageMalformed,
    catalogKey,
    connected,
    providerEvent,
    accepted,
    rejected,
    closed,
    connectFailed,
  };
  console.log(`PLUGIN_HOOK_BASELINE ${JSON.stringify(capture)}`);
}, 30_000);

test("surfaces process death while an original hook runs", () => {
  const tsx = path.join(root, "node_modules/.bin/tsx");
  const child = spawnSync(tsx, [path.join(import.meta.dirname, "plugin-hook-death-child.ts")], {
    cwd: root,
    env: { ...process.env, PASEO_REFERENCE_ROOT: root },
    encoding: "utf8",
    timeout: 10_000,
  });
  if (child.error) throw child.error;
  if (child.status !== 17) {
    throw new Error(`expected hook child exit 17, got ${child.status}: ${child.stderr}`);
  }
  console.log(
    `PLUGIN_HOOK_BASELINE_FAILURES ${JSON.stringify({ timeout: "terminated", processDeath: "rejected" })}`,
  );
});
