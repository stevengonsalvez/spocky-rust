import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

type CancellationStatus = "not_running" | "settled" | "refused";

async function main(): Promise<void> {
  const stateRoot = process.env.PASEO_DIFFERENTIAL_STATE;
  const referenceRoot = process.env.PASEO_REFERENCE_RUNTIME;
  if (!stateRoot) throw new Error("PASEO_DIFFERENTIAL_STATE is required");
  if (!referenceRoot) throw new Error("PASEO_REFERENCE_RUNTIME is required");

  const lifecycleModule = await import(
    pathToFileURL(
      path.join(referenceRoot, "packages/server/src/server/agent/lifecycle-command.ts"),
    ).href,
  );
  const { cancelAgentRunCommand } = lifecycleModule;
  const logger = {
    trace() {},
    debug() {},
    warn() {},
  };

  const cases: Array<{
    id: string;
    inFlight: boolean;
    cancellation: CancellationStatus;
  }> = [
    { id: "not_running", inFlight: false, cancellation: "not_running" },
    { id: "settled", inFlight: true, cancellation: "settled" },
    { id: "race", inFlight: true, cancellation: "not_running" },
    { id: "refused", inFlight: true, cancellation: "refused" },
  ];

  const results = [];
  for (const scenario of cases) {
    const agent = {
      id: "agent-1",
      cwd: "/workspace/project",
      lifecycle: scenario.inFlight ? "running" : "idle",
    };
    const manager = {
      getAgent() {
        return agent;
      },
      hasInFlightRun() {
        return scenario.inFlight;
      },
      async cancelAgentRun() {
        if (scenario.cancellation !== "refused") agent.lifecycle = "idle";
        return { status: scenario.cancellation };
      },
    };
    try {
      const result = await cancelAgentRunCommand({ agentManager: manager, logger }, agent.id);
      results.push({
        id: scenario.id,
        ok: true,
        cancelled: result.cancelled,
        lifecycle: agent.lifecycle,
        error: null,
      });
    } catch (error) {
      results.push({
        id: scenario.id,
        ok: false,
        cancelled: false,
        lifecycle: agent.lifecycle,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  const outputRoot = path.join(stateRoot, "output");
  await mkdir(outputRoot, { recursive: true });
  await writeFile(path.join(outputRoot, "structured.json"), JSON.stringify(results));
  await writeFile(
    path.join(outputRoot, "counts.json"),
    JSON.stringify({ fixtures: results.length, assertions: results.length * 4 }),
  );
  process.stdout.write(`lifecycle cases ${results.length}\n`);
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.stderr.write(`${message}\n`);
  process.exitCode = 1;
});
