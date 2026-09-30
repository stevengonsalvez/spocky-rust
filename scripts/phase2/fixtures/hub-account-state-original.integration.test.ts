import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, it } from "vitest";
import { startProductionRuntime, stopProductionRuntime } from "./index.js";

const APP_URL = "http://localhost:3000";
const EMAIL = "owner@example.test";
const INVITED_EMAIL = "member@example.test";
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

let root: string;
let previousEnvironment: Map<string, string | undefined>;

beforeEach(async () => {
  root = await mkdtemp(join(tmpdir(), "hub-account-state-original-"));
  previousEnvironment = new Map(ENVIRONMENT_NAMES.map((name) => [name, process.env[name]]));
  delete process.env["DATABASE_URL"];
  process.env["PASEO_HUB_DATA_DIR"] = join(root, "database");
  process.env["PASEO_HUB_AUTH_SECRET"] = "account-state-original-secret-at-least-32-characters";
  process.env["PASEO_HUB_APP_URL"] = APP_URL;
  process.env["PASEO_REGISTRATION_MODE"] = "invite_only";
  process.env["PASEO_ORGANIZATION_CREATION"] = "disabled";
  process.env["PASEO_BOOTSTRAP_ORGANIZATION"] = "organization-1";
  process.env["PASEO_BOOTSTRAP_OWNER_EMAIL"] = EMAIL;
  process.env["PASEO_BOOTSTRAP_OWNER_PASSWORD"] = TEMPORARY_PASSWORD;
});

afterEach(async () => {
  await stopProductionRuntime().catch(() => undefined);
  for (const [name, value] of previousEnvironment) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  await rm(root, { recursive: true, force: true });
});

it("captures the pinned browser account state sequence", async () => {
  const output = process.env["PASEO_ACCOUNT_STATE_OUTPUT"];
  assert.ok(output, "PASEO_ACCOUNT_STATE_OUTPUT is required");
  const runtime = await startProductionRuntime();
  const states: Record<string, unknown> = {};

  states["signedOut"] = await readState(runtime);
  let cookie = await signIn(runtime, EMAIL, TEMPORARY_PASSWORD);
  states["passwordChangeRequired"] = await readState(runtime, cookie);

  await runtime.changePassword!(
    { currentPassword: TEMPORARY_PASSWORD, newPassword: REPLACEMENT_PASSWORD },
    new Headers({ cookie, origin: APP_URL }),
  );
  cookie = await signIn(runtime, EMAIL, REPLACEMENT_PASSWORD);
  states["appSetupRequired"] = await readState(runtime, cookie);

  await runtime.completeAppOnboarding!(
    new Request(`${APP_URL}/api/auth/paseo/complete-app-setup`, {
      method: "POST",
      headers: { cookie, origin: APP_URL },
    }),
  );
  const invitation = await runtime.auth(
    new Request(`${APP_URL}/api/auth/paseo/create-invitation`, {
      method: "POST",
      headers: { cookie, origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify({ email: "member@example.test", role: "member" }),
    }),
  );
  assert.equal(invitation.status, 201);
  const invitationBody = (await invitation.json()) as { id: string };
  const canceledInvitation = await runtime.auth(
    new Request(`${APP_URL}/api/auth/paseo/create-invitation`, {
      method: "POST",
      headers: { cookie, origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify({ email: "cancel@example.test", role: "admin" }),
    }),
  );
  assert.equal(canceledInvitation.status, 201);
  const canceledInvitationBody = (await canceledInvitation.json()) as { id: string };
  const canceled = await runtime.auth(
    new Request(`${APP_URL}/api/auth/paseo/cancel-invitation`, {
      method: "POST",
      headers: { cookie, origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify({ invitationId: canceledInvitationBody.id }),
    }),
  );
  assert.equal(canceled.status, 200);
  const canceledBody = await canceled.json();
  states["active"] = await readState(runtime, cookie);

  const missingInvitation = await signUp(runtime, {
    name: "Member",
    email: INVITED_EMAIL,
    password: "member-password",
  });
  const unknownInvitation = await signUp(runtime, {
    name: "Member",
    email: INVITED_EMAIL,
    password: "member-password",
    invitation: "unknown",
  });
  const wrongEmail = await signUp(runtime, {
    name: "Wrong",
    email: "wrong@example.test",
    password: "member-password",
    invitation: invitationBody.id,
  });
  const invalidEmail = await signUp(runtime, {
    name: "Member",
    email: "invalid",
    password: "member-password",
    invitation: invitationBody.id,
  });

  const signedUp = await signUp(runtime, {
    name: "Invited Member",
    email: INVITED_EMAIL,
    password: "member-password",
    invitation: invitationBody.id,
  });
  assert.equal(signedUp.status, 200);
  const invitedCookie = await signIn(runtime, INVITED_EMAIL, "member-password");
  const accepted = await runtime.auth(
    new Request(`${APP_URL}/api/auth/paseo/accept-invitation`, {
      method: "POST",
      headers: { cookie: invitedCookie, origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify({ invitationId: invitationBody.id }),
    }),
  );
  assert.equal(accepted.status, 200);
  const acceptedBody = await accepted.json();
  states["invitedActive"] = await readState(runtime, invitedCookie);
  states["ownerAfterAcceptance"] = await readState(runtime, cookie);

  await writeFile(
    output,
    `${JSON.stringify({
      schemaVersion: 1,
      baseline: process.env["PASEO_HUB_BASELINE"],
      operations: {
        createInvitationStatus: invitation.status,
        cancelInvitationStatus: canceled.status,
        cancelInvitationBody: canceledBody,
        signUpInvitedStatus: signedUp.status,
        acceptInvitationStatus: accepted.status,
        acceptInvitationBody: acceptedBody,
        admissionWithoutInvitation: await responseSummary(missingInvitation),
        admissionWithUnknownInvitation: await responseSummary(unknownInvitation),
        admissionWithWrongEmail: await responseSummary(wrongEmail),
        admissionWithInvalidEmail: await responseSummary(invalidEmail),
      },
      states,
    }, null, 2)}\n`,
  );
});

type Runtime = Awaited<ReturnType<typeof startProductionRuntime>>;

async function readState(runtime: Runtime, cookie?: string): Promise<unknown> {
  const headers = cookie === undefined ? undefined : { cookie };
  const response = await runtime.browserAccount!(
    new Request(`${APP_URL}/api/auth/paseo/state`, { headers }),
  );
  assert.equal(response.status, 200);
  return response.json();
}

async function signIn(
  runtime: Runtime,
  email: string,
  password: string,
): Promise<string> {
  const response = await runtime.auth(
    new Request(`${APP_URL}/api/auth/sign-in/email`, {
      method: "POST",
      headers: { origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify({ email, password }),
    }),
  );
  assert.equal(response.status, 200);
  const cookie = response.headers.get("set-cookie")?.match(/^(?:[^;]+);/u)?.[0]?.slice(0, -1);
  assert.ok(cookie, "sign-in did not issue a session cookie");
  return cookie;
}

async function signUp(runtime: Runtime, body: Record<string, string>): Promise<Response> {
  return runtime.auth(
    new Request(`${APP_URL}/api/auth/sign-up/email`, {
      method: "POST",
      headers: { origin: APP_URL, "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

async function responseSummary(response: Response): Promise<unknown> {
  return { status: response.status, body: await response.json() };
}
