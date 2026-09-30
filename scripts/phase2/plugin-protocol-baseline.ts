import { pathToFileURL } from "node:url";
import path from "node:path";

const root = process.env.PASEO_REFERENCE_ROOT;
if (!root) throw new Error("PASEO_REFERENCE_ROOT is required");

const commit = process.env.PASEO_REFERENCE_COMMIT;
if (commit !== "5de45e208690b0efc51c59a585ae9729325a9204") {
  throw new Error(`unexpected Paseo baseline: ${commit}`);
}

const importSource = async (relativePath: string) =>
  import(pathToFileURL(path.join(root, relativePath)).href);

async function main() {
  const processProtocol = await importSource(
    "packages/server/src/server/plugins/plugin-process-protocol.ts",
  );
  const messages = await importSource("packages/protocol/src/messages.ts");

  const values = {
  workerInitialize: processProtocol.PluginProcessRequestSchema.parse({
    type: "initialize",
    pluginId: "review",
    bundle: "bundle",
    appVersion: "0.8.0",
    pluginDirectory: "/plugins/review",
    settingsDirectory: "/settings/review",
  }),
  workerReady: processProtocol.PluginProcessMessageSchema.parse({
    type: "ready",
    methods: ["review.start"],
    providers: [],
    usageSources: [],
    hooks: { events: [], before: [] },
  }),
  workerInvoke: processProtocol.PluginProcessRequestSchema.parse({
    type: "invoke",
    requestId: "worker-1",
    method: "review.start",
    input: { change: 7 },
  }),
  workerResult: processProtocol.PluginProcessMessageSchema.parse({
    type: "result",
    requestId: "worker-1",
    output: { accepted: true },
  }),
  workerShutdown: processProtocol.PluginProcessRequestSchema.parse({ type: "shutdown" }),
  catalogRequest: messages.SessionInboundMessageSchema.parse({
    type: "plugin.catalog.get.request",
    requestId: "catalog-1",
  }),
  catalogResponse: messages.SessionOutboundMessageSchema.parse({
    type: "plugin.catalog.get.response",
    payload: {
      requestId: "catalog-1",
      plugins: [
        {
          id: "review",
          clientBundle: "bundle",
          requirements: { paseo: ">=0.8.0" },
        },
      ],
    },
  }),
  invokeRequest: messages.SessionInboundMessageSchema.parse({
    type: "plugin.rpc.invoke.request",
    requestId: "rpc-1",
    pluginId: "review",
    method: "review.start",
    input: { change: 7 },
  }),
  invokeResponse: messages.SessionOutboundMessageSchema.parse({
    type: "plugin.rpc.invoke.response",
    payload: { requestId: "rpc-1", output: { accepted: true } },
  }),
  settingsChanged: messages.StatusMessageSchema.parse({
    type: "status",
    payload: {
      status: "plugin_settings_changed",
      pluginId: "review",
      settingsId: "display",
    },
  }),
  };

  process.stdout.write(`PLUGIN_BASELINE_PROTOCOL ${JSON.stringify(values)}\n`);
}

void main();
