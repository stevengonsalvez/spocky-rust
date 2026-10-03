import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { it } from "vitest";
import { startProductionRuntime, stopProductionRuntime } from "./index.js";

// Replays the shared account route case list against the pinned Hub's production runtime on its
// embedded PGlite. Every traced step is a raw Request through `runtime.auth`, the same entry the
// `/api/auth/$` route calls. Generated values are replaced by their names before the trace is
// written; nothing else is rewritten.

const TEMPORARY_PASSWORD = "temporary-password";
const REPLACEMENT_PASSWORD = "replacement-password";
const ENVIRONMENT_NAMES = [
  "DATABASE_URL",
  "PASEO_HUB_DATA_DIR",
  "PASEO_HUB_AUTH_SECRET",
  "PASEO_HUB_APP_URL",
  "PASEO_REGISTRATION_MODE",
  "PASEO_ORGANIZATION_CREATION",
  "PASEO_BOOTSTRAP_ORGANIZATION",
  "PASEO_BOOTSTRAP_OWNER_EMAIL",
  "PASEO_BOOTSTRAP_OWNER_PASSWORD",
] as const;

type Runtime = Awaited<ReturnType<typeof startProductionRuntime>>;
interface Step {
  kind: "setup" | "request";
  [key: string]: unknown;
}
interface Scenario {
  id: string;
  env: { registration: string; organizationCreation: string; bootstrap?: boolean };
  steps: Step[];
}

it("captures the pinned account, organization and API key routes", async () => {
  const casesPath = process.env["SPOCKY_HUB_ACCOUNT_CASES"];
  const output = process.env["SPOCKY_HUB_ACCOUNT_OUTPUT"];
  assert.ok(casesPath, "SPOCKY_HUB_ACCOUNT_CASES is required");
  assert.ok(output, "SPOCKY_HUB_ACCOUNT_OUTPUT is required");
  const spec = JSON.parse(await readFile(casesPath, "utf8")) as {
    appUrl: string;
    scenarios: Scenario[];
  };
  const scenarios: unknown[] = [];
  for (const scenario of spec.scenarios) {
    scenarios.push(await runScenario(spec.appUrl, scenario));
  }
  await writeFile(
    output,
    `${JSON.stringify({ schemaVersion: 1, baseline: process.env["PASEO_HUB_BASELINE"], scenarios }, null, 2)}\n`,
  );
}, 600_000);

async function runScenario(appUrl: string, scenario: Scenario): Promise<unknown> {
  const root = await mkdtemp(join(tmpdir(), "hub-account-routes-original-"));
  const previous = new Map(ENVIRONMENT_NAMES.map((name) => [name, process.env[name]]));
  delete process.env["DATABASE_URL"];
  process.env["PASEO_HUB_DATA_DIR"] = join(root, "database");
  process.env["PASEO_HUB_AUTH_SECRET"] = "account-routes-original-secret-at-least-32-characters";
  process.env["PASEO_HUB_APP_URL"] = appUrl;
  process.env["PASEO_REGISTRATION_MODE"] = scenario.env.registration;
  process.env["PASEO_ORGANIZATION_CREATION"] = scenario.env.organizationCreation;
  if (scenario.env.bootstrap === true) {
    process.env["PASEO_BOOTSTRAP_ORGANIZATION"] = "organization-1";
    process.env["PASEO_BOOTSTRAP_OWNER_EMAIL"] = "owner@example.test";
    process.env["PASEO_BOOTSTRAP_OWNER_PASSWORD"] = TEMPORARY_PASSWORD;
  } else {
    delete process.env["PASEO_BOOTSTRAP_ORGANIZATION"];
    delete process.env["PASEO_BOOTSTRAP_OWNER_EMAIL"];
    delete process.env["PASEO_BOOTSTRAP_OWNER_PASSWORD"];
  }
  try {
    const runtime = await startProductionRuntime();
    return { id: scenario.id, steps: await replay(runtime, appUrl, scenario) };
  } finally {
    await stopProductionRuntime().catch(() => undefined);
    for (const [name, value] of previous) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    await rm(root, { recursive: true, force: true });
  }
}

async function replay(runtime: Runtime, appUrl: string, scenario: Scenario): Promise<unknown[]> {
  const cookies = new Map<string, string>();
  const values = new Map<string, string>();
  const trace: unknown[] = [];

  const resolve = (template: string): string =>
    template.replace(/\{\{([^}]+)\}\}/gu, (_whole, name: string) => {
      const value = values.get(name);
      assert.ok(value !== undefined, `unresolved template value ${name}`);
      return value;
    });

  // Learn each actor's account and membership id as soon as they exist, with unrecorded reads, so a
  // value is known before a later step removes the membership it names.
  const learn = async (): Promise<void> => {
    for (const [actor, cookie] of cookies) {
      if (!values.has(`${actor}.account`)) {
        const response = await runtime.auth(
          new Request(`${appUrl}/api/auth/get-session`, { headers: { cookie } }),
        );
        const body = (await response.json()) as { user?: { id?: string } } | null;
        const id = body?.user?.id;
        if (id !== undefined) values.set(`${actor}.account`, id);
      }
      if (!values.has(`${actor}.member`)) {
        const response = await runtime.auth(
          new Request(`${appUrl}/api/auth/paseo/state`, { headers: { cookie } }),
        );
        const body = (await response.json()) as {
          membership?: { id?: string };
          organization?: { id?: string; slug?: string };
        };
        const id = body.membership?.id;
        if (id !== undefined) values.set(`${actor}.member`, id);
        // The owner's organization is the one the scenario operates on.
        const organization = body.organization;
        if (actor === "owner" && organization?.id !== undefined && !values.has("org")) {
          values.set("org", organization.id);
          if (organization.slug !== undefined) values.set("orgSlug", organization.slug);
        }
      }
    }
  };

  for (const step of scenario.steps) {
    if (step.kind === "setup") {
      await setup(runtime, appUrl, step, cookies, resolve);
      await learn();
      continue;
    }
    const actor = step["actor"] as string;
    const headers = new Headers();
    const cookie = cookies.get(actor);
    if (cookie !== undefined) {
      headers.set("cookie", cookie);
      headers.set("origin", appUrl);
    }
    const body = step["body"] as string | null;
    if (body !== null) headers.set("content-type", "application/json");
    for (const [name, value] of Object.entries((step["headers"] ?? {}) as Record<string, string | null>)) {
      if (value === null) headers.delete(name);
      else headers.set(name, resolve(value));
    }
    const query = step["query"] === undefined ? "" : resolve(step["query"] as string);
    const path = `${resolve(step["path"] as string)}${query}`;
    const startedAt = Date.now();
    const response = await runtime.auth(
      new Request(`${appUrl}${path}`, {
        method: step["method"] as string,
        headers,
        ...(body === null ? {} : { body: resolve(body) }),
      }),
    );
    const text = await response.text();
    for (const [name, pointer] of Object.entries((step["capture"] ?? {}) as Record<string, string>)) {
      const value = pointer.split(".").reduce<unknown>(
        (node, key) => (node as Record<string, unknown> | undefined)?.[key],
        JSON.parse(text),
      );
      assert.equal(typeof value, "string", `capture ${name} from ${step["id"] as string}`);
      values.set(name, value as string);
    }
    await learn();
    trace.push({
      id: step["id"],
      status: response.status,
      headers: [...response.headers.entries()],
      body: mask(text, values, startedAt),
    });
  }
  return trace;
}

async function setup(
  runtime: Runtime,
  appUrl: string,
  step: Step,
  cookies: Map<string, string>,
  resolve: (template: string) => string,
): Promise<void> {
  const actor = step["actor"] as string;
  const op = step["op"] as string;
  if (op === "createAccount") {
    const email = step["email"] as string;
    const password = step["password"] as string;
    const body: Record<string, string> = { name: step["name"] as string, email, password };
    if (typeof step["invitation"] === "string") body["invitation"] = resolve(step["invitation"]);
    const signedUp = await post(runtime, appUrl, "/api/auth/sign-up/email", body);
    assert.equal(signedUp.status, 200, `sign-up ${actor}`);
    cookies.set(actor, await signIn(runtime, appUrl, email, password));
    return;
  }
  assert.equal(actor, "owner");
  if (op === "bootstrapOwnerSignIn") {
    cookies.set(actor, await signIn(runtime, appUrl, "owner@example.test", TEMPORARY_PASSWORD));
    return;
  }
  assert.equal(op, "bootstrapOwnerReady");
  const cookie = cookies.get(actor);
  assert.ok(cookie);
  await runtime.changePassword!(
    { currentPassword: TEMPORARY_PASSWORD, newPassword: REPLACEMENT_PASSWORD },
    new Headers({ cookie, origin: appUrl }),
  );
  const replaced = await signIn(runtime, appUrl, "owner@example.test", REPLACEMENT_PASSWORD);
  cookies.set(actor, replaced);
  await runtime.completeAppOnboarding!(
    new Request(`${appUrl}/api/auth/paseo/complete-app-setup`, {
      method: "POST",
      headers: { cookie: replaced, origin: appUrl },
    }),
  );
}

function post(runtime: Runtime, appUrl: string, path: string, body: unknown): Promise<Response> {
  return runtime.auth(
    new Request(`${appUrl}${path}`, {
      method: "POST",
      headers: { origin: appUrl, "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

async function signIn(
  runtime: Runtime,
  appUrl: string,
  email: string,
  password: string,
): Promise<string> {
  const response = await post(runtime, appUrl, "/api/auth/sign-in/email", { email, password });
  assert.equal(response.status, 200, `sign-in ${email}`);
  const cookie = response.headers.get("set-cookie")?.match(/^(?:[^;]+);/u)?.[0]?.slice(0, -1);
  assert.ok(cookie, "sign-in did not issue a session cookie");
  return cookie;
}

// Generated identity and wall clock only: every known generated value becomes its name, and an
// ISO timestamp becomes its whole-hour offset from the request.
function mask(text: string, values: Map<string, string>, startedAt: number): string {
  let masked = text;
  const known = [...values.entries()].sort((a, b) => b[1].length - a[1].length);
  for (const [name, value] of known) masked = masked.split(value).join(`<${name}>`);
  return masked.replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/gu, (iso) => {
    const hours = Math.round((Date.parse(iso) - startedAt) / 3_600_000);
    return `<wall-clock${hours >= 0 ? "+" : ""}${hours}h>`;
  });
}
