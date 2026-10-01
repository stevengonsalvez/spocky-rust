import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, expect, test } from "vitest";
import { defineSettings } from "@getpaseo/plugin";
import { z } from "zod";
import { PluginSettingsStore } from "@paseo-settings-source";

const roots: string[] = [];
afterAll(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

const schema = z
  .object({
    items: z.array(z.object({ name: z.string().min(3), weight: z.number().min(0) })).min(1),
    label: z.string().default("ready"),
    mode: z.enum(["fast", "safe"]).default("safe"),
  })
  .superRefine(async (value, context) => {
    await Promise.resolve();
    if (value.items[0]?.name === "bad")
      context.addIssue({ code: "custom", message: "name rejected asynchronously" });
  });

const definition = defineSettings({ id: "rich", scope: "host", version: 1, schema });
const integerDefinition = defineSettings({
  id: "display",
  scope: "host",
  version: 1,
  schema: z.object({
    count: z.number().int().min(1).default(5),
    enabled: z.boolean().default(true),
  }),
});

async function setup() {
  const directory = await mkdtemp(path.join(tmpdir(), "plugin-settings-differential-"));
  roots.push(directory);
  const reports: string[] = [];
  const original = console.error;
  console.error = (prefix: unknown, error: unknown) => {
    reports.push(`${String(prefix)}: ${error instanceof Error ? error.message : String(error)}`);
  };
  const handlers = new PluginSettingsStore(directory, () => {}).register(definition);
  return { handlers, reports, restore: () => (console.error = original) };
}

test("captures pinned settings schema behavior", async () => {
  const cases: unknown[] = [];
  for (const [name, values] of [
    ["array", { items: [] }],
    ["enum", { items: [{ name: "valid", weight: 1 }], mode: "other" }],
    ["required", { items: [{ weight: 1 }] }],
    ["string", { items: [{ name: "x", weight: 1.5 }] }],
    ["number", { items: [{ name: "valid", weight: -1 }] }],
    ["refinement", { items: [{ name: "bad", weight: 1 }] }],
    ["multiple", { items: [{ name: "x", weight: -1 }], mode: "other" }],
  ] as const) {
    const { handlers, restore } = await setup();
    cases.push({ name, result: await handlers.write.handle({ revision: "missing", values }) });
    restore();
  }
  for (const [name, count] of [
    ["integer-minimum", -1],
    ["integer-fraction", 1.5],
    ["integer-type", "1"],
  ] as const) {
    const directory = await mkdtemp(path.join(tmpdir(), "plugin-settings-differential-"));
    roots.push(directory);
    const handlers = new PluginSettingsStore(directory, () => {}).register(integerDefinition);
    cases.push({
      name,
      result: await handlers.write.handle({ revision: "missing", values: { count } }),
    });
  }

  const callback = await setup();
  callback.handlers.settings.subscribe(async () => {
    await Promise.resolve();
    throw new Error("subscriber rejected");
  });
  const saved = await callback.handlers.write.handle({
    revision: "missing",
    values: { items: [{ name: "valid", weight: 1.5, ignored: true }], extra: true },
  });
  await new Promise((resolve) => setImmediate(resolve));
  cases.push({ name: "saved", reports: callback.reports, result: saved });
  callback.restore();

  expect(cases).toHaveLength(11);
  console.log(`PLUGIN_SETTINGS_BASELINE ${JSON.stringify(cases)}`);
});
