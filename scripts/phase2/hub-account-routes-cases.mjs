// Case list for the Hub account, organization and API-key route differential.
//
// One list drives both sides: the pinned Hub captures it through its real production runtime and the
// Rust handlers replay it. `node hub-account-routes-cases.mjs` prints the JSON that is committed as
// hub-account-routes-cases.json.
//
// Scenario shape: { id, env, steps }. A step is one of
//   { kind: "setup", op, ... }          not traced; builds the world (accounts, bootstrap owner)
//   { kind: "request", ... }            traced; one raw request and the raw response
// Request fields: id, actor, method, path, query (appended to the path, templates allowed), headers
// (name -> value, null removes), body (raw text or null), capture ({ name: "dotted.path" } read from
// the JSON response body).
// Templates `{{name}}` in path and body resolve to a captured value or a world value
// (`<actor>.account`, `<actor>.member`, `org`, `orgSlug`).
//
// A request with an `actor` carries that actor's session cookie and `origin: <app url>` unless the
// step overrides a header. The anonymous actor carries no cookie.

const APP_URL = "http://localhost:3000";
const PREFIX = "/api/auth/paseo/";
const scenarios = [];

function scenario(id, env, build) {
  const steps = [];
  let count = 0;
  const api = {
    setup(op, fields = {}) {
      steps.push({ kind: "setup", op, ...fields });
    },
    request(actor, method, path, body = null, fields = {}) {
      count += 1;
      const label = fields.label ?? `${method.toLowerCase()}-${path.replace(/^\/api\/auth\//u, "")}`;
      const { label: _ignored, ...rest } = fields;
      steps.push({
        kind: "request",
        id: `${id}/${String(count).padStart(3, "0")}-${actor}-${label}`,
        actor,
        method,
        path,
        ...rest,
        body,
      });
    },
    post(actor, route, body, fields = {}) {
      api.request(actor, "POST", `${PREFIX}${route}`, body, { label: route, ...fields });
    },
    get(actor, route, fields = {}) {
      api.request(actor, "GET", `${PREFIX}${route}`, null, { label: route, ...fields });
    },
  };
  build(api);
  scenarios.push({ id, env, steps });
}

const json = (value) => JSON.stringify(value);
const text = (value) => value;

// Origin guard variants applied to every POST route when a cookie is present.
const GUARD_VARIANTS = [
  ["no-origin", { origin: null }],
  ["null-origin", { origin: "null" }],
  ["foreign-origin", { origin: "https://evil.example" }],
  ["cross-site", { "sec-fetch-site": "cross-site" }],
  ["bad-origin-text", { origin: "not a url" }],
  ["referer-only-ok", { origin: null, referer: `${APP_URL}/settings` }],
  ["referer-foreign", { origin: null, referer: "https://evil.example/x" }],
];
const POST_ROUTES = [
  "create-organization",
  "select-organization",
  "create-invitation",
  "cancel-invitation",
  "accept-invitation",
  "change-member-role",
  "remove-member",
  "api-keys",
  "revoke-api-key",
  "revoke-cli-credential",
];
const UUID_A = "00000000-0000-4000-8000-000000000001";

// ---------------------------------------------------------------------------------------------
// Scenario one: open registration and open organization creation. Roles, validation, order of
// checks, replay.
scenario("open", { registration: "open", organizationCreation: "open" }, (s) => {
  for (const [actor, name] of [
    ["owner", "Olive Owner"],
    ["admin", "Adam Admin"],
    ["member", "Mia Member"],
    ["outsider", "Otto Outsider"],
    ["rival", "Rita Rival"],
  ]) {
    s.setup("createAccount", { actor, name, email: `${actor}@example.test`, password: `${actor}-password-1` });
  }

  // Routing and dispatch.
  s.request("anon", "GET", "/api/auth/paseo/state", null, { label: "state-anonymous" });
  s.request("anon", "GET", "/api/auth/paseo/nope", null, { label: "unknown-get" });
  s.request("anon", "POST", "/api/auth/paseo/nope", null, { label: "unknown-post" });
  s.request("owner", "POST", "/api/auth/paseo/nope", null, { label: "unknown-post-cookie" });
  s.request("owner", "POST", "/api/auth/paseo/nope", null, {
    label: "unknown-post-foreign-origin",
    headers: { origin: "https://evil.example" },
  });
  s.request("anon", "POST", "/api/auth/paseo/state", null, { label: "state-post" });
  s.request("anon", "GET", "/api/auth/paseo/api-keys/", null, { label: "trailing-slash" });
  s.request("anon", "GET", "/api/auth/paseo/State", null, { label: "case" });
  s.request("anon", "GET", "/api/auth/paseo/", null, { label: "empty-route" });
  s.request("anon", "PUT", "/api/auth/paseo/create-organization", json({ name: "x" }), { label: "put" });
  s.request("anon", "DELETE", "/api/auth/paseo/remove-member", null, { label: "delete" });
  s.request("anon", "GET", "/api/auth/paseo/create-organization", null, { label: "get-on-post-route" });
  s.request("anon", "GET", "/api/auth/other", null, { label: "outside-allowlist" });
  s.request("anon", "POST", "/api/auth/request-password-reset", json({ email: "a@example.test" }), {
    label: "recovery-path-not-served",
  });
  s.request("anon", "POST", "/api/auth/reset-password", json({ token: "t", newPassword: "x" }), {
    label: "reset-path-not-served",
  });
  s.request("anon", "GET", "/api/auth/get-session", null, { label: "get-session-anonymous" });
  s.request("anon", "POST", "/api/auth/paseo/create-organization", json({ name: "Anon" }), {
    label: "create-organization-anonymous",
  });

  // Anonymous and no-organization responses on every route, in check order.
  for (const route of POST_ROUTES) s.post("anon", route, json({}), { label: `${route}-anonymous` });
  s.get("anon", "api-keys", { label: "api-keys-anonymous" });
  for (const route of POST_ROUTES) {
    s.post("outsider", route, json({}), { label: `${route}-outsider-empty` });
  }
  s.get("outsider", "api-keys", { label: "api-keys-outsider" });
  s.get("outsider", "state", { label: "state-outsider" });

  // Origin guard on every POST route with a cookie, before the body and the session.
  for (const route of POST_ROUTES) {
    for (const [name, headers] of GUARD_VARIANTS) {
      s.post("owner", route, json({}), { label: `${route}-guard-${name}`, headers });
    }
  }
  // The guard skips a request without a cookie and a GET.
  s.post("anon", "create-organization", json({ name: "Skip" }), {
    label: "guard-skipped-no-cookie",
    headers: { origin: "https://evil.example" },
  });
  s.get("owner", "state", { label: "state-guard-skipped-get", headers: { origin: "https://evil.example" } });

  // create-organization validation (session is read before the body).
  const bodies = [
    ["invalid-json", text("{")],
    ["empty-body", text("")],
    ["null", text("null")],
    ["array", text("[]")],
    ["empty-object", json({})],
    ["empty-name", json({ name: "" })],
    ["blank-name", json({ name: "   " })],
    ["number-name", json({ name: 5 })],
    ["extra-key", json({ name: "Valid", extra: 1 })],
    ["name-101", json({ name: "n".repeat(101) })],
    ["name-padded-101", json({ name: ` ${"n".repeat(100)} ` })],
    ["proto-key", text('{"name":"Valid","__proto__":{"a":1}}')],
    ["duplicate-key", text('{"name":"","name":"Valid"}')],
    ["bom", text('﻿{"name":"Valid"}')],
  ];
  for (const [name, body] of bodies) {
    s.post("outsider", "create-organization", body, { label: `create-organization-${name}` });
  }
  s.post("outsider", "create-organization", text(json({ name: "n".repeat(100) })), {
    label: "create-organization-name-100",
    capture: { outsiderOrg: "organizationId", outsiderOrgSlug: "organizationSlug" },
  });
  s.post("owner", "create-organization", json({ name: "  Acme Robotics  " }), {
    label: "create-organization-created",
    capture: { org: "organizationId", orgSlug: "organizationSlug" },
  });
  s.post("rival", "create-organization", json({ name: "Zeta Labs" }), {
    label: "create-organization-rival",
    capture: { rivalOrg: "organizationId" },
  });
  s.post("outsider", "create-organization", json({ name: "Otto &  Co!" }), {
    label: "create-organization-second",
  });

  // Invitations: manager setup, validation, duplicates, replay.
  s.post("owner", "create-invitation", json({ email: "admin@example.test", role: "admin" }), {
    label: "create-invitation-admin",
    capture: { invAdmin: "id" },
  });
  s.post("owner", "create-invitation", json({ email: "member@example.test", role: "member" }), {
    label: "create-invitation-member",
    capture: { invMember: "id" },
  });
  s.post("owner", "create-invitation", json({ email: "  ADMIN@Example.Test " , role: "admin" }), {
    label: "create-invitation-replay-normalized",
  });
  s.post("owner", "create-invitation", json({ email: "owner@example.test", role: "member" }), {
    label: "create-invitation-already-member",
  });
  for (const [name, body] of [
    ["invalid-json", text("not json")],
    ["empty-object", json({})],
    ["role-owner", json({ email: "x@example.test", role: "owner" })],
    ["role-missing", json({ email: "x@example.test" })],
    ["email-missing", json({ role: "member" })],
    ["email-invalid", json({ email: "nope", role: "member" })],
    ["email-space", json({ email: "a b@example.test", role: "member" })],
    ["email-no-tld", json({ email: "a@b", role: "member" })],
    ["email-plus", json({ email: "a+tag@example.test", role: "member" })],
    ["email-unicode", json({ email: "üser@example.test", role: "member" })],
    ["email-trailing-dot", json({ email: "a@example.test.", role: "member" })],
    ["email-quoted", json({ email: '"a b"@example.test', role: "member" })],
    ["email-long-local", json({ email: `${"a".repeat(65)}@example.test`, role: "member" })],
    ["email-number", json({ email: 5, role: "member" })],
    ["extra-key", json({ email: "x@example.test", role: "member", extra: true })],
  ]) {
    s.post("owner", "create-invitation", body, { label: `create-invitation-${name}` });
  }
  s.post("owner", "create-invitation", json({ email: "plus+tag@example.test", role: "member" }), {
    label: "create-invitation-valid-plus",
    capture: { invPlus: "id" },
  });
  s.get("owner", "state", { label: "state-owner-with-invitations" });
  s.get("outsider", "state", { label: "state-outsider-unrelated-invitation", query: "?invitation={{invAdmin}}" });
  s.get("anon", "state", { label: "state-anonymous-with-invitation", query: "?invitation={{invAdmin}}" });
  s.get("anon", "state", { label: "state-anonymous-unknown-invitation", query: `?invitation=${UUID_A}` });

  // Accept: wrong email, unknown, happy path, replay, then roles exist.
  s.post("outsider", "accept-invitation", json({ invitationId: "{{invAdmin}}" }), {
    label: "accept-invitation-wrong-email",
  });
  s.post("outsider", "accept-invitation", json({ invitationId: UUID_A }), { label: "accept-invitation-unknown" });
  s.post("outsider", "accept-invitation", json({}), { label: "accept-invitation-empty" });
  s.post("outsider", "accept-invitation", json({ invitationId: "" }), { label: "accept-invitation-empty-id" });
  s.post("outsider", "accept-invitation", json({ invitationId: "x", extra: 1 }), { label: "accept-invitation-extra" });
  s.post("admin", "accept-invitation", json({ invitationId: "{{invAdmin}}" }), {
    label: "accept-invitation-admin",
  });
  s.post("admin", "accept-invitation", json({ invitationId: "{{invAdmin}}" }), {
    label: "accept-invitation-replay",
  });
  s.post("member", "accept-invitation", json({ invitationId: "{{invMember}}" }), {
    label: "accept-invitation-member",
  });
  s.get("admin", "state", { label: "state-admin" });
  s.get("member", "state", { label: "state-member" });
  s.get("owner", "state", { label: "state-owner-after-accept" });
  s.get("member", "state", { label: "state-member-selected-invitation", query: "?invitation={{invPlus}}" });

  // select-organization.
  s.post("member", "select-organization", json({ organizationId: "{{rivalOrg}}" }), {
    label: "select-organization-foreign",
  });
  s.post("member", "select-organization", json({ organizationId: "{{org}}" }), {
    label: "select-organization-own",
  });
  s.post("member", "select-organization", json({ organizationId: UUID_A }), {
    label: "select-organization-unknown",
  });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-id", json({ organizationId: "" })],
    ["number-id", json({ organizationId: 1 })],
    ["extra-key", json({ organizationId: "{{org}}", extra: 1 })],
    ["empty-object", json({})],
  ]) {
    s.post("member", "select-organization", body, { label: `select-organization-${name}` });
  }

  // API keys: every role, validation, scope handling, revoke.
  s.get("owner", "api-keys", { label: "api-keys-owner-empty" });
  s.get("admin", "api-keys", { label: "api-keys-admin-empty" });
  s.get("member", "api-keys", { label: "api-keys-member-forbidden" });
  s.post("member", "api-keys", json({ name: "m", scopes: ["projects:read"] }), { label: "api-keys-member-forbidden-create" });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-object", json({})],
    ["empty-name", json({ name: "", scopes: ["projects:read"] })],
    ["blank-name", json({ name: "  ", scopes: ["projects:read"] })],
    ["name-101", json({ name: "k".repeat(101), scopes: ["projects:read"] })],
    ["scopes-empty", json({ name: "k", scopes: [] })],
    ["scopes-missing", json({ name: "k" })],
    ["scopes-not-array", json({ name: "k", scopes: "projects:read" })],
    ["scopes-number", json({ name: "k", scopes: [1] })],
    ["scopes-unknown", json({ name: "k", scopes: ["projects:read", "nope"] })],
    ["scopes-duplicate", json({ name: "k", scopes: ["projects:read", "projects:read"] })],
    ["scopes-case", json({ name: "k", scopes: ["Projects:Read"] })],
    ["extra-key", json({ name: "k", scopes: ["projects:read"], extra: 1 })],
  ]) {
    s.post("owner", "api-keys", body, { label: `api-keys-${name}` });
  }
  s.post("admin", "api-keys", json({ name: " Deploy key ", scopes: ["runs:dispatch", "projects:read"] }), {
    label: "api-keys-created-admin",
    capture: { keyAdmin: "key.id", secretAdmin: "secret", prefixAdmin: "key.prefix" },
  });
  s.post("owner", "api-keys", json({ name: "k".repeat(100), scopes: ["projects:read", "configuration:validate", "configuration:install", "runs:dispatch", "daemons:enroll"] }), {
    label: "api-keys-created-all-scopes",
    capture: { keyOwner: "key.id" },
  });
  s.get("owner", "api-keys", { label: "api-keys-owner-listed" });
  s.get("rival", "api-keys", { label: "api-keys-rival-isolated" });
  s.post("rival", "revoke-api-key", json({ id: "{{keyOwner}}" }), { label: "revoke-api-key-other-org" });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-object", json({})],
    ["not-uuid", json({ id: "abc" })],
    ["uppercase-uuid", json({ id: UUID_A.toUpperCase() })],
    ["nil-uuid", json({ id: "00000000-0000-0000-0000-000000000000" })],
    ["max-uuid", json({ id: "ffffffff-ffff-ffff-ffff-ffffffffffff" })],
    ["version-zero", json({ id: "00000000-0000-0000-8000-000000000000" })],
    ["extra-key", json({ id: UUID_A, extra: 1 })],
    ["number", json({ id: 1 })],
  ]) {
    s.post("owner", "revoke-api-key", body, { label: `revoke-api-key-${name}` });
  }
  s.post("owner", "revoke-api-key", json({ id: UUID_A }), { label: "revoke-api-key-unknown" });
  s.post("member", "revoke-api-key", json({ id: "{{keyOwner}}" }), { label: "revoke-api-key-member-forbidden" });
  s.post("admin", "revoke-api-key", json({ id: "{{keyAdmin}}" }), { label: "revoke-api-key-admin" });
  s.post("admin", "revoke-api-key", json({ id: "{{keyAdmin}}" }), { label: "revoke-api-key-replay" });
  s.get("owner", "api-keys", { label: "api-keys-owner-after-revoke" });
  s.post("owner", "revoke-cli-credential", json({ id: UUID_A }), { label: "revoke-cli-credential-unknown" });
  s.post("owner", "revoke-cli-credential", json({ id: "{{keyOwner}}" }), { label: "revoke-cli-credential-api-key-id" });
  s.post("member", "revoke-cli-credential", json({ id: UUID_A }), { label: "revoke-cli-credential-member-forbidden" });
  s.post("owner", "revoke-cli-credential", json({ id: "x" }), { label: "revoke-cli-credential-not-uuid" });

  // Members: change role and remove, in check order, last owner.
  s.post("member", "change-member-role", json({ memberId: "{{admin.member}}", role: "member" }), {
    label: "change-member-role-member-forbidden",
  });
  s.post("admin", "change-member-role", json({ memberId: "{{owner.member}}", role: "member" }), {
    label: "change-member-role-admin-demotes-owner",
  });
  s.post("admin", "change-member-role", json({ memberId: "{{member.member}}", role: "owner" }), {
    label: "change-member-role-admin-promotes-owner",
  });
  s.post("owner", "change-member-role", json({ memberId: UUID_A, role: "admin" }), {
    label: "change-member-role-unknown-member",
  });
  s.post("rival", "change-member-role", json({ memberId: "{{member.member}}", role: "admin" }), {
    label: "change-member-role-other-org",
  });
  s.post("owner", "change-member-role", json({ memberId: "{{owner.member}}", role: "admin" }), {
    label: "change-member-role-sole-owner-demote",
  });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-object", json({})],
    ["role-invalid", json({ memberId: "{{member.member}}", role: "root" })],
    ["role-missing", json({ memberId: "{{member.member}}" })],
    ["member-empty", json({ memberId: "", role: "admin" })],
    ["extra-key", json({ memberId: "{{member.member}}", role: "admin", extra: 1 })],
  ]) {
    s.post("owner", "change-member-role", body, { label: `change-member-role-${name}` });
  }
  s.post("admin", "change-member-role", json({ memberId: "{{member.member}}", role: "admin" }), {
    label: "change-member-role-admin-promotes-member",
  });
  s.post("owner", "change-member-role", json({ memberId: "{{member.member}}", role: "member" }), {
    label: "change-member-role-demote",
  });
  s.post("owner", "change-member-role", json({ memberId: "{{member.member}}", role: "owner" }), {
    label: "change-member-role-promote-owner",
  });
  s.post("owner", "change-member-role", json({ memberId: "{{owner.member}}", role: "admin" }), {
    label: "change-member-role-two-owners-demote-self",
  });
  s.post("member", "change-member-role", json({ memberId: "{{owner.member}}", role: "owner" }), {
    label: "change-member-role-new-owner-restores",
  });
  s.get("member", "state", { label: "state-after-role-changes" });
  s.post("owner", "change-member-role", json({ memberId: "{{owner.member}}", role: "owner" }), {
    label: "change-member-role-owner-to-owner",
  });

  s.post("admin", "remove-member", json({ memberId: "{{owner.member}}" }), { label: "remove-member-admin-removes-owner" });
  s.post("owner", "remove-member", json({ memberId: UUID_A }), { label: "remove-member-unknown" });
  s.post("rival", "remove-member", json({ memberId: "{{admin.member}}" }), { label: "remove-member-other-org" });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-object", json({})],
    ["member-empty", json({ memberId: "" })],
    ["extra-key", json({ memberId: "{{admin.member}}", extra: 1 })],
  ]) {
    s.post("owner", "remove-member", body, { label: `remove-member-${name}` });
  }
  s.post("owner", "remove-member", json({ memberId: "{{owner.member}}" }), {
    label: "remove-member-owner-removes-other-owner",
  });
  s.post("owner", "remove-member", json({ memberId: "{{admin.member}}" }), { label: "remove-member-admin" });
  s.get("admin", "state", { label: "state-removed-admin" });
  s.post("admin", "select-organization", json({ organizationId: "{{org}}" }), {
    label: "select-organization-removed",
  });
  s.get("owner", "state", { label: "state-owner-final" });

  // Cancel invitation.
  s.post("member", "cancel-invitation", json({ invitationId: "{{invPlus}}" }), { label: "cancel-invitation-member-forbidden" });
  s.post("owner", "cancel-invitation", json({ invitationId: UUID_A }), { label: "cancel-invitation-unknown" });
  s.post("rival", "cancel-invitation", json({ invitationId: "{{invPlus}}" }), { label: "cancel-invitation-other-org" });
  for (const [name, body] of [
    ["invalid-json", text("{")],
    ["empty-object", json({})],
    ["empty-id", json({ invitationId: "" })],
    ["extra-key", json({ invitationId: "{{invPlus}}", extra: 1 })],
  ]) {
    s.post("owner", "cancel-invitation", body, { label: `cancel-invitation-${name}` });
  }
  s.post("owner", "cancel-invitation", json({ invitationId: "{{invPlus}}" }), { label: "cancel-invitation-canceled" });
  s.post("owner", "cancel-invitation", json({ invitationId: "{{invPlus}}" }), { label: "cancel-invitation-replay" });
  s.post("owner", "cancel-invitation", json({ invitationId: "{{invAdmin}}" }), { label: "cancel-invitation-accepted" });
});

// ---------------------------------------------------------------------------------------------
// Scenario two: invite-only instance with the bootstrap owner, organization creation disabled.
// Forced password change, creation policy, sole-owner protection.
scenario(
  "bootstrap",
  { registration: "invite_only", organizationCreation: "disabled", bootstrap: true },
  (s) => {
    s.setup("bootstrapOwnerSignIn", { actor: "owner" });
    s.get("owner", "state", { label: "state-password-change-required" });
    for (const route of POST_ROUTES) {
      s.post("owner", route, json({}), { label: `${route}-password-change-required` });
    }
    s.get("owner", "api-keys", { label: "api-keys-password-change-required" });
    s.setup("bootstrapOwnerReady", { actor: "owner" });
    s.get("owner", "state", { label: "state-active" });
    s.post("owner", "create-organization", json({ name: "Second" }), { label: "create-organization-disabled" });
    s.post("owner", "create-organization", text("{"), { label: "create-organization-disabled-bad-body" });
    s.post("owner", "change-member-role", json({ memberId: "{{owner.member}}", role: "admin" }), {
      label: "change-member-role-sole-owner",
    });
    s.post("owner", "remove-member", json({ memberId: "{{owner.member}}" }), { label: "remove-member-sole-owner" });
    s.post("owner", "create-invitation", json({ email: "guest@example.test", role: "member" }), {
      label: "create-invitation-guest",
      capture: { invGuest: "id" },
    });
    s.get("anon", "state", { label: "state-anonymous-invitation", query: "?invitation={{invGuest}}" });
    s.get("anon", "state", { label: "state-anonymous-no-invitation" });
    s.setup("createAccount", {
      actor: "guest",
      name: "Gus Guest",
      email: "guest@example.test",
      password: "guest-password-1",
      invitation: "{{invGuest}}",
    });
    s.get("guest", "state", { label: "state-guest-invited", query: "?invitation={{invGuest}}" });
    s.post("guest", "accept-invitation", json({ invitationId: "{{invGuest}}" }), { label: "accept-invitation-guest" });
    s.get("owner", "state", { label: "state-owner-with-guest" });
    s.post("owner", "remove-member", json({ memberId: "{{guest.member}}" }), { label: "remove-member-guest" });
    s.get("guest", "state", { label: "state-guest-removed" });
  },
);

const document = { schemaVersion: 1, appUrl: APP_URL, scenarios };
process.stdout.write(`${JSON.stringify(document, null, 2)}\n`);
