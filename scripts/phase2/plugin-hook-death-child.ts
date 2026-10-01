import path from "node:path";
import { pathToFileURL } from "node:url";

async function main() {
  const root = process.env.PASEO_REFERENCE_ROOT;
  if (!root) throw new Error("PASEO_REFERENCE_ROOT is required");
  const lifecycleUrl = pathToFileURL(
    path.join(root, "packages/server/src/server/plugins/lifecycle/index.ts"),
  ).href;
  const { PluginHookHandlers } = await import(lifecycleUrl);
  const hooks = new PluginHookHandlers(() => {});
  hooks.on("workspace.created", () => process.exit(17));
  await hooks.invoke(
    "death",
    "event",
    "workspace.created",
    { workspace: { id: "workspace" } },
    {},
  );
}

void main();
