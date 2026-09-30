import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { afterAll, beforeAll, describe, it } from "vitest";
import { z } from "zod";
import type { AccountMailer } from "./auth/account-emails.js";
import { composeEntitlements } from "./auth/entitlements.js";
import { createAuthServer, type AuthServer } from "./auth/server.js";
import { createDatabase } from "./db/pg.js";
import { embeddedDatabaseRuntime } from "./db/runtime/index.js";

const ORIGIN = "http://localhost:3000";
const output = process.env.PASEO_ACCOUNT_RECOVERY_OUTPUT;

describe("account recovery raw trace", () => {
  let close: (() => Promise<void>) | undefined;

  beforeAll(() => {
    assert.ok(output, "PASEO_ACCOUNT_RECOVERY_OUTPUT is required");
  });

  afterAll(async () => close?.());

  it("captures the pinned response and email boundary", async () => {
    const directory = process.env.PASEO_ACCOUNT_RECOVERY_DATA;
    assert.ok(directory, "PASEO_ACCOUNT_RECOVERY_DATA is required");
    const { runtime, locks } = await embeddedDatabaseRuntime(directory);
    await runtime.migrate();
    const database = createDatabase(runtime, locks);
    const entitlements = composeEntitlements(database, runtime);
    const mailer = new RecordingAccountMailer();
    const auth = createAuthServer({
      database: runtime,
      locks,
      entitlements: entitlements.service,
      secret: "account-recovery-trace-secret-at-least-32-characters",
      baseURL: ORIGIN,
      policy: { registrationMode: "open", organizationCreation: "open", bootstrap: undefined },
      accountMailer: mailer,
    });
    close = async () => {
      await auth.close();
      await entitlements.close();
      await database.close();
    };

    const signup = await post(auth, "/api/auth/sign-up/email", {
      name: "Verified User",
      email: "verified@example.test",
      password: "original-password",
      callbackURL: `${ORIGIN}/?auth=email-verification`,
    });
    const unverified = await post(auth, "/api/auth/sign-in/email", {
      email: "verified@example.test",
      password: "original-password",
    });
    const verification = await auth.handle(new Request(mailer.verifications[0]!.url));
    const verificationCookie = verification.headers.get("set-cookie");
    assert.ok(verificationCookie);
    const cookie = verificationCookie.match(/better-auth\.session_token=[^;]+/u)?.[0];
    assert.ok(cookie);
    const session = await auth.handle(
      new Request(`${ORIGIN}/api/auth/get-session`, { headers: { cookie } }),
    );
    const sessionBody = await session.json();

    await auth.requestPasswordReset!("verified@example.test", originHeaders());
    await auth.requestPasswordReset!("missing@example.test", originHeaders());
    const resetCallback = await auth.handle(new Request(mailer.passwordResets[0]!.url));
    const resetLocation = resetCallback.headers.get("location");
    assert.ok(resetLocation);
    const resetToken = new URL(resetLocation).searchParams.get("token");
    assert.ok(resetToken);
    await auth.resetPassword!(
      { token: resetToken, newPassword: "replacement-password" },
      originHeaders(),
    );
    const revoked = await auth.handle(
      new Request(`${ORIGIN}/api/auth/get-session`, { headers: { cookie } }),
    );
    const oldPassword = await post(auth, "/api/auth/sign-in/email", {
      email: "verified@example.test",
      password: "original-password",
    });
    const newPassword = await post(auth, "/api/auth/sign-in/email", {
      email: "verified@example.test",
      password: "replacement-password",
    });
    let replayCode: string | undefined;
    try {
      await auth.resetPassword!(
        { token: resetToken, newPassword: "another-password" },
        originHeaders(),
      );
    } catch (error) {
      replayCode = z.object({ body: z.object({ code: z.string() }) }).parse(error).body.code;
    }

    const trace = {
      baseline: process.env.PASEO_HUB_BASELINE,
      signup: { status: signup.status, setCookie: signup.headers.get("set-cookie") },
      verificationEmail: mailer.verifications[0],
      unverifiedSignIn: {
        status: unverified.status,
        body: await unverified.json(),
      },
      verification: {
        status: verification.status,
        location: verification.headers.get("location"),
        setCookie: verificationCookie,
      },
      session: {
        status: session.status,
        email: z.object({ user: z.object({ email: z.string() }) }).parse(sessionBody).user.email,
      },
      passwordResetEmails: mailer.passwordResets,
      resetCallback: { status: resetCallback.status, location: resetLocation },
      revokedSession: { status: revoked.status, body: await revoked.text() },
      oldPasswordStatus: oldPassword.status,
      newPasswordStatus: newPassword.status,
      replayCode,
    };
    await mkdir(dirname(output!), { recursive: true });
    await writeFile(output!, `${JSON.stringify(trace, null, 2)}\n`);
  }, 120_000);
});

class RecordingAccountMailer implements AccountMailer {
  readonly verifications: { url: string; token: string; email: string }[] = [];
  readonly passwordResets: { url: string; token: string; email: string }[] = [];

  sendVerificationEmail(email: Parameters<AccountMailer["sendVerificationEmail"]>[0]) {
    this.verifications.push({ url: email.url, token: email.token, email: email.user.email });
    return Promise.resolve();
  }

  sendPasswordReset(email: Parameters<AccountMailer["sendPasswordReset"]>[0]) {
    this.passwordResets.push({ url: email.url, token: email.token, email: email.user.email });
    return Promise.resolve();
  }
}

function post(auth: AuthServer, path: string, body: Record<string, unknown>): Promise<Response> {
  return auth.handle(
    new Request(`${ORIGIN}${path}`, {
      method: "POST",
      headers: { origin: ORIGIN, "content-type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

function originHeaders(): Headers {
  return new Headers({ origin: ORIGIN });
}
