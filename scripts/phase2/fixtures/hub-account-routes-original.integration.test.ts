import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { it } from "vitest";
import { startProductionRuntime, stopProductionRuntime } from "./index.js";

// Replays the shared account route case list against the pinned Hub's production runtime on its
// embedded PGlite. Every traced step is a raw Request through `runtime.auth`, the same entry the
// `/api/auth/$` route calls. Only generated identity and wall-clock values are replaced before the
// trace is written (see `mask` and the header of the case generator); nothing else is rewritten.

const PINNED_NODE = "v22.20.0";
const API_KEY_PREFIX = /^paseo_pk_[A-Za-z0-9_-]{12}$/u;
const API_KEY_RANDOM = /^[A-Za-z0-9_-]{43}$/u;
const NOMINAL_LIFETIMES_MS = [0, 48 * 3_600_000];

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

interface Window {
  ordinal: string;
  startedAt: number;
  endedAt: number;
}

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
  assert.equal(process.version, PINNED_NODE, "the capture runs on the node the Hub ships");
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
  const clock = new Map<string, string>();
  const windows: Window[] = [];

  const resolve = (template: string): string =>
    template.replace(/\{\{([^}]+)\}\}/gu, (_whole, name: string) => {
      const value = values.get(name);
      assert.ok(value !== undefined, `unresolved template value ${name}`);
      return value;
    });

  // Learn each actor's account and membership id as soon as they exist, so a value is known before
  // a later step removes the membership it names. The only hidden reads are `paseo/state`; the Rust
  // replay issues the same ones through its own handler, and their only write is the bootstrap
  // organization activation a recorded state read performs too.
  const learn = async (): Promise<void> => {
    for (const [actor, cookie] of cookies) {
      if (values.has(`${actor}.account`) && values.has(`${actor}.member`)) continue;
      const response = await runtime.auth(
        new Request(`${appUrl}/api/auth/paseo/state`, { headers: { cookie } }),
      );
      const body = (await response.json()) as {
        account?: { id?: string };
        membership?: { id?: string };
        organization?: { id?: string };
      };
      const account = body.account?.id;
      if (account !== undefined && !values.has(`${actor}.account`)) {
        values.set(`${actor}.account`, account);
      }
      const member = body.membership?.id;
      if (member !== undefined && !values.has(`${actor}.member`)) {
        values.set(`${actor}.member`, member);
      }
      // The owner's organization is the one the scenario operates on.
      const organization = body.organization?.id;
      if (actor === "owner" && organization !== undefined && !values.has("org")) {
        values.set("org", organization);
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
    const endedAt = Date.now();
    capture(step, text, values);
    windows.push({ ordinal: ordinalOf(step["id"] as string), startedAt, endedAt });
    await learn();
    trace.push({
      id: step["id"],
      status: response.status,
      headers: [...response.headers.entries()],
      body: mask(text, values, clock, windows),
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

// Reads the captures of a step. An API key part keeps its exact prefix and format: the capture holds
// only the random part, after the prefix, the format, the length and the charset are asserted.
function capture(step: Step, text: string, values: Map<string, string>): void {
  const captures = (step["capture"] ?? {}) as Record<string, string>;
  const found = new Map<string, string>();
  for (const [name, pointer] of Object.entries(captures)) {
    const value = pointer.split(".").reduce<unknown>(
      (node, key) => (node as Record<string, unknown> | undefined)?.[key],
      JSON.parse(text),
    );
    assert.equal(typeof value, "string", `capture ${name} from ${step["id"] as string}`);
    found.set(name, value as string);
  }
  for (const [name, value] of found) {
    if (!name.startsWith("prefix")) continue;
    assert.match(value, API_KEY_PREFIX, `API key prefix ${name}`);
    values.set(name, value.slice("paseo_pk_".length));
  }
  for (const [name, value] of found) {
    if (name.startsWith("prefix")) continue;
    if (!name.startsWith("secret")) {
      values.set(name, value);
      continue;
    }
    const partner = values.get(`prefix${name.slice("secret".length)}`);
    assert.ok(partner !== undefined, `secret ${name} needs its prefix capture`);
    const random = value.slice(`paseo_pk_${partner}_`.length);
    assert.equal(value, `paseo_pk_${partner}_${random}`, `API key secret format ${name}`);
    assert.match(random, API_KEY_RANDOM, `API key secret ${name}`);
    values.set(name, random);
  }
}

// The step number inside a step id such as `open/192-admin-api-keys-created-admin`.
function ordinalOf(id: string): string {
  const match = /\/(\d+)-/u.exec(id);
  assert.ok(match?.[1], `step id ${id}`);
  return match[1];
}

// Generated identity and wall clock only. A captured value becomes its name; the first 8 characters
// of a captured UUID become `<name:8>` (the generated part of an organization slug). A timestamp is
// paired with the one request whose window, shifted by a nominal lifetime, contains it, and becomes
// `<wall-clock@STEP+Nms>`; the first response that shows a timestamp may be a later request than
// the one that wrote it (a revocation shows in the next listing). A timestamp that fits no window
// stays raw, so a wrong lifetime shows in the trace; one that fits two windows fails the capture.
function mask(
  text: string,
  values: Map<string, string>,
  clock: Map<string, string>,
  windows: readonly Window[],
): string {
  const known: [string, string][] = [];
  for (const [name, value] of values) {
    known.push([value, `<${name}>`]);
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u.test(value)) {
      known.push([value.slice(0, 8), `<${name}:8>`]);
    }
  }
  known.sort((a, b) => b[0].length - a[0].length);
  let masked = text;
  for (const [value, label] of known) masked = masked.split(value).join(label);
  return masked.replace(/\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/gu, (iso) => {
    const seen = clock.get(iso);
    if (seen !== undefined) return seen;
    const at = Date.parse(iso);
    const matches = windows.flatMap((window) =>
      NOMINAL_LIFETIMES_MS.filter(
        (offset) => window.startedAt + offset <= at && at <= window.endedAt + offset,
      ).map((offset) => `<wall-clock@${window.ordinal}+${offset}ms>`),
    );
    assert.ok(matches.length <= 1, `timestamp ${iso} fits more than one request window`);
    const label = matches[0];
    if (label === undefined) return iso;
    clock.set(iso, label);
    return label;
  });
}
