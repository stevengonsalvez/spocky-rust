import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { test, vi } from "vitest";
import { OrganizationApiKeys } from "../src/auth/api-keys.js";
import { ProductRequestError } from "../src/auth/organization-access.js";
import { OrganizationCliCredentials, cliCredentialParts } from "../src/auth/cli-credentials.js";
import { PublicCredentialAuthenticator } from "../src/auth/public-credentials.js";
import { CliAuthorizations } from "../src/cli-authorizations/index.js";
import { createMemoryDatabase } from "../src/db/memory.js";
import { DatabaseUnavailableError } from "../src/db/errors.js";
import { createPublicApi, publicOpenApiDocument, publicOperationManifest } from "../src/public-api/index.js";

// Deterministic generated values keep the trace reproducible byte for byte. The Rust differential
// derives the same sequences: UUID n is 00000000-0000-4000-8000-<n in 12 hex digits>, and the
// n-th randomBytes call returns the first `size` bytes of SHA-256("spocky-hub-api-random:<n>"). A
// scenario with `repeatUserCode` makes every 8-byte draw return the first 8-byte draw.
vi.mock("node:crypto", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:crypto")>();
  const state = globalThis as unknown as {
    __hubApiUuid: number;
    __hubApiBytes: number;
    __hubApiRepeatEight: boolean;
    __hubApiEight: Buffer | undefined;
  };
  const randomUUID = () =>
    `00000000-0000-4000-8000-${(++state.__hubApiUuid).toString(16).padStart(12, "0")}`;
  const randomBytes = (size: number) => {
    state.__hubApiBytes += 1;
    if (size > 32) throw new Error("deterministic randomBytes supports at most 32 bytes");
    const bytes = actual
      .createHash("sha256")
      .update(`spocky-hub-api-random:${state.__hubApiBytes}`)
      .digest()
      .subarray(0, size);
    // A scenario can make every 8-byte draw (a user code) repeat the first one, which forces user
    // code collisions between authorizations.
    if (size === 8 && state.__hubApiRepeatEight) {
      state.__hubApiEight ??= bytes;
      return state.__hubApiEight;
    }
    return bytes;
  };
  return { ...actual, default: { ...actual, randomUUID, randomBytes }, randomUUID, randomBytes };
});

const BASE = "https://hub.test";

interface BodySpec {
  text?: string;
  base64?: string;
  repeat?: { prefix: string; fill: string; count: number; suffix: string; closeFill?: string };
  template?: Record<string, unknown>;
}

interface RequestSpec {
  method: string;
  url: string;
  headers: Array<[string, string]>;
  body: BodySpec | null;
}

function resetGenerators(repeatUserCode = false): void {
  const state = globalThis as unknown as {
    __hubApiUuid: number;
    __hubApiBytes: number;
    __hubApiRepeatEight: boolean;
    __hubApiEight: Buffer | undefined;
  };
  state.__hubApiUuid = 0;
  state.__hubApiBytes = 0;
  state.__hubApiRepeatEight = repeatUserCode;
  state.__hubApiEight = undefined;
}

function materialize(body: BodySpec | null, substitutions?: (text: string) => string): string | Uint8Array | null {
  if (body === null) return null;
  if (body.text !== undefined) return body.text;
  if (body.base64 !== undefined) return new Uint8Array(Buffer.from(body.base64, "base64"));
  if (body.repeat !== undefined) {
    const { prefix, fill, count, suffix, closeFill } = body.repeat;
    return `${prefix}${fill.repeat(count)}${suffix}${(closeFill ?? "").repeat(count)}`;
  }
  if (body.template !== undefined) {
    const text = JSON.stringify(body.template);
    return substitutions === undefined ? text : substitutions(text);
  }
  throw new Error("unknown body spec");
}

function buildRequest(spec: RequestSpec, substitutions?: (text: string) => string): Request {
  const headers = new Headers();
  for (const [name, value] of spec.headers) headers.append(name, substitutions?.(value) ?? value);
  const body = materialize(spec.body, substitutions);
  return new Request(spec.url, {
    method: spec.method,
    headers,
    ...(body === null ? {} : { body }),
  });
}

async function responseTrace(response: Response) {
  return {
    status: response.status,
    headers: Object.fromEntries([...response.headers]),
    body: await response.text(),
  };
}

type OperationSpec = { result: Record<string, unknown> } | { throw: "database" | "error" } | null;

// Long operation inputs are recorded as UTF-16 length plus SHA-256 of the UTF-8 text to keep the
// committed trace small; inputs up to 2048 UTF-16 units are recorded verbatim.
function inputDigest(text: string | undefined): string | null {
  if (text === undefined) return null;
  if (text.length <= 2048) return text;
  return `sha256:${text.length}:${createHash("sha256").update(text).digest("hex")}`;
}

function operationStubs(spec: OperationSpec, calls: Array<Record<string, unknown>>) {
  const respond = async () => {
    if (spec === null) throw new Error("no operation configured");
    if ("throw" in spec) {
      throw spec.throw === "database" ? new DatabaseUnavailableError() : new Error("operation exploded");
    }
    const result = { ...spec.result };
    if (typeof result["expiresAt"] === "string") result["expiresAt"] = new Date(result["expiresAt"]);
    return result;
  };
  const record = (operation: string) => (authorization: unknown, input?: unknown) => {
    calls.push({ operation, authorization, input: inputDigest(JSON.stringify(input)) });
    return respond();
  };
  return {
    listTriggers: record("listTriggers"),
    validateTrigger: record("validateTrigger"),
    installTrigger: record("installTrigger"),
    listProjects: record("listProjects"),
    listConfigurationResources: record("listConfigurationResources"),
    listSetupResources: record("listSetupResources"),
    validateConfiguration: record("validateConfiguration"),
    installConfiguration: record("installConfiguration"),
    dispatchManualRun: record("dispatchManualRun"),
    issueEnrollmentToken: record("issueEnrollmentToken"),
  };
}

function authenticatorStub(outcome: string, calls: string[]) {
  return {
    authorize(_request: Request, scope: string) {
      calls.push(scope);
      if (outcome === "unavailable") return Promise.reject(new DatabaseUnavailableError());
      if (outcome === "throw") return Promise.reject(new Error("authenticator exploded"));
      if (outcome !== "authorized") return Promise.resolve({ status: outcome });
      return Promise.resolve({
        status: "authorized",
        access: {
          kind: "apiKey",
          credentialId: "key-1",
          organizationId: "organization-1",
          scopes: [scope],
        },
      });
    },
  };
}

async function runCase(spec: {
  via: string;
  composition?: string;
  request: RequestSpec;
  auth: string;
  operation: OperationSpec;
}) {
  resetGenerators();
  const authorizeCalls: string[] = [];
  const operationCalls: Array<Record<string, unknown>> = [];
  const operations = operationStubs(spec.operation, operationCalls);
  const composition =
    spec.composition === "unavailable"
      ? ({ status: "unavailable" } as const)
      : ({ status: "enabled", authenticator: authenticatorStub(spec.auth, authorizeCalls) } as const);
  const api = createPublicApi(composition as never, operations as never);
  const request = buildRequest(spec.request);
  const response = spec.via.startsWith("operation:")
    ? await api.handleOperation(spec.via.slice("operation:".length) as never, request)
    : await api.handle(request);
  return { ...(await responseTrace(response)), authorizeCalls, operationCalls };
}

interface Row {
  id: string;
  organization_id: string;
  name?: string;
  prefix: string;
  verifier: string;
  scopes?: string[];
  created_at: Date;
  last_used_at: Date | null;
  revoked_at: Date | null;
}

// The two credential classes under test only run these statements; the fake answers exactly them.
class CredentialRows {
  readonly apiKeys = new Map<string, Row>();
  readonly cli = new Map<string, Row>();

  query(sql: string, params: readonly unknown[] = []) {
    const statement = sql.replace(/\s+/gu, " ").trim();
    const table = statement.includes("organization_cli_credentials") ? this.cli : this.apiKeys;
    if (statement.startsWith("insert into organization_api_keys")) {
      const [id, organization, name, prefix, verifier, scopes] = params as [
        string, string, string, string, string, string[],
      ];
      const row: Row = {
        id, organization_id: organization, name, prefix, verifier, scopes,
        created_at: new Date(0), last_used_at: null, revoked_at: null,
      };
      this.apiKeys.set(prefix, row);
      return Promise.resolve({ rows: [row], rowCount: 1 });
    }
    if (/^select .* from organization_(api_keys|cli_credentials) where prefix = \$1$/u.test(statement)) {
      const row = table.get(String(params[0]));
      return Promise.resolve({ rows: row === undefined ? [] : [row], rowCount: row === undefined ? 0 : 1 });
    }
    if (/^update organization_(api_keys|cli_credentials) set last_used_at/u.test(statement)) {
      const row = [...table.values()].find((candidate) => candidate.id === params[0]);
      const touched = row !== undefined && row.revoked_at === null;
      return Promise.resolve({ rows: touched ? [{ id: row.id }] : [], rowCount: touched ? 1 : 0 });
    }
    return Promise.reject(new Error(`unexpected statement: ${statement}`));
  }
}

const SCENARIO_START_URL = `${BASE}/api/v1/cli-authorizations`;
const POLL_URL = `${BASE}/api/v1/cli-authorizations/poll`;

function browserAccess(kind: string) {
  if (kind === "none") return undefined;
  const organization =
    kind === "other-org"
      ? { id: "org-other", name: "Other", slug: "other" }
      : { id: "org-acme", name: "Acme", slug: "acme" };
  const owner = kind !== "member";
  return {
    resolveOrganizationAccess: () => {
      if (kind === "product-401") return Promise.reject(new ProductRequestError(401, "unauthenticated"));
      if (kind === "product-403") return Promise.reject(new ProductRequestError(403, "forbidden_org"));
      if (kind === "product-500-custom") return Promise.reject(new ProductRequestError(500, "custom_failure"));
      if (kind === "throws") return Promise.reject(new Error("access exploded"));
      return Promise.resolve({
        session: { id: "session-owner" },
        account: { id: "user-owner", name: "Owner", email: "owner@example.test" },
        organization,
        membership: { id: "member-owner", role: owner ? ("owner" as const) : ("member" as const) },
        capabilities: { view: true as const, manageResources: owner, manageMembers: owner, manageOwners: owner },
      });
    },
    resolveAccount: () => Promise.reject(new Error("unused")),
    rejectCookieMutation: () =>
      kind === "reject-cookie" ? Response.json({ error: "invalid_origin" }, { status: 403 }) : undefined,
  };
}

const EXOTIC: Record<string, string> = {
  A: "Ａ", B: "ℬ", C: "ℂ", D: "ⅅ", E: "ℰ", F: "ℱ", G: "ℊ",
  H: "ℋ", I: "ℐ", J: "𝐉", K: "K", L: "ℒ", M: "ℳ", N: "ℕ",
  O: "𝕆", P: "ℙ", Q: "ℚ", R: "ℝ", S: "ſ", T: "𝐓",
  U: "𝓤", V: "Ⅴ", W: "𝕎", X: "Ⅹ", Y: "𝐘", Z: "ℤ",
  "2": "²", "3": "³", "4": "⁴", "5": "⁵", "6": "⁶", "7": "⁷",
};

const COMPAT: Record<string, string> = { K: "K", S: "ſ", C: "ℂ", A: "Ａ", "2": "²" };

function transformUserCode(code: string, transform: string | undefined): string {
  switch (transform) {
    case undefined:
      return code;
    case "lower":
      return code.toLowerCase();
    case "strip-dashes":
      return code.replaceAll("-", "");
    case "spaces":
      return code.replaceAll("-", "  ");
    case "compat":
      return [...code].map((char) => COMPAT[char] ?? char).join("");
    case "fullwidth":
      return [...code]
        .map((char) =>
          /[A-Z2-7]/u.test(char) ? String.fromCodePoint(char.codePointAt(0)! + 0xfee0) : char,
        )
        .join("");
    case "junk":
      return `!!${code}??`;
    case "digits-inserted":
      return code.replaceAll("-", "0-1");
    case "truncate":
      return code.slice(0, -1);
    case "sharp-s-prefix":
      return `ß${code}`;
    case "ligature-prefix":
      return `ﬁ${code}`;
    case "roman-prefix":
      return `Ⅶ${code}`;
    case "circled":
      return [...code]
        .map((char) =>
          /[A-Z]/u.test(char)
            ? String.fromCodePoint(0x24b6 + char.charCodeAt(0) - 65)
            : /[2-7]/u.test(char)
              ? String.fromCodePoint(0x2461 + char.charCodeAt(0) - 50)
              : char,
        )
        .join("");
    case "parenthesized":
      return [...code]
        .map((char) =>
          /[A-Z]/u.test(char)
            ? String.fromCodePoint(0x249c + char.charCodeAt(0) - 65)
            : /[2-7]/u.test(char)
              ? String.fromCodePoint(0x2474 + char.charCodeAt(0) - 49)
              : char,
        )
        .join("");
    case "exotic":
      return [...code].map((char) => EXOTIC[char] ?? char).join("");
    case "circled-prefix":
      return `ⒶⒷ${code}`;
    default:
      throw new Error(`unknown transform ${transform}`);
  }
}

async function runScenario(spec: {
  config: { publicBaseUrl: string | null; access: string; startAt: string; apiKeys: Array<{ name: string; scopes: string[]; revoked?: boolean }>; repeatUserCode?: boolean };
  steps: Array<Record<string, any>>;
}) {
  resetGenerators(spec.config.repeatUserCode === true);
  let now = new Date(spec.config.startAt);
  const database = createMemoryDatabase({ now: () => now });
  const authorizations = new CliAuthorizations(
    database,
    browserAccess(spec.config.access) as never,
    spec.config.publicBaseUrl ?? undefined,
  );
  const rows = new CredentialRows();
  const apiKeys = new OrganizationApiKeys(rows as never, {} as never);
  const authenticator = new PublicCredentialAuthenticator(
    apiKeys,
    new OrganizationCliCredentials(rows as never),
  );
  const keys: Record<string, { secret: string; prefix: string }> = {};
  for (const key of spec.config.apiKeys) {
    const created = await apiKeys.create("organization-a", "user-a", key.name, key.scopes as never);
    keys[key.name] = { secret: created.secret, prefix: created.summary.prefix };
    if (key.revoked) rows.apiKeys.get(created.summary.prefix)!.revoked_at = new Date(1);
  }
  const started: Record<string, { deviceCode: string; userCode: string }> = {};
  const credentials: Record<string, { credential: string; organizationId: string }> = {};
  const substitute = (text: string): string =>
    text
      .replace(/\{userCode\.(\w+)\}/gu, (_, name: string) => started[name]!.userCode)
      .replace(/\{key\.(\w+)\.prefix\}/gu, (_, name: string) => keys[name]!.prefix)
      .replace(/\{key\.(\w+)\}/gu, (_, name: string) => keys[name]!.secret)
      .replace(/\{poll\.(\w+)\.credential\.prefix\}/gu, (_, name: string) =>
        credentials[name] === undefined ? "missing" : cliCredentialParts(credentials[name].credential).prefix,
      )
      .replace(/\{poll\.(\w+)\.credential\.secret\}/gu, (_, name: string) =>
        credentials[name] === undefined ? "missing" : credentials[name].credential.slice(23),
      )
      .replace(/\{poll\.(\w+)\.credential\}/gu, (_, name: string) =>
        credentials[name] === undefined ? "missing" : credentials[name].credential,
      );
  const post = (url: string, headers: Array<[string, string]>, body: BodySpec | null) =>
    buildRequest({ method: "POST", url, headers, body }, substitute);
  const jsonHeaders: Array<[string, string]> = [["content-type", "application/json"]];
  const trace: Array<Record<string, unknown>> = [];
  const call = async (operation: () => Promise<Response>) => {
    try {
      return await responseTrace(await operation());
    } catch (error) {
      return { threw: (error as Error).message };
    }
  };
  for (const step of spec.steps) {
    switch (step["do"]) {
      case "start": {
        const result = await call(() =>
          authorizations.start(post(step["url"], step["headers"] ?? jsonHeaders, step["body"] ?? { text: "{}" })),
        );
        if ("status" in result && result.status === 201 && step["as"] !== undefined) {
          started[step["as"]] = JSON.parse(result.body) as { deviceCode: string; userCode: string };
        }
        trace.push({ do: "start", as: step["as"] ?? null, ...result });
        break;
      }
      case "startMany": {
        const statuses: Record<string, number> = {};
        for (let index = 0; index < step["count"]; index += 1) {
          const response = await authorizations.start(
            post(SCENARIO_START_URL, [["x-paseo-client-address", `${step["fingerprintPrefix"]}${index}`]], { text: "{}" }),
          );
          statuses[String(response.status)] = (statuses[String(response.status)] ?? 0) + 1;
        }
        trace.push({ do: "startMany", count: step["count"], statuses });
        break;
      }
      case "poll": {
        const deviceCode = step["device"] ?? started[step["of"]]?.deviceCode;
        const body = step["body"] ?? { text: JSON.stringify({ deviceCode }) };
        const result = await call(() =>
          authorizations.poll(post(step["url"] ?? POLL_URL, step["headers"] ?? jsonHeaders, body)),
        );
        if ("body" in result && result.status === 200 && step["of"] !== undefined) {
          const parsed = JSON.parse(result.body) as { status: string; credential?: string; organizationId?: string };
          if (parsed.status === "authorized") {
            credentials[step["of"]] = { credential: parsed.credential!, organizationId: parsed.organizationId! };
            const parts = cliCredentialParts(parsed.credential!);
            rows.cli.set(parts.prefix, {
              id: `cli-${step["of"]}`,
              organization_id: parsed.organizationId!,
              prefix: parts.prefix,
              verifier: parts.verifier,
              created_at: now,
              last_used_at: null,
              revoked_at: null,
            });
          }
        }
        trace.push({ do: "poll", of: step["of"] ?? null, ...result });
        break;
      }
      case "inspect":
      case "decide": {
        const code = transformUserCode(started[step["of"]]!.userCode, step["transform"]);
        const userCode = step["userCode"] ?? code;
        const defaultBody =
          step["do"] === "inspect"
            ? { text: JSON.stringify({ userCode }) }
            : { text: JSON.stringify({ userCode, decision: step["decision"], organizationId: step["organizationId"] }) };
        const url = `${BASE}/cli-authorizations/${step["do"] === "inspect" ? "inspect" : "decision"}`;
        const result = await call(() =>
          step["do"] === "inspect"
            ? authorizations.inspect(post(url, jsonHeaders, step["body"] ?? defaultBody))
            : authorizations.decide(post(url, jsonHeaders, step["body"] ?? defaultBody)),
        );
        trace.push({ do: step["do"], of: step["of"] ?? null, ...result });
        break;
      }
      case "advance":
        now = new Date(now.getTime() + step["seconds"] * 1000);
        trace.push({ do: "advance", seconds: step["seconds"] });
        break;
      case "revokeCli":
        rows.cli.get(cliCredentialParts(credentials[step["of"]]!.credential).prefix)!.revoked_at = new Date(1);
        trace.push({ do: "revokeCli", of: step["of"] });
        break;
      case "authorize": {
        const headers = new Headers();
        const values: Array<[string, string]> = [["authorization", step["header"]], ...(step["headers"] ?? [])];
        for (const [name, value] of values) headers.append(name, substitute(value));
        const result = (await authenticator.authorize(
          new Request("https://hub.test/api/v1/projects", { headers }),
          step["scope"],
        )) as { status: string; access?: Record<string, unknown> };
        trace.push({
          do: "authorize",
          header: step["header"].length > 80 ? `${step["header"].slice(0, 80)}...` : step["header"],
          scope: step["scope"],
          status: result.status,
          kind: result.access?.["kind"] ?? null,
          organizationId: result.access?.["organizationId"] ?? null,
          scopes: result.access?.["scopes"] ?? null,
        });
        break;
      }
      default:
        throw new Error(`unknown step ${String(step["do"])}`);
    }
  }
  return trace;
}

test("captures the offline public API, CLI device authorization and OpenAPI behavior", async () => {
  const outputPath = process.env["SPOCKY_HUB_API_OUTPUT"];
  const openapiPath = process.env["SPOCKY_HUB_API_OPENAPI_OUTPUT"];
  const casesPath = process.env["SPOCKY_HUB_API_CASES"];
  if (!outputPath || !openapiPath || !casesPath) {
    throw new Error("SPOCKY_HUB_API_OUTPUT, SPOCKY_HUB_API_OPENAPI_OUTPUT and SPOCKY_HUB_API_CASES are required");
  }
  const spec = JSON.parse(await readFile(casesPath, "utf8"));
  const names = new Set<string>();
  for (const item of [...spec.cases, ...spec.scenarios]) {
    assert.ok(!names.has(item.name), `duplicate case name ${item.name}`);
    names.add(item.name);
  }

  let construction: string;
  try {
    createPublicApi({ status: "enabled", authenticator: authenticatorStub("authorized", []) } as never, null);
    construction = "no error";
  } catch (error) {
    construction = `threw: ${(error as Error).message}`;
  }
  const openapiResponse = createPublicApi({ status: "unavailable" }, null).openapi();
  const openapiHeaders = Object.fromEntries([...openapiResponse.headers]);
  const openapiText = await openapiResponse.text();
  await writeFile(openapiPath, openapiText);
  const openapiAgain = await createPublicApi({ status: "unavailable" }, null).openapi().text();
  assert.equal(openapiAgain, openapiText);
  assert.equal(openapiText, JSON.stringify(publicOpenApiDocument));

  const cases: Record<string, unknown> = {};
  for (const item of spec.cases) cases[item.name] = await runCase(item);
  const scenarios: Record<string, unknown> = {};
  for (const item of spec.scenarios) scenarios[item.name] = await runScenario(item);

  const output = {
    schemaVersion: 1,
    construction: { enabledWithoutOperations: construction },
    manifest: publicOperationManifest.map((definition) => ({
      id: definition.id,
      method: definition.method,
      path: definition.path,
      scope: definition.scope,
      successStatus: definition.successStatus,
      resultMapping: definition.resultMapping,
      tag: definition.tag,
      summary: definition.summary,
      description: definition.description,
      hasRequestSchema: definition.requestSchema !== undefined,
      responses: definition.responses,
    })),
    openapi: {
      status: openapiResponse.status,
      headers: openapiHeaders,
      bytes: Buffer.byteLength(openapiText),
      sha256: createHash("sha256").update(openapiText).digest("hex"),
    },
    cases,
    scenarios,
  };
  await writeFile(outputPath, `${JSON.stringify(output, null, 2)}\n`);
}, 600_000);
