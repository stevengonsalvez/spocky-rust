import { createWriteStream, statSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

const [
  tsx,
  baselineDriver,
  candidateBinary,
  database,
  eventsPath,
  processesPath,
] = process.argv.slice(2);
if (!processesPath) throw new Error("six arguments are required");

const candidateEnvironment = {
  ...process.env,
  SPOCKY_NODE: process.execPath,
};
const processes = [];
const events = [];
let active = [];

try {
  const baseline = start("baseline-owner", tsx, [
    baselineDriver,
    "hold",
    database,
  ], {
    ...process.env,
  });
  const baselineReady = await nextJson(baseline, 90_000);
  events.push(baselineReady);

  const directoryInode = statSync(database).ino;
  const candidateProbe = await run(
    "candidate-forward-probe",
    candidateBinary,
    ["try-open", database],
    candidateEnvironment,
    60_000,
  );
  const candidateExcluded = parseSingleJson(candidateProbe.stdout);
  events.push(candidateExcluded);

  const baselineExit = await closeOwner(baseline, 30_000);
  const unchangedDirectory = statSync(database).ino === directoryInode;

  const candidate = start(
    "candidate-owner",
    candidateBinary,
    ["hold", database],
    candidateEnvironment,
  );
  const candidateReady = await nextJson(candidate, 90_000);
  events.push(candidateReady);

  const baselineProbe = await run(
    "baseline-reverse-probe",
    tsx,
    [baselineDriver, "try-open", database],
    process.env,
    60_000,
  );
  const baselineExcluded = parseSingleJson(baselineProbe.stdout);
  events.push(baselineExcluded);
  const candidateExit = await closeOwner(candidate, 30_000);

  const reverseGuaranteed = baselineExcluded.opened === false &&
    baselineExcluded.error.includes("already in use");
  const report = {
    scope: "ordered-live-starts-only",
    limitations: [
      "simultaneous_pre_owner_record_race_unqualified",
      "schema_downgrade_unqualified",
    ],
    forwardExclusion: {
      baselineReady: baselineReady.event === "ready",
      candidateExcluded: candidateExcluded.opened === false,
      candidateError: candidateExcluded.error,
    },
    handoff: {
      baselineExit: baselineExit.code === 0 ? "bounded-clean" : "failed",
      directoryRecreated: !unchangedDirectory,
      candidateOpenedUnchangedDirectory: candidateReady.event === "ready" &&
        unchangedDirectory,
      baselineMarkerPayload: candidateReady.baselineMarkerPayload,
    },
    reverseExclusion: {
      candidateReady: candidateReady.event === "ready",
      baselineExcluded: reverseGuaranteed,
      baselineErrorContains: reverseGuaranteed ? "already in use" : "",
    },
    compatibilityMechanism: reverseGuaranteed
      ? {
        status: "not-required-for-ordered-starts",
        reason: "shared live PID owner record excludes ordered mixed starts",
      }
      : {
        status: "required",
        reason: "pinned baseline ignored candidate ownership",
      },
  };
  processes.push(
    baselineExit,
    candidateProbe.process,
    baselineProbe.process,
    candidateExit,
  );
  await writeFile(eventsPath, `${JSON.stringify(events, null, 2)}\n`);
  await writeFile(processesPath, `${JSON.stringify(processes, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify(report)}\n`);
} finally {
  await Promise.all(active.map((child) => forceStop(child)));
}

function start(name, command, args, env) {
  const stderrPath = `${processesPath}.${name}.stderr`;
  const stderr = createWriteStream(stderrPath, { flags: "w" });
  const child = spawn(command, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  child.name = name;
  child.stderr.pipe(stderr);
  child.stderrPath = stderrPath;
  child.startedAt = new Date().toISOString();
  child.lines = createInterface({ input: child.stdout });
  active.push(child);
  return child;
}

async function nextJson(child, timeoutMs) {
  const line = await withTimeout(
    new Promise((resolve, reject) => {
      child.lines.once("line", resolve);
      child.once(
        "exit",
        (code, signal) =>
          reject(
            new Error(
              `${child.name} exited before ready: code=${code} signal=${signal}`,
            ),
          ),
      );
    }),
    timeoutMs,
    `${child.name} readiness`,
  );
  return JSON.parse(line);
}

async function closeOwner(child, timeoutMs) {
  child.stdin.end("close\n");
  const outcome = await waitForExit(child, timeoutMs);
  active = active.filter((entry) => entry !== child);
  return outcome;
}

async function run(name, command, args, env, timeoutMs) {
  const child = spawn(command, args, {
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => (stdout += chunk));
  child.stderr.on("data", (chunk) => (stderr += chunk));
  active.push(child);
  const process = await waitForExit(child, timeoutMs);
  active = active.filter((entry) => entry !== child);
  if (process.code !== 0) throw new Error(`${name} failed: ${stderr}`);
  return { stdout, process: { ...process, name, stderr } };
}

async function waitForExit(child, timeoutMs) {
  const result = await withTimeout(
    new Promise((resolve) =>
      child.once("exit", (code, signal) =>
        resolve({
          name: child.name,
          pid: child.pid,
          startedAt: child.startedAt,
          exitedAt: new Date().toISOString(),
          code,
          signal,
        }))
    ),
    timeoutMs,
    `${child.name ?? "process"} exit`,
  ).catch(async (error) => {
    await forceStop(child);
    throw error;
  });
  return result;
}

async function forceStop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  await Promise.race([waitWithoutTimeout(child), delay(2_000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await Promise.race([waitWithoutTimeout(child), delay(2_000)]);
  }
}

function waitWithoutTimeout(child) {
  return new Promise((resolve) => child.once("exit", resolve));
}

function withTimeout(promise, timeoutMs, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`${label} timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function parseSingleJson(stdout) {
  const lines = stdout.trim().split("\n");
  if (lines.length !== 1) {
    throw new Error(`expected one JSON line, got ${lines.length}`);
  }
  return JSON.parse(lines[0]);
}
