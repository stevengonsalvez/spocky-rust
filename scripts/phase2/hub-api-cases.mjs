// Generates scripts/phase2/hub-api-cases.json, the case list that both the pinned Hub capture
// (hub-api-original.integration.test.ts) and the Rust differential (tests/hub_api_evidence.rs) run.
//
// Usage: node scripts/phase2/hub-api-cases.mjs > scripts/phase2/hub-api-cases.json

const BASE = "https://hub.test";
const AUTH = ["authorization", "Bearer valid"];
const JSON_CT = ["content-type", "application/json"];

const OPERATIONS = [
  { id: "listTriggers", method: "GET", path: "/api/v1/triggers", body: null },
  { id: "validateTrigger", method: "POST", path: "/api/v1/triggers/validate", body: "yaml" },
  { id: "installTrigger", method: "POST", path: "/api/v1/triggers/install", body: "yaml" },
  { id: "listProjects", method: "GET", path: "/api/v1/projects", body: null },
  { id: "listConfigurationResources", method: "GET", path: "/api/v1/configuration-resources", body: null },
  { id: "listSetupResources", method: "GET", path: "/api/v1/setup-resources", body: null },
  { id: "validateConfiguration", method: "POST", path: "/api/v1/configurations/validate", body: "config" },
  { id: "installConfiguration", method: "POST", path: "/api/v1/configurations/install", body: "config" },
  { id: "dispatchManualRun", method: "POST", path: "/api/v1/manual-runs", body: "manual" },
  { id: "issueEnrollmentToken", method: "POST", path: "/api/v1/daemons/enrollment-tokens", body: null },
];
const byId = Object.fromEntries(OPERATIONS.map((operation) => [operation.id, operation]));

const UUID_A = "84af3583-23ff-4fcc-9838-ed3262499be2";
const UUID_B = "f83dc934-02a0-4849-8de7-699110be24ed";
const UUID_C = "845e9d26-7977-45e1-bc69-d80a7b55a9cc";

const VALID_BODIES = {
  yaml: { yaml: "name: mention\nenabled: true\n" },
  config: {
    projectSlug: "payments",
    files: [{ path: ".paseo/hub.yml", content: "environments: {}\nagents: {}\n" }],
  },
  manual: {
    projectSlug: "payments",
    trigger: "deploy",
    actor: "automation",
    deliveryKey: "deploy-1",
    input: { environment: "production" },
  },
};

const text = (value) => ({ text: value });
const json = (value) => ({ text: JSON.stringify(value) });
// prefix + fill x count + suffix + closeFill x count (closeFill is optional)
const repeat = (prefix, fill, count, suffix, closeFill = "") => ({ repeat: { prefix, fill, count, suffix, closeFill } });
const base64 = (bytes) => ({ base64: Buffer.from(bytes).toString("base64") });

function request(operation, overrides = {}) {
  const spec = byId[operation];
  const headers = overrides.headers ?? [
    AUTH,
    ...(spec.body === null ? [] : [JSON_CT]),
  ];
  const body = overrides.body === undefined
    ? spec.body === null
      ? null
      : json(VALID_BODIES[spec.body])
    : overrides.body;
  return {
    method: overrides.method ?? spec.method,
    url: overrides.url ?? `${BASE}${spec.path}`,
    headers,
    body,
  };
}

const cases = [];
function add(name, via, requestSpec, extra = {}) {
  cases.push({
    name,
    via,
    request: requestSpec,
    auth: extra.auth ?? "authorized",
    operation: extra.operation ?? null,
  });
}

// Success and domain result mapping for every operation.
const RESULTS = {
  listTriggers: {
    listed: {
      status: "listed",
      triggers: [
        { id: UUID_A, name: "mention", enabled: true, format: "single_run", yaml: "name: mention\nenabled: true\n" },
        { id: UUID_B, name: "legacy \"quoted\" é ", enabled: false, format: "legacy_multistep", yaml: "a: b\n\ttab\r\n" },
      ],
    },
    empty: { status: "listed", triggers: [] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  validateTrigger: {
    valid: { status: "valid", name: "mention", valid: true },
    invalid: { status: "invalid_trigger", issues: [{ path: ["steps", 0, "id"], message: "Required" }, { path: [], message: "bad" }] },
    invalidEmptyIssues: { status: "invalid_trigger", issues: [] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  installTrigger: {
    installed: { status: "installed", triggerId: UUID_A, name: "mention", revisionId: UUID_B, version: 3, active: true },
    invalid: { status: "invalid_trigger", issues: [{ path: ["name"], message: "Required" }] },
    infrastructure: { status: "infrastructure_unavailable" },
    badTriggerId: { status: "installed", triggerId: "not-a-uuid", name: "mention", revisionId: UUID_B, version: 3, active: true },
    badRevisionId: { status: "installed", triggerId: UUID_A, name: "mention", revisionId: "nope", version: 3, active: true },
    zeroVersion: { status: "installed", triggerId: UUID_A, name: "mention", revisionId: UUID_B, version: 0, active: true },
    negativeVersion: { status: "installed", triggerId: UUID_A, name: "mention", revisionId: UUID_B, version: -1, active: true },
    inactive: { status: "installed", triggerId: UUID_A, name: "mention", revisionId: UUID_B, version: 3, active: false },
  },
  listProjects: {
    listed: { status: "listed", projects: [{ id: UUID_A, name: "Payments", slug: "payments" }, { id: UUID_B, name: "Docs é", slug: "docs" }] },
    empty: { status: "listed", projects: [] },
    badId: { status: "listed", projects: [{ id: "x", name: "Payments", slug: "payments" }] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  listConfigurationResources: {
    listed: {
      status: "listed",
      daemons: [{ id: UUID_A, slug: "build-server" }],
      github: [{ slug: "github-a", accountLogin: "octocat", accountType: "User", repositories: ["octocat/starter", "octocat/two"] }],
      discord: [{ slug: "discord-a", guildName: "Discord A" }],
      slack: [{ slug: "slack-a", teamName: "Slack A" }],
      linear: [{ slug: "linear-a", organizationName: "Linear A" }],
    },
    empty: { status: "listed", daemons: [], github: [], discord: [], slack: [], linear: [] },
    badDaemonId: {
      status: "listed",
      daemons: [{ id: "daemon", slug: "build-server" }],
      github: [], discord: [], slack: [], linear: [],
    },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  listSetupResources: {
    listed: {
      status: "listed",
      github: [{ slug: "github-connection", accountLogin: "octocat", accountType: "User", repositories: ["octocat/starter"] }],
      discord: [{ guildId: "123456789", guildName: "Paseo Guild" }],
      slack: [{ teamId: "T01234567", teamName: "Paseo Workspace" }],
    },
    empty: { status: "listed", github: [], discord: [], slack: [] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  validateConfiguration: {
    valid: { status: "valid", projectSlug: "project", valid: true },
    wouldCreate: { status: "valid", projectSlug: "project", valid: true, wouldCreateProject: true },
    wouldCreateFalse: { status: "valid", projectSlug: "project", valid: true, wouldCreateProject: false },
    projectNotFound: { status: "project_not_found" },
    invalidBundle: { status: "invalid_bundle", issues: [{ path: ["partials", 0, "path"], message: "partial file is not referenced by the configuration" }] },
    invalidConfiguration: { status: "invalid_configuration", issues: [{ path: ["environments", 0], message: "unknown daemon" }] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  installConfiguration: {
    installed: { status: "installed", projectSlug: "project", versionId: UUID_A, version: 4, active: true },
    inactive: { status: "installed", projectSlug: "project", versionId: UUID_A, version: 4, active: false },
    badVersionId: { status: "installed", projectSlug: "project", versionId: "v", version: 4, active: true },
    zeroVersion: { status: "installed", projectSlug: "project", versionId: UUID_A, version: 0, active: true },
    projectNotFound: { status: "project_not_found" },
    invalidBundle: { status: "invalid_bundle", issues: [{ path: ["../secret.md"], message: "unsafe path" }] },
    invalidConfiguration: { status: "invalid_configuration", versionId: UUID_B, issues: [{ path: ["triggers", 0, "steps"], message: "Required" }] },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  dispatchManualRun: {
    running: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: UUID_C, triggerRunId: UUID_B, configuredTriggerName: "deploy", workflowStatus: "running" },
    succeeded: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: UUID_C, triggerRunId: UUID_B, configuredTriggerName: "deploy", workflowStatus: "succeeded" },
    failed: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: UUID_C, triggerRunId: UUID_B, configuredTriggerName: "deploy", workflowStatus: "failed" },
    timedOut: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: UUID_C, triggerRunId: UUID_B, configuredTriggerName: "deploy", workflowStatus: "timed_out" },
    escapes: { status: "dispatched", deliveryKey: "k\"\\/\b\f\n\r\t\u0001\u007f  😀", providerEventReceiptId: UUID_C, triggerRunId: UUID_B, configuredTriggerName: "déploy", workflowStatus: "running" },
    badReceiptId: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: "r", triggerRunId: UUID_B, configuredTriggerName: "deploy", workflowStatus: "running" },
    badRunId: { status: "dispatched", deliveryKey: "deploy-1", providerEventReceiptId: UUID_C, triggerRunId: "r", configuredTriggerName: "deploy", workflowStatus: "running" },
    projectNotFound: { status: "project_not_found" },
    actorForbidden: { status: "actor_forbidden" },
    configurationNotFound: { status: "configuration_not_found" },
    triggerNotFound: { status: "trigger_not_found" },
    expectedNotCurrent: { status: "expected_configuration_not_current" },
    daemonOffline: { status: "daemon_offline" },
    invalidInput: { status: "invalid_input", triggerRunId: UUID_B, issues: [{ path: ["environment"], message: "Invalid option" }] },
    invalidInputEmpty: { status: "invalid_input", triggerRunId: UUID_B, issues: [] },
    dispatchConflict: { status: "dispatch_conflict" },
    infrastructure: { status: "infrastructure_unavailable" },
  },
  issueEnrollmentToken: {
    issued: { status: "issued", token: "a".repeat(43), expiresAt: "2026-08-06T18:10:00.000Z" },
    issuedShort: { status: "issued", token: "a".repeat(31), expiresAt: "2026-08-06T18:10:00.000Z" },
    issuedMinimum: { status: "issued", token: "b".repeat(32), expiresAt: "2026-08-06T18:10:00.123Z" },
    revoked: { status: "credential_revoked" },
    infrastructure: { status: "infrastructure_unavailable" },
  },
};
for (const operation of OPERATIONS) {
  for (const [variant, result] of Object.entries(RESULTS[operation.id])) {
    add(`result/${operation.id}/${variant}`, "handle", request(operation.id, {
      headers: [AUTH, ...(operation.body === null ? [] : [JSON_CT]), ["x-request-id", "request-1"]],
    }), { operation: { result } });
  }
}

// Authentication outcomes and unexpected failures for every operation.
for (const operation of OPERATIONS) {
  const successResult = Object.values(RESULTS[operation.id])[0];
  for (const auth of ["unauthorized", "forbidden", "unavailable", "throw"]) {
    add(`auth/${operation.id}/${auth}`, "handle", request(operation.id, {
      headers: [AUTH, ...(operation.body === null ? [] : [JSON_CT]), ["x-request-id", "request-1"]],
    }), { auth, operation: { result: successResult } });
  }
  add(`failure/${operation.id}/database`, "handle", request(operation.id, {
    headers: [AUTH, ...(operation.body === null ? [] : [JSON_CT]), ["x-request-id", "request-1"]],
  }), { operation: { throw: "database" } });
  add(`failure/${operation.id}/error`, "handle", request(operation.id, {
    headers: [AUTH, ...(operation.body === null ? [] : [JSON_CT]), ["x-request-id", "request-1"]],
  }), { operation: { throw: "error" } });
  add(`scope/${operation.id}`, "handle", request(operation.id), {
    operation: { result: successResult },
  });
}

// Routing: unknown paths and wrong methods.
for (const [name, method, url] of [
  ["unknown", "POST", `${BASE}/api/v1/not-a-route`],
  ["root", "GET", `${BASE}/`],
  ["prefix-only", "GET", `${BASE}/api/v1`],
  ["prefix-slash", "GET", `${BASE}/api/v1/`],
  ["trailing-slash", "GET", `${BASE}/api/v1/projects/`],
  ["uppercase-path", "GET", `${BASE}/API/v1/projects`],
  ["encoded-path", "GET", `${BASE}/api/v1/%70rojects`],
  ["dot-segments", "GET", `${BASE}/api/v1/../v1/projects`],
  ["double-slash", "GET", `${BASE}/api/v1//projects`],
  ["query-ignored", "GET", `${BASE}/api/v1/projects?limit=1&x=%`],
  ["fragment-ignored", "GET", `${BASE}/api/v1/projects#fragment`],
  ["other-host-and-port", "GET", "http://127.0.0.1:3000/api/v1/projects"],
  ["cli-start-path", "POST", `${BASE}/api/v1/cli-authorizations`],
  ["cli-poll-path", "POST", `${BASE}/api/v1/cli-authorizations/poll`],
  ["openapi-path", "GET", `${BASE}/api/openapi.json`],
  ["get-on-post-route", "GET", `${BASE}/api/v1/manual-runs`],
  ["post-on-get-route", "POST", `${BASE}/api/v1/projects`],
  ["put-on-get-route", "PUT", `${BASE}/api/v1/projects`],
  ["delete-on-post-route", "DELETE", `${BASE}/api/v1/triggers/install`],
  ["lowercase-custom-method", "patch", `${BASE}/api/v1/triggers`],
  ["options", "OPTIONS", `${BASE}/api/v1/triggers`],
  ["head", "HEAD", `${BASE}/api/v1/triggers`],
  ["lowercase-get", "get", `${BASE}/api/v1/projects`],
  ["mixed-case-delete", "Delete", `${BASE}/api/v1/projects`],
  ["encoded-slash", "GET", `${BASE}/api/v1%2Fprojects`],
  ["encoded-space", "GET", `${BASE}/api/v1/projects%20`],
  ["semicolon", "GET", `${BASE}/api/v1/projects;x`],
  ["backslash-path", "GET", `${BASE}\\api\\v1\\projects`],
  ["tab-in-url", "GET", `${BASE}/api/v1/pro\tjects`],
  ["uppercase-host", "GET", "HTTPS://HUB.TEST/api/v1/projects"],
  ["trailing-dot-host", "GET", "https://hub.test./api/v1/projects"],
  ["default-port", "GET", "https://hub.test:443/api/v1/projects"],
  ["ipv6-host", "GET", "http://[::1]:3000/api/v1/projects"],
  ["query-only", "GET", `${BASE}?x=/api/v1/projects`],
  ["dot-segment-encoded", "GET", `${BASE}/api/v1/%2e%2e/v1/projects`],
  ["triple-slash", "GET", `${BASE}///api/v1/projects`],
  ["unicode-path", "GET", `${BASE}/api/v1/pr\u00f6jects`],
]) {
  add(`router/${name}`, "handle", { method, url, headers: [AUTH, ["x-request-id", "request-1"]], body: null }, {
    operation: { result: RESULTS.listProjects.listed },
  });
}

// Request identity.
for (const [name, headers] of [
  ["absent", [AUTH]],
  ["blank", [AUTH, ["x-request-id", ""]]],
  ["spaces", [AUTH, ["x-request-id", "   "]]],
  ["padded", [AUTH, ["x-request-id", "  caller-id  "]]],
  ["inner-spaces", [AUTH, ["x-request-id", " a  b "]]],
  ["tab-and-space", [AUTH, ["x-request-id", "\t id \t"]]],
  ["quoted", [AUTH, ["x-request-id", "a\"b\\c"]]],
  ["very-long", [AUTH, ["x-request-id", "r".repeat(1000)]]],
  ["duplicate", [AUTH, ["x-request-id", "first"], ["x-request-id", "second"]]],
  ["nbsp-only", [AUTH, ["x-request-id", "\u00a0"]]],
  ["nbsp-padded", [AUTH, ["x-request-id", "\u00a0id\u00a0"]]],
  ["newline-padded", [AUTH, ["x-request-id", "\n id \r"]]],
  ["line-tab-mixed", [AUTH, ["x-request-id", "\t\u00a0 \t"]]],
  ["upper-name", [AUTH, ["X-Request-ID", "mixed-case"]]],
  ["latin1", [AUTH, ["x-request-id", "café"]]],
]) {
  add(`request-id/success/${name}`, "handle", request("listProjects", { headers }), {
    operation: { result: RESULTS.listProjects.listed },
  });
  add(`request-id/problem/${name}`, "handle", request("listProjects", { headers }), { auth: "unauthorized" });
}

// Authorization header is handed to the authenticator untouched; the stub only records the scope.
add("authorization/missing-header", "handle", request("listProjects", { headers: [] }), {
  operation: { result: RESULTS.listProjects.listed },
});

// Body decoding and content type rules.
const configBody = json(VALID_BODIES.config);
for (const [name, headers] of [
  ["json", [AUTH, JSON_CT]],
  ["charset", [AUTH, ["content-type", "application/json; charset=utf-8"]]],
  ["uppercase", [AUTH, ["content-type", "APPLICATION/JSON"]]],
  ["substring-prefix", [AUTH, ["content-type", "x-application/json"]]],
  ["substring-suffix", [AUTH, ["content-type", "application/jsonx"]]],
  ["problem-json", [AUTH, ["content-type", "application/problem+json"]]],
  ["text-plain", [AUTH, ["content-type", "text/plain"]]],
  ["form", [AUTH, ["content-type", "application/x-www-form-urlencoded"]]],
  ["empty-value", [AUTH, ["content-type", ""]]],
  ["absent", [AUTH]],
]) {
  add(`content-type/${name}`, "handle", request("installConfiguration", { headers, body: configBody }), {
    operation: { result: RESULTS.installConfiguration.installed },
  });
}

const rawBodies = [
  ["empty", text("")],
  ["whitespace", text(" \t\r\n ")],
  ["open-brace", text("{")],
  ["null", text("null")],
  ["array", text("[]")],
  ["number", text("42")],
  ["string", text("\"text\"")],
  ["true", text("true")],
  ["object-empty", text("{}")],
  ["trailing-comma", text("{\"yaml\":\"a\",}")],
  ["trailing-garbage", text("{\"yaml\":\"a\"} x")],
  ["single-quotes", text("{'yaml':'a'}")],
  ["bom-one", base64([0xef, 0xbb, 0xbf, ...Buffer.from(JSON.stringify(VALID_BODIES.yaml))])],
  ["bom-two", base64([0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf, ...Buffer.from(JSON.stringify(VALID_BODIES.yaml))])],
  ["bom-three", base64([0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf, ...Buffer.from(JSON.stringify(VALID_BODIES.yaml))])],
  ["invalid-utf8-in-string", base64([...Buffer.from("{\"yaml\":\"a"), 0xff, ...Buffer.from("b\"}")])],
  ["invalid-utf8-outside", base64([...Buffer.from("{\"yaml\":\"a\"}"), 0xff])],
  ["duplicate-key-last-wins", text("{\"yaml\":\"first\",\"yaml\":\"second\"}")],
  ["duplicate-key-invalid-last", text("{\"yaml\":\"first\",\"yaml\":1}")],
  ["proto-key", text("{\"yaml\":\"a\",\"__proto__\":1}")],
  ["numeric-extra-keys", text("{\"b\":1,\"2\":2,\"yaml\":\"a\",\"1\":1,\"a\":0}")],
  ["escaped-key", text("{\"y\\u0061ml\":\"a\"}")],
  ["escaped-string", text("{\"yaml\":\"line\\nbreak \\u00e9 \\ud83d\\ude00 \\/ \\\\\"}")],
  ["control-char-in-string", text("{\"yaml\":\"a\u0001b\"}")],
  ["leading-zero-number", text("{\"yaml\":\"a\",\"n\":01}")],
  ["nan", text("{\"yaml\":NaN}")],
  ["whitespace-around", text("  \n{ \"yaml\" : \"a\" }\t\r\n")],
  ["line-separator-after", text("{\"yaml\":\"a\"}\u2028")],
  ["nbsp-after", text("{\"yaml\":\"a\"}\u00a0")],
  ["vertical-tab-after", text("{\"yaml\":\"a\"}\u000b")],
  ["bom-then-space", base64([0xef, 0xbb, 0xbf, 0x20, ...Buffer.from(JSON.stringify(VALID_BODIES.yaml))])],
  ["bom-inside", text("{\ufeff\"yaml\":\"a\"}")],
  ["nested-extra", text("{\"yaml\":\"a\",\"extra\":{\"deep\":[1,2,{\"x\":null}]}}")],
];
for (const operation of ["validateTrigger", "dispatchManualRun"]) {
  for (const [name, body] of rawBodies) {
    add(`body/${operation}/${name}`, "handle", request(operation, { body }), {
      operation: { result: operation === "validateTrigger" ? RESULTS.validateTrigger.valid : RESULTS.dispatchManualRun.running },
    });
  }
}
// About 1 MB of nesting must neither crash nor change the answer.
for (const [name, body] of [
  ["deep-array-1mb", repeat("", "[", 500000, "", "]")],
  ["deep-object-1mb", repeat("", '{"a":', 200000, "1", "}")],
  ["deep-unterminated-1mb", repeat("", "[", 1000000, "")],
]) {
  add(`body/validateTrigger/${name}`, "handle", request("validateTrigger", { body }), {
    operation: { result: RESULTS.validateTrigger.valid },
  });
}
// Garbage bodies on operations without a request schema are ignored.
for (const operation of ["listTriggers", "listProjects", "issueEnrollmentToken"]) {
  add(`body/${operation}/ignored-garbage`, "handle", request(operation, { headers: [AUTH, ["content-type", "text/plain"]], body: operation === "listTriggers" || operation === "listProjects" ? null : text("{{{") }), {
    operation: { result: Object.values(RESULTS[operation])[0] },
  });
}

// Schema validation grids.
const validation = [];
function grid(operation, kind, name, body) {
  validation.push([operation, `${kind}/${name}`, body]);
}
const yamlBase = VALID_BODIES.yaml;
const yamlCases = {
  missing: {},
  empty: { yaml: "" },
  one: { yaml: "x" },
  "max-length": null,
  "over-max": null,
  "astral-499999": null,
  "astral-500001": null,
  number: { yaml: 1 },
  null: { yaml: null },
  array: { yaml: ["a"] },
  object: { yaml: { a: 1 } },
  boolean: { yaml: true },
  extra: { ...yamlBase, extra: 1 },
  "extra-two": { ...yamlBase, a: 1, b: 2 },
  "missing-and-extra": { extra: 1 },
  "extra-numeric": { ...yamlBase, 7: 1, 3: 2 },
};
for (const [name, value] of Object.entries(yamlCases)) {
  if (value !== null) grid("installTrigger", "yaml", name, json(value));
}
grid("installTrigger", "yaml", "max-length", repeat('{"yaml":"', "x", 1_000_000, '"}'));
grid("installTrigger", "yaml", "over-max", repeat('{"yaml":"', "x", 1_000_001, '"}'));
grid("installTrigger", "yaml", "astral-499999", repeat('{"yaml":"', "😀", 499_999, '"}'));
grid("installTrigger", "yaml", "astral-500001", repeat('{"yaml":"', "😀", 500_001, '"}'));
grid("installTrigger", "yaml", "whitespace-only-ok", json({ yaml: "   " }));
for (const [name, body] of [
  ["array-empty", '{"yaml":[]}'],
  ["array-one", '{"yaml":["a"]}'],
  ["object-length-zero", '{"yaml":{"length":0}}'],
  ["object-length-big", '{"yaml":{"length":1000001}}'],
  ["object-length-ok", '{"yaml":{"length":5}}'],
  ["proto-key", '{"yaml":"a","__proto__":1}'],
  ["proto-key-and-extra", '{"yaml":"a","__proto__":1,"extra":2}'],
  ["proto-key-only-extra", '{"__proto__":{"yaml":"a"}}'],
  ["constructor-key", '{"yaml":"a","constructor":1}'],
  ["quoted-key", '{"yaml":"a","a\\"b":1}'],
  ["backslash-key", '{"yaml":"a","a\\\\b":1}'],
  ["newline-key", '{"yaml":"a","a\\nb":1}'],
  ["unicode-key", '{"yaml":"a","caf\\u00e9":1,"\\ud83d\\ude00":2}'],
  ["empty-key", '{"yaml":"a","":1}'],
  ["object-length-string", '{"yaml":{"length":"abc"}}'],
  ["object-length-numeric-string", '{"yaml":{"length":"0"}}'],
  ["object-length-big-string", '{"yaml":{"length":"2000000"}}'],
  ["object-length-null", '{"yaml":{"length":null}}'],
  ["object-length-true", '{"yaml":{"length":true}}'],
  ["object-length-false", '{"yaml":{"length":false}}'],
  ["object-length-negative", '{"yaml":{"length":-1}}'],
  ["object-length-empty-array", '{"yaml":{"length":[]}}'],
  ["object-length-array-five", '{"yaml":{"length":[5]}}'],
  ["object-length-array-big", '{"yaml":{"length":[1000001]}}'],
  ["object-length-array-two", '{"yaml":{"length":[1,2]}}'],
  ["object-length-object", '{"yaml":{"length":{}}}'],
  ["object-length-hex", '{"yaml":{"length":"0x10"}}'],
  ["object-length-space", '{"yaml":{"length":" 2000000 "}}'],
  ["object-length-infinity", '{"yaml":{"length":"Infinity"}}'],
  ["object-length-empty-string", '{"yaml":{"length":""}}'],
  ["object-length-exponent", '{"yaml":{"length":"1e7"}}'],
  ["object-length-fraction", '{"yaml":{"length":0.5}}'],
  ["to-string-key", '{"yaml":"a","toString":1,"hasOwnProperty":2}'],
]) {
  grid("installTrigger", "yaml", name, text(body));
}

const file = (path, content) => ({ path, content });
const hubFile = file(".paseo/hub.yml", "environments: {}\n");
const configCases = {
  "no-slug": { files: [hubFile] },
  "slug-empty": { projectSlug: "", files: [hubFile] },
  "slug-spaces": { projectSlug: "   ", files: [hubFile] },
  "slug-padded": { projectSlug: "  payments  ", files: [hubFile] },
  "slug-nbsp": { projectSlug: " payments ", files: [hubFile] },
  "slug-feff": { projectSlug: "﻿payments﻿", files: [hubFile] },
  "slug-nel-not-trimmed": { projectSlug: "\u0085payments\u0085", files: [hubFile] },
  "slug-line-separator": { projectSlug: " payments ", files: [hubFile] },
  "slug-100": { projectSlug: "s".repeat(100), files: [hubFile] },
  "slug-101": { projectSlug: "s".repeat(101), files: [hubFile] },
  "slug-100-padded": { projectSlug: ` ${"s".repeat(100)} `, files: [hubFile] },
  "slug-101-padded": { projectSlug: ` ${"s".repeat(101)} `, files: [hubFile] },
  "slug-astral-51": { projectSlug: "😀".repeat(51), files: [hubFile] },
  "slug-number": { projectSlug: 1, files: [hubFile] },
  "slug-null": { projectSlug: null, files: [hubFile] },
  "slug-array": { projectSlug: [], files: [hubFile] },
  "files-missing": { projectSlug: "payments" },
  "files-empty": { projectSlug: "payments", files: [] },
  "files-null": { projectSlug: "payments", files: null },
  "files-string": { projectSlug: "payments", files: "x" },
  "files-object": { projectSlug: "payments", files: {} },
  "files-100": { projectSlug: "payments", files: Array.from({ length: 100 }, (_, index) => file(`.paseo/workflows/w${index}.yml`, "a")) },
  "files-101": { projectSlug: "payments", files: Array.from({ length: 101 }, (_, index) => file(`.paseo/workflows/w${index}.yml`, "a")) },
  "file-string": { projectSlug: "payments", files: ["x"] },
  "file-null": { projectSlug: "payments", files: [null] },
  "file-array": { projectSlug: "payments", files: [[]] },
  "file-empty-object": { projectSlug: "payments", files: [{}] },
  "file-path-empty": { projectSlug: "payments", files: [file("", "a")] },
  "file-path-512": { projectSlug: "payments", files: [file("p".repeat(512), "a")] },
  "file-path-513": { projectSlug: "payments", files: [file("p".repeat(513), "a")] },
  "file-path-number": { projectSlug: "payments", files: [{ path: 1, content: "a" }] },
  "file-content-empty": { projectSlug: "payments", files: [file("a", "")] },
  "file-content-number": { projectSlug: "payments", files: [{ path: "a", content: 1 }] },
  "file-content-missing": { projectSlug: "payments", files: [{ path: "a" }] },
  "file-extra": { projectSlug: "payments", files: [{ path: "a", content: "b", mode: 1 }] },
  "file-extra-two": { projectSlug: "payments", files: [{ path: "a", content: "b", mode: 1, owner: 2 }] },
  "files-101-bad-first": { projectSlug: "payments", files: ["x", ...Array.from({ length: 100 }, (_, i) => file(`.paseo/workflows/w${i}.yml`, "a"))] },
  "files-empty-with-extra": { projectSlug: "payments", files: [], extra: 1 },
  "files-many-errors": { projectSlug: "", files: [{ path: "", content: 1 }, "x", { path: "a", content: "b", z: 1 }], extra: true },
  "top-extra": { projectSlug: "payments", files: [hubFile], extra: 1 },
  "top-extra-three": { projectSlug: "payments", files: [hubFile], c: 1, a: 2, b: 3 },
  "everything-missing": {},
  "slug-array-empty": { projectSlug: [], files: [hubFile] },
  "slug-array-big": { projectSlug: Array.from({ length: 101 }, () => 0), files: [hubFile] },
  "slug-array-three": { projectSlug: [1, 2, 3], files: [hubFile] },
  "files-empty-string": { projectSlug: "payments", files: "" },
  "files-long-string": { projectSlug: "payments", files: "x".repeat(101) },
  "files-object-length-zero": { projectSlug: "payments", files: { length: 0 } },
  "files-object-length-big": { projectSlug: "payments", files: { length: 101 } },

  "partial-bundle": {
    projectSlug: "payments",
    files: [hubFile, file(".paseo/workflows/deploy.yml", "name: deploy"), file(".paseo/workflows/partials/safety.md", "Follow the checklist.")],
  },
};
for (const [name, value] of Object.entries(configCases)) {
  grid("installConfiguration", "config", name, json(value));
}
grid("installConfiguration", "config", "file-proto-key", text('{"files":[{"path":"a","content":"b","__proto__":1}]}'));
grid("installConfiguration", "config", "file-proto-key-and-extra", text('{"files":[{"path":"a","content":"b","__proto__":1,"mode":2}]}'));
grid("installConfiguration", "config", "content-max", repeat('{"files":[{"path":"a","content":"', "x", 1_000_000, '"}]}'));
grid("installConfiguration", "config", "content-over-max", repeat('{"files":[{"path":"a","content":"', "x", 1_000_001, '"}]}'));
grid("installConfiguration", "config", "content-astral-over", repeat('{"files":[{"path":"a","content":"', "😀", 500_001, '"}]}'));
grid("validateConfiguration", "config", "valid", json(VALID_BODIES.config));
grid("validateConfiguration", "config", "no-slug", json(configCases["no-slug"]));
grid("validateConfiguration", "config", "slug-padded", json(configCases["slug-padded"]));
grid("validateConfiguration", "config", "files-many-errors", json(configCases["files-many-errors"]));

const manualBase = VALID_BODIES.manual;
const manualWithout = (key) => {
  const copy = { ...manualBase };
  delete copy[key];
  return copy;
};
const uuidVariants = {
  valid: UUID_A,
  upper: UUID_A.toUpperCase(),
  nil: "00000000-0000-0000-0000-000000000000",
  max: "ffffffff-ffff-ffff-ffff-ffffffffffff",
  "max-upper": "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
  "version-0": "84af3583-23ff-0fcc-9838-ed3262499be2",
  "version-8": "84af3583-23ff-8fcc-9838-ed3262499be2",
  "version-9": "84af3583-23ff-9fcc-9838-ed3262499be2",
  "variant-7": "84af3583-23ff-4fcc-7838-ed3262499be2",
  "variant-8": "84af3583-23ff-4fcc-8838-ed3262499be2",
  "variant-c": "84af3583-23ff-4fcc-c838-ed3262499be2",
  "no-dashes": "84af358323ff4fcc9838ed3262499be2",
  short: "84af3583-23ff-4fcc-9838-ed3262499be",
  long: "84af3583-23ff-4fcc-9838-ed3262499be22",
  braces: "{84af3583-23ff-4fcc-9838-ed3262499be2}",
  urn: "urn:uuid:84af3583-23ff-4fcc-9838-ed3262499be2",
  "non-hex": "g4af3583-23ff-4fcc-9838-ed3262499be2",
  padded: ` ${UUID_A} `,
  "trailing-newline": `${UUID_A}\n`,
  empty: "",
};
const manualCases = {
  "no-expected": manualBase,
  "missing-slug": manualWithout("projectSlug"),
  "missing-trigger": manualWithout("trigger"),
  "missing-actor": manualWithout("actor"),
  "missing-delivery": manualWithout("deliveryKey"),
  "missing-input": manualWithout("input"),
  "input-null": { ...manualBase, input: null },
  "input-string": { ...manualBase, input: "text" },
  "input-array": { ...manualBase, input: [1, "2", null, { a: [] }] },
  "input-numbers": { ...manualBase, input: [1.0, 0.1e1, 1e2, 1E+2, -0, 123456789012345680000, 1e21, 1e-7, 0.000001, 5e-324, 1.7976931348623157e308, 100, 0.5, 1234.5678] },
  "input-nested-order": { ...manualBase, input: { b: 1, a: { d: 1, c: 2 }, 10: "ten", 2: "two" } },
  "input-proto": null,
  "all-empty": { projectSlug: "", trigger: "", actor: "", deliveryKey: "" },
  "all-spaces": { projectSlug: " ", trigger: " ", actor: " ", deliveryKey: " " },
  "all-padded": { projectSlug: " p ", trigger: " t ", actor: " a ", deliveryKey: " d ", input: 1 },
  "slug-100": { ...manualBase, projectSlug: "s".repeat(100) },
  "slug-101": { ...manualBase, projectSlug: "s".repeat(101) },
  "trigger-200": { ...manualBase, trigger: "t".repeat(200) },
  "trigger-201": { ...manualBase, trigger: "t".repeat(201) },
  "actor-200": { ...manualBase, actor: "a".repeat(200) },
  "actor-201": { ...manualBase, actor: "a".repeat(201) },
  "delivery-200": { ...manualBase, deliveryKey: "d".repeat(200) },
  "delivery-201": { ...manualBase, deliveryKey: "d".repeat(201) },
  "delivery-201-padded": { ...manualBase, deliveryKey: ` ${"d".repeat(200)} ` },
  "types-wrong": { projectSlug: 1, trigger: null, actor: [], deliveryKey: {}, expectedVersionId: 2 },
  "expected-null": { ...manualBase, expectedVersionId: null },
  extra: { ...manualBase, extra: true },
  "extra-and-missing": { trigger: "deploy", extra: true },
};
delete manualCases["input-proto"];
for (const [name, value] of Object.entries(manualCases)) {
  grid("dispatchManualRun", "manual", name, json(value));
}
for (const [name, value] of Object.entries(uuidVariants)) {
  grid("dispatchManualRun", "manual-expected", name, json({ ...manualBase, expectedVersionId: value }));
}
grid("dispatchManualRun", "manual", "input-proto-key", text("{\"projectSlug\":\"p\",\"trigger\":\"t\",\"actor\":\"a\",\"deliveryKey\":\"d\",\"input\":{\"__proto__\":{\"x\":1},\"y\":2}}"));
grid("dispatchManualRun", "manual", "input-number-overflow", text("{\"projectSlug\":\"p\",\"trigger\":\"t\",\"actor\":\"a\",\"deliveryKey\":\"d\",\"input\":[1e400,-1e400,1e-400,-1e-400]}"));
grid("dispatchManualRun", "manual", "input-duplicate-keys", text("{\"projectSlug\":\"p\",\"trigger\":\"t\",\"actor\":\"a\",\"deliveryKey\":\"d\",\"input\":{\"a\":1,\"b\":2,\"a\":3}}"));
grid("dispatchManualRun", "manual", "input-large-integers", text("{\"projectSlug\":\"p\",\"trigger\":\"t\",\"actor\":\"a\",\"deliveryKey\":\"d\",\"input\":[9007199254740993,12345678901234567890,0.1,0.30000000000000004,1e22,1.5e300,4.35,-1.5e-10]}"));
grid("dispatchManualRun", "manual", "input-unicode", json({ ...manualBase, input: "é 😀\u0000\u001f\u007f" }));
grid("dispatchManualRun", "manual", "deep-input-1000", repeat('{"projectSlug":"p","trigger":"t","actor":"a","deliveryKey":"d","input":', "[", 1000, "]".repeat(1000) + "}"));
grid("dispatchManualRun", "manual", "deep-input-100", repeat('{"projectSlug":"p","trigger":"t","actor":"a","deliveryKey":"d","input":', '{"a":', 100, "1" + "}".repeat(100) + "}"));
for (const [operation, name, body] of validation) {
  const result = operation === "installTrigger"
    ? RESULTS.installTrigger.installed
    : operation === "installConfiguration"
      ? RESULTS.installConfiguration.installed
      : operation === "validateConfiguration"
        ? RESULTS.validateConfiguration.valid
        : RESULTS.dispatchManualRun.running;
  add(`validation/${operation}/${name}`, "handle", request(operation, { body }), { operation: { result } });
}

// handleOperation dispatches by operation id and skips path and method routing.
add("operation/listProjects-with-post", "operation:listProjects", request("listProjects", { method: "POST", url: `${BASE}/nowhere` }), {
  operation: { result: RESULTS.listProjects.listed },
});
add("operation/manual-with-put", "operation:dispatchManualRun", request("dispatchManualRun", { method: "PUT", url: `${BASE}/nowhere` }), {
  operation: { result: RESULTS.dispatchManualRun.running },
});
add("operation/unauthorized", "operation:installTrigger", request("installTrigger"), { auth: "unauthorized" });
add("operation/request-id", "operation:issueEnrollmentToken", request("issueEnrollmentToken", { headers: [AUTH, ["x-request-id", " op-id "]] }), {
  operation: { result: RESULTS.issueEnrollmentToken.issued },
});

// Compositions: unavailable and enabled without operations.
for (const operation of OPERATIONS) {
  cases.push({
    name: `composition/unavailable/${operation.id}`,
    via: "handle",
    composition: "unavailable",
    request: request(operation.id, {
      headers: [AUTH, ...(operation.body === null ? [] : [JSON_CT]), ["x-request-id", "request-1"]],
    }),
    auth: "authorized",
    operation: null,
  });
}
cases.push({
  name: "composition/unavailable/operation",
  via: "operation:listProjects",
  composition: "unavailable",
  request: request("listProjects"),
  auth: "authorized",
  operation: null,
});
cases.push({
  name: "composition/unavailable/unknown-path",
  via: "handle",
  composition: "unavailable",
  request: { method: "GET", url: `${BASE}/api/v1/nothing`, headers: [], body: null },
  auth: "authorized",
  operation: null,
});

// CLI authorization scenarios.
const ALL_SCOPES = ["projects:read", "configuration:validate", "configuration:install", "runs:dispatch", "daemons:enroll"];
const START_URL = `${BASE}/api/v1/cli-authorizations`;
const POLL_URL = `${BASE}/api/v1/cli-authorizations/poll`;
const scenarios = [];
const scenario = (name, config, steps) => scenarios.push({ name, config, steps });
const cfg = (overrides = {}) => ({
  publicBaseUrl: BASE,
  access: "owner",
  startAt: "2026-08-06T12:00:00.000Z",
  apiKeys: [],
  ...overrides,
});
const start = (as, extra = {}) => ({ do: "start", as, url: START_URL, ...extra });
const poll = (of, extra = {}) => ({ do: "poll", of, ...extra });
const inspect = (of, extra = {}) => ({ do: "inspect", of, ...extra });
const decide = (of, decision, extra = {}) => ({ do: "decide", of, decision, organizationId: "org-acme", ...extra });

scenario("approve-disclose-once", cfg(), [
  start("a"),
  inspect("a"),
  poll("a"),
  decide("a", "approve"),
  inspect("a"),
  { do: "advance", seconds: 5 },
  poll("a"),
  poll("a"),
  { do: "authorize", header: "Bearer {poll.a.credential}", scope: "projects:read" },
  { do: "authorize", header: "Bearer {poll.a.credential}", scope: "daemons:enroll" },
  { do: "authorize", header: "Bearer {poll.a.credential}", scope: "runs:dispatch" },
]);
scenario("deny-is-terminal", cfg(), [
  start("a"),
  decide("a", "approve", { organizationId: "org-other" }),
  decide("a", "deny"),
  poll("a"),
  decide("a", "approve"),
  poll("a"),
  inspect("a"),
]);
scenario("poll-throttle", cfg(), [
  start("a"),
  poll("a"),
  poll("a"),
  poll("a"),
  { do: "advance", seconds: 14 },
  poll("a"),
  { do: "advance", seconds: 15 },
  poll("a"),
  decide("a", "approve"),
  { do: "advance", seconds: 15 },
  poll("a"),
  poll("a"),
]);
scenario("approve-before-first-poll-interval", cfg(), [
  start("a"),
  poll("a"),
  decide("a", "approve"),
  poll("a"),
  { do: "advance", seconds: 10 },
  poll("a"),
  poll("a"),
]);
scenario("expiry", cfg(), [
  start("a"),
  start("b"),
  start("c"),
  { do: "advance", seconds: 599 },
  inspect("a"),
  decide("b", "approve"),
  { do: "advance", seconds: 1 },
  inspect("a"),
  poll("a"),
  decide("c", "approve"),
  poll("b"),
  poll("c"),
  { do: "poll", device: "unknown-device-code-0123456789abcdefghij" },
  start("d"),
  poll("d"),
]);
scenario("start-limits", cfg(), [
  ...["a", "b", "c", "d", "e"].map((name) => start(name, { headers: [["x-paseo-client-address", "203.0.113.7"]] })),
  start("f", { headers: [["x-paseo-client-address", "203.0.113.7"]] }),
  start("g", { headers: [["x-paseo-client-address", "203.0.113.8"]] }),
  start("h"),
  start("i"),
  decide("a", "deny"),
  start("j", { headers: [["x-paseo-client-address", "203.0.113.7"]] }),
  decide("b", "approve"),
  start("k", { headers: [["x-paseo-client-address", "203.0.113.7"]] }),
  { do: "advance", seconds: 600 },
  start("l", { headers: [["x-paseo-client-address", "203.0.113.7"]] }),
]);
scenario("start-fingerprint-headers", cfg(), [
  start("a", { headers: [["x-paseo-client-address", "a"], ["x-paseo-client-address", "b"]] }),
  start("b", { headers: [["x-paseo-client-address", "a, b"]] }),
  start("c", { headers: [["X-Paseo-Client-Address", "  a, b  "]] }),
  start("d", { headers: [["x-paseo-client-address", ""]] }),
  start("e", { headers: [["x-paseo-client-address", "unknown"]] }),
  start("f", { headers: [] }),
  start("g", { headers: [["x-paseo-client-address", "unknown"]] }),
  start("h", { headers: [["x-paseo-client-address", "unknown"]] }),
  start("i", { headers: [["x-paseo-client-address", "unknown"]] }),
  start("j", { headers: [["x-paseo-client-address", "unknown"]] }),
]);
scenario("expired-record-retained", cfg(), [
  start("a", { headers: [["x-paseo-client-address", "one"]] }),
  poll("a"),
  poll("a"),
  { do: "advance", seconds: 601 },
  start("b", { headers: [["x-paseo-client-address", "two"]] }),
  start("c", { headers: [["x-paseo-client-address", "three"]] }),
  start("d", { headers: [["x-paseo-client-address", "four"]] }),
  poll("a"),
  poll("a"),
  decide("a", "approve"),
  inspect("a"),
]);
scenario("user-code-collision", cfg({ repeatUserCode: true }), [
  start("a", { headers: [["x-paseo-client-address", "one"]] }),
  start("b", { headers: [["x-paseo-client-address", "two"]] }),
  start("c", { headers: [["x-paseo-client-address", "three"]] }),
  start("d", { headers: [["x-paseo-client-address", "four"]] }),
  inspect("d"),
  decide("d", "approve"),
  decide("b", "deny"),
  inspect("c"),
  poll("a"),
  poll("b"),
  poll("c"),
  poll("d"),
]);
scenario("global-limit", cfg(), [
  { do: "startMany", count: 1000, fingerprintPrefix: "load-", as: "load" },
  start("over"),
  start("other", { headers: [["x-paseo-client-address", "fresh"]] }),
  { do: "advance", seconds: 600 },
  start("again"),
]);
for (const [name, body] of [
  ["empty-object", text("{}")],
  ["empty-body", text("")],
  ["open-brace", text("{")],
  ["null", text("null")],
  ["array", text("[]")],
  ["extra-key", text('{"a":1}')],
  ["extra-key-nested", text('{"interval":5}')],
  ["whitespace-object", text(" { } ")],
  ["bom", base64([0xef, 0xbb, 0xbf, 0x7b, 0x7d])],
  ["string", text('"x"')],
  ["number", text("1")],
]) {
  scenario(`start-body/${name}`, cfg(), [start("a", { body, headers: [] })]);
}
scenario("start-without-content-type", cfg(), [
  { do: "start", as: "a", url: START_URL, headers: [], body: text("{}") },
  { do: "start", as: "b", url: START_URL, headers: [["content-type", "text/plain"]], body: text("{}") },
]);
for (const [name, publicBaseUrl, url] of [
  ["configured-base", "https://hub.example.com:8443/base/path/?q=1#f", START_URL],
  ["configured-plain", "http://127.0.0.1:3000", START_URL],
  ["configured-ipv6", "http://[::1]:3000/", START_URL],
  ["configured-idn", "https://bücher.example/", START_URL],
  ["configured-default-port", "https://hub.test:443", START_URL],
  ["configured-uppercase-host", "HTTPS://HUB.TEST", START_URL],
  ["request-url", null, `${BASE}/api/v1/cli-authorizations?x=1#y`],
  ["request-url-port", null, "http://localhost:3000/api/v1/cli-authorizations"],
]) {
  scenario(`verification-uri/${name}`, cfg({ publicBaseUrl }), [start("a", { url })]);
}
for (const [name, body] of [
  ["empty-object", text("{}")],
  ["empty-body", text("")],
  ["open-brace", text("{")],
  ["null", text("null")],
  ["array", text("[]")],
  ["number", text("42")],
  ["missing", text("{}")],
  ["short-31", json({ deviceCode: "d".repeat(31) })],
  ["min-32", json({ deviceCode: "d".repeat(32) })],
  ["max-200", json({ deviceCode: "d".repeat(200) })],
  ["over-201", json({ deviceCode: "d".repeat(201) })],
  ["astral-100", json({ deviceCode: "😀".repeat(100) })],
  ["number-code", json({ deviceCode: 12345678901234567890123456789012 })],
  ["null-code", json({ deviceCode: null })],
  ["extra", json({ deviceCode: "d".repeat(40), extra: 1 })],
  ["extra-only", json({ extra: 1 })],
  ["bom", base64([0xef, 0xbb, 0xbf, ...Buffer.from(JSON.stringify({ deviceCode: "d".repeat(40) }))])],
]) {
  scenario(`poll-body/${name}`, cfg(), [{ do: "poll", url: POLL_URL, body }]);
}
scenario("poll-headers", cfg(), [
  start("a"),
  { do: "poll", of: "a", headers: [] },
  { do: "poll", of: "a", headers: [["content-type", "text/plain"]] },
]);
scenario("user-code-normalization", cfg(), [
  start("a"),
  inspect("a", { transform: "lower" }),
  inspect("a", { transform: "strip-dashes" }),
  inspect("a", { transform: "spaces" }),
  inspect("a", { transform: "compat" }),
  inspect("a", { transform: "fullwidth" }),
  inspect("a", { transform: "junk" }),
  inspect("a", { transform: "digits-inserted" }),
  inspect("a", { transform: "truncate" }),
  inspect("a", { userCode: "" }),
  inspect("a", { userCode: "x".repeat(41) }),
  inspect("a", { userCode: "x".repeat(40) }),
  inspect("a", { userCode: "---" }),
  inspect("a", { userCode: "\uff10\uff11" }),
  inspect("a", { userCode: "\u0000" }),
  inspect("a", { userCode: "ßſKℂﬁⅦ" }),
  decide("a", "approve", { transform: "compat" }),
  poll("a"),
]);
scenario("normalization-exotic-letters", cfg(), [
  start("a"),
  { do: "inspect", of: "a", transform: "sharp-s-prefix" },
  { do: "inspect", of: "a", transform: "ligature-prefix" },
  { do: "inspect", of: "a", transform: "roman-prefix" },
  { do: "inspect", of: "a", transform: "circled-prefix" },
  { do: "inspect", of: "a", transform: "circled" },
  { do: "inspect", of: "a", transform: "parenthesized" },
  { do: "inspect", of: "a", transform: "exotic" },
  decide("a", "approve", { transform: "exotic" }),
  poll("a"),
]);
scenario("inspect-body", cfg(), [
  start("a"),
  { do: "inspect", of: "a", body: text("") },
  { do: "inspect", of: "a", body: text("{") },
  { do: "inspect", of: "a", body: json({}) },
  { do: "inspect", of: "a", body: json({ userCode: 5 }) },
  { do: "inspect", of: "a", body: json({ userCode: "A", extra: 1 }) },
  { do: "inspect", of: "a", body: text("null") },
]);
scenario("decide-body", cfg(), [
  start("a"),
  { do: "decide", of: "a", body: text("") },
  { do: "decide", of: "a", body: text("{") },
  { do: "decide", of: "a", body: json({}) },
  { do: "decide", of: "a", body: { template: { userCode: "{userCode.a}", decision: "maybe", organizationId: "org-acme" } } },
  { do: "decide", of: "a", body: { template: { userCode: "{userCode.a}", decision: "approve", organizationId: "" } } },
  { do: "decide", of: "a", body: { template: { userCode: "{userCode.a}", decision: "approve" } } },
  { do: "decide", of: "a", body: { template: { userCode: "{userCode.a}", decision: "approve", organizationId: "org-acme", extra: 1 } } },
  { do: "decide", of: "a", body: { template: { userCode: "", decision: "approve", organizationId: "org-acme" } } },
  { do: "decide", of: "a", body: { template: { userCode: "{userCode.a}", decision: "approve", organizationId: 7 } } },
  inspect("a"),
  decide("a", "approve", { transform: "lower" }),
  inspect("a"),
]);
for (const access of ["none", "reject-cookie", "member", "product-401", "product-403", "product-500-custom", "throws", "other-org"]) {
  scenario(`access/${access}`, cfg({ access }), [
    start("a"),
    inspect("a"),
    decide("a", "approve"),
    decide("a", "deny", { body: text("{") }),
    poll("a"),
  ]);
}
scenario("access/none-start-poll-still-work", cfg({ access: "none" }), [start("a"), poll("a")]);
scenario("credentials", cfg({
  apiKeys: [
    { name: "all", scopes: ALL_SCOPES },
    { name: "read", scopes: ["projects:read"] },
    { name: "multi", scopes: ["projects:read", "runs:dispatch"] },
    { name: "revoked", scopes: ["projects:read"], revoked: true },
  ],
}), [
  start("a"),
  decide("a", "approve"),
  poll("a"),
  start("b"),
  decide("b", "approve"),
  poll("b"),
  { do: "revokeCli", of: "b" },
  ...[
    "Bearer {key.all}",
    "Bearer {key.read}",
    "Bearer {key.multi}",
    "Bearer {key.revoked}",
    "Bearer {key.all}x",
    "Bearer {key.all.prefix}",
    "Bearer {key.all.prefix}_",
    "Bearer {key.all.prefix}_x",
    "Bearer x{key.all}",
    "bearer {key.all}",
    "BEARER {key.all}",
    "Bearer  {key.all}",
    "Bearer {key.all} ",
    "Basic {key.all}",
    "Bearer PASEO_CLI_aaaaaaaaaaaa_secret",
    "Bearer paseo_pk_AAAAAAAAAAAA_secret",
    "Bearer paseo_cli_{poll.a.credential}",
    "{key.all}",
    "Bearer ",
    "Bearer",
    "",
    `Bearer ${"a".repeat(200)}`,
    `Bearer ${"a".repeat(201)}`,
    "Bearer paseo_pk_",
    "Bearer paseo_pk_aaaaaaaaaaaa_secret",
    "Bearer paseo_pk_aaaaaaaaaaa_secret",
    "Bearer paseo_pk_aaaaaaaaaaaaa_secret",
    "Bearer paseo_pk_aaaaaaaaaa!!_secret",
    "Bearer paseo_cli_",
    "Bearer paseo_cli_aaaaaaaaaaaa_secret",
    "Bearer paseo_cli_aaaaaaaaaaaa",
    "Bearer paseo_cli_aaaaaaaaaaaa_",
    "Bearer paseo_cli_aaaaaaaaaa!!_secret",
    "Bearer {poll.a.credential}",
    "Bearer {poll.a.credential}x",
    "Bearer {poll.a.credential.prefix}_wrong",
    "Bearer {poll.a.credential.prefix}",
    "Bearer {poll.a.credential.prefix}_",
    "Bearer {poll.b.credential}",
    "bearer {poll.a.credential}",
    "Bearer {poll.a.credential} ",
    "Bearer x{poll.a.credential}",
    "Bearer {poll.a.credential.secret}",
  ].map((header) => ({ do: "authorize", header, scope: "projects:read" })),
  ...["projects:read", "configuration:validate", "configuration:install", "runs:dispatch", "daemons:enroll"].flatMap((scope) => [
    { do: "authorize", header: "Bearer {key.read}", scope },
    { do: "authorize", header: "Bearer {key.multi}", scope },
    { do: "authorize", header: "Bearer {poll.a.credential}", scope },
  ]),
  { do: "authorize", header: "Bearer {key.all}", scope: "projects:read", headers: [["authorization", "Bearer {key.read}"]] },
]);

const output = {
  schemaVersion: 1,
  operations: OPERATIONS.map(({ id, method, path }) => ({ id, method, path })),
  cases,
  scenarios,
};
process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
