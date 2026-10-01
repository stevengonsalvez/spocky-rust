import { createWriteStream, statSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

let active = [];
let cleanupPromise;
let shutdownPromise;
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => void terminateForSignal(signal));
}

if (process.argv[2] === "--self-test-cleanup") {
  try {
    await selfTestCleanup();
    process.stdout.write('{"processGroupCleanup":"passed"}\n');
  } finally {
    await cleanupActive();
  }
  process.exitCode = 0;
} else if (process.argv[2] === "--self-test-signal-cleanup") {
  try {
    await selfTestSignalCleanup();
  } catch (error) {
    if (shutdownPromise) await shutdownPromise;
    throw error;
  }
} else {
  try {
    await qualifyMixedOwnership();
  } catch (error) {
    if (shutdownPromise) await shutdownPromise;
    throw error;
  }
}

async function qualifyMixedOwnership() {
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

  try {
    const baseline = start("baseline-owner", tsx, [
      baselineDriver,
      "hold",
      database,
    ], {
      ...process.env,
    }, processesPath);
    const baselineReady = await nextJson(baseline, 90_000);
    baseline.ownedPids.push(baselineReady.ownerPid);
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
    if (
      baselineExit.code !== 0 ||
      baselineExit.signal !== null ||
      !baselineExit.processGroupGone
    ) {
      throw new Error(
        `baseline owner did not close cleanly: ${JSON.stringify(baselineExit)}`,
      );
    }
    const unchangedDirectory = statSync(database).ino === directoryInode;

    const candidate = start(
      "candidate-owner",
      candidateBinary,
      ["hold", database],
      candidateEnvironment,
      processesPath,
    );
    const candidateReady = await nextJson(candidate, 90_000);
    candidate.ownedPids.push(candidateReady.retainedProcessId);
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
    if (
      candidateExit.code !== 0 ||
      candidateExit.signal !== null ||
      !candidateExit.processGroupGone
    ) {
      throw new Error(
        `candidate owner did not close cleanly: ${
          JSON.stringify(candidateExit)
        }`,
      );
    }

    const reverseGuaranteed = baselineExcluded.opened === false &&
      baselineExcluded.error.includes("already in use");
    const report = {
      scope: "ordered-live-starts-only",
      platform: {
        os: process.platform,
        arch: process.arch,
        processGroupCleanup: process.platform === "win32"
          ? "direct-child-only"
          : "dedicated-process-group",
      },
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
        baselineExit: "bounded-clean",
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
      storageObservation: {
        baselineJournalRows: baselineReady.journalRows,
        candidateJournalRows: candidateReady.journalRows,
        candidateMigrationsApplied: candidateReady.migrationsApplied,
      },
      shutdown: {
        candidateExitCode: candidateExit.code,
        candidateExitSignal: candidateExit.signal,
        candidateProcessGroupGone: candidateExit.processGroupGone,
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
    await cleanupActive();
  }
}

function start(name, command, args, env, processesPath) {
  const stderrPath = `${processesPath}.${name}.stderr`;
  const stderr = createWriteStream(stderrPath, { flags: "w" });
  const child = spawn(command, args, {
    env,
    detached: process.platform !== "win32",
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.name = name;
  child.stderr.pipe(stderr);
  child.stderrPath = stderrPath;
  child.startedAt = new Date().toISOString();
  child.ownedPids = [child.pid];
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
  const processGroupGone = await waitForOwnedGroupGone(child, 2_000);
  if (!processGroupGone) {
    await forceStop(child);
  }
  active = active.filter((entry) => entry !== child);
  return { ...outcome, processGroupGone };
}

async function run(name, command, args, env, timeoutMs) {
  const child = spawn(command, args, {
    env,
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.ownedPids = [child.pid];
  child.stdout.on("data", (chunk) => (stdout += chunk));
  child.stderr.on("data", (chunk) => (stderr += chunk));
  active.push(child);
  const outcome = await waitForExit(child, timeoutMs);
  if (!(await waitForOwnedGroupGone(child, 2_000))) {
    await forceStop(child);
    throw new Error(`${name} left its owned process group alive`);
  }
  active = active.filter((entry) => entry !== child);
  if (outcome.code !== 0) throw new Error(`${name} failed: ${stderr}`);
  return { stdout, process: { ...outcome, name, stderr } };
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
  if (!(await ownedProcessAlive(child))) return;
  signalOwnedProcess(child, "SIGTERM");
  if (await waitForOwnedGroupGone(child, 2_000)) return;
  signalOwnedProcess(child, "SIGKILL");
  if (!(await waitForOwnedGroupGone(child, 2_000))) {
    throw new Error(`owned process group ${child.pid} survived SIGKILL`);
  }
}

async function cleanupActive() {
  if (cleanupPromise) return cleanupPromise;
  const children = [...active];
  cleanupPromise = (async () => {
    const outcomes = await Promise.allSettled(
      children.map((child) => forceStop(child)),
    );
    active = active.filter((child) => !children.includes(child));
    const failures = outcomes
      .filter((outcome) => outcome.status === "rejected")
      .map((outcome) => outcome.reason);
    if (failures.length > 0) {
      throw new AggregateError(failures, "owned process cleanup failed");
    }
  })();
  try {
    await cleanupPromise;
  } finally {
    cleanupPromise = undefined;
  }
}

async function terminateForSignal(signal) {
  if (shutdownPromise) return shutdownPromise;
  shutdownPromise = (async () => {
    let exitCode = { SIGHUP: 129, SIGINT: 130, SIGTERM: 143 }[signal] ?? 1;
    try {
      await cleanupActive();
    } catch (error) {
      exitCode = 1;
      process.stderr.write(`${error.stack ?? error}\n`);
    }
    process.exit(exitCode);
  })();
  return shutdownPromise;
}

async function ownedProcessAlive(child) {
  const exactProcessAlive = child.ownedPids.some((pid) => processExists(pid));
  if (process.platform === "win32") {
    return exactProcessAlive;
  }
  try {
    process.kill(-child.pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH" || error?.code === "EPERM") {
      return exactProcessAlive;
    }
    throw error;
  }
}

function processExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

function signalOwnedProcess(child, signal) {
  try {
    if (process.platform === "win32") {
      child.kill(signal);
    } else {
      process.kill(-child.pid, signal);
    }
  } catch (error) {
    if (error?.code === "ESRCH") return;
    if (error?.code !== "EPERM") throw error;
    for (const pid of child.ownedPids) {
      try {
        process.kill(pid, signal);
      } catch (pidError) {
        if (pidError?.code !== "ESRCH") throw pidError;
      }
    }
  }
}

async function waitForOwnedGroupGone(child, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (await ownedProcessAlive(child)) {
    if (Date.now() >= deadline) return false;
    await delay(20);
  }
  return true;
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

async function selfTestCleanup() {
  if (process.platform === "win32") return;
  const owner = await createCleanupFixture();
  try {
    process.kill(owner.ownedPids[1], "SIGSTOP");
    await forceStop(owner);
    if (await ownedProcessAlive(owner)) {
      throw new Error("cleanup self-test process group remains alive");
    }
  } finally {
    active = active.filter((entry) => entry !== owner);
    await forceStop(owner);
  }
}

async function selfTestSignalCleanup() {
  if (process.platform === "win32") {
    throw new Error("signal cleanup self-test requires Unix process groups");
  }
  const owner = await createCleanupFixture();
  process.kill(owner.ownedPids[1], "SIGSTOP");
  owner.lines = createInterface({ input: owner.stdout });
  process.stdout.write(`${
    JSON.stringify({
      ownerPid: owner.pid,
      descendantPid: owner.ownedPids[1],
    })
  }\n`);
  await nextJson(owner, 60_000);
}

async function createCleanupFixture() {
  const program = `
    const { spawn } = require("node:child_process");
    const child = spawn("/bin/sh", ["-c", "trap '' TERM; while :; do sleep 1; done"], {
      stdio: "ignore"
    });
    process.stdout.write(String(child.pid) + "\\n");
    setInterval(() => {}, 1000);
  `;
  const owner = spawn(process.execPath, ["-e", program], {
    detached: true,
    stdio: ["ignore", "pipe", "ignore"],
  });
  owner.name = "cleanup-self-test-owner";
  owner.ownedPids = [owner.pid];
  active.push(owner);
  try {
    const descendantPid = Number(
      await withTimeout(
        new Promise((resolve) =>
          owner.stdout.once("data", (chunk) => resolve(chunk.toString().trim()))
        ),
        2_000,
        "cleanup self-test descendant readiness",
      ),
    );
    if (!Number.isSafeInteger(descendantPid) || descendantPid <= 1) {
      throw new Error(
        `invalid cleanup self-test descendant PID: ${descendantPid}`,
      );
    }
    owner.ownedPids.push(descendantPid);
    return owner;
  } catch (error) {
    active = active.filter((entry) => entry !== owner);
    await forceStop(owner);
    throw error;
  }
}
