import { readFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "vitest";

const referenceRoot = process.env.PASEO_REFERENCE_ROOT;
const repositoryRoot = process.env.SPOCKY_REPOSITORY_ROOT;
if (!referenceRoot) throw new Error("PASEO_REFERENCE_ROOT is required");
if (!repositoryRoot) throw new Error("SPOCKY_REPOSITORY_ROOT is required");

test("captures every pinned plugin daemon RPC shape", async () => {
  const messagesUrl = pathToFileURL(
    path.join(referenceRoot, "packages/protocol/src/messages.ts"),
  ).href;
  const { SessionInboundMessageSchema, SessionOutboundMessageSchema } = await import(messagesUrl);
  const fixture = JSON.parse(
    readFileSync(
      path.join(
        repositoryRoot,
        "crates/spocky-plugin-pilot/tests/fixtures/plugin_daemon_rpc_cases.json",
      ),
      "utf8",
    ),
  );
  const capture = {
    requests: fixture.requests.map((value: unknown) => SessionInboundMessageSchema.parse(value)),
    responses: fixture.responses.map((value: unknown) => SessionOutboundMessageSchema.parse(value)),
    errors: fixture.errors.map((value: unknown) => SessionOutboundMessageSchema.parse(value)),
    notifications: fixture.notifications.map((value: unknown) =>
      SessionOutboundMessageSchema.parse(value),
    ),
    rejections: [
      ...fixture.rejectedRequests.map(
        (value: unknown) => !SessionInboundMessageSchema.safeParse(value).success,
      ),
      ...fixture.rejectedResponses.map(
        (value: unknown) => !SessionOutboundMessageSchema.safeParse(value).success,
      ),
    ],
  };
  console.log(`PLUGIN_DAEMON_RPC_BASELINE ${JSON.stringify(capture)}`);
});
