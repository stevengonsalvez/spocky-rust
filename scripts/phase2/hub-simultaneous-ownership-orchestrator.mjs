import { spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { open, readFile, stat, unlink, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join } from "node:path";

const EXPECTED_SOURCE_SHA256 = "cf3b965451bd8cd9203f16118bdda8df80cec096b51d8d7f3e5b0456dc5f92e7";
let active = [];
let cleanupPromise;
let shutdownPromise;

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => void terminateForSignal(signal));
}

if (process.argv[2] === "--self-test-signal-cleanup") {
  try {
    await selfTestSignalCleanup();
  } finally {
    await cleanupActive();
  }
} else {
  try {
    await qualifyOwnershipRaces();
  } finally {
    await cleanupActive();
  }
}

async function qualifyOwnershipRaces() {
  const [candidateBinary, database, baselineSource, baselineCommit] = process.argv.slice(2);
  if (!baselineCommit) throw new Error("four arguments are required");
  const lockPath = join(database, ".paseo-hub.lock");
  const source = await readFile(baselineSource, "utf8");
  const sourceSha256 = createHash("sha256").update(source).digest("hex");
  if (sourceSha256 !== EXPECTED_SOURCE_SHA256) {
    throw new Error(`pinned baseline source hash mismatch: ${sourceSha256}`);
  }
  const sourceMode = (await stat(baselineSource)).mode;
  for (const spelling of [
    'open(path, "wx", 0o600)',
    "const OWNER_READ_ATTEMPTS = 10",
    "const OWNER_READ_DELAY_MS = 10",
    "const existingOwner = await readLockOwner(path)",
    "existingOwner !== undefined && processIsRunning(existingOwner.pid)",
    "await unlink(path)",
  ]) {
    if (!source.includes(spelling)) throw new Error(`pinned baseline spelling missing: ${spelling}`);
  }

  const candidatePaused = startCandidate(candidateBinary, database, "pause-before-open");
  const pause = await nextJson(candidatePaused, 10_000);
  const baselineOwner = await baselineAcquireComplete(lockPath);
  const ownerBeforeCandidateResume = await identitySnapshot(lockPath);
  candidatePaused.stdin.end("open\n");
  const rejected = await nextJson(candidatePaused, 10_000);
  const rejectedExit = await waitForExit(candidatePaused, 10_000);
  const ownerAfterCandidateExit = await identitySnapshot(lockPath);
  await baselineRelease(lockPath, baselineOwner);

  const partial = await baselineCreatePartial(lockPath);
  const candidateOwner = startCandidate(candidateBinary, database, "hold");
  const candidateReady = await nextJson(candidateOwner, 10_000);
  const candidateIdentity = await identitySnapshot(lockPath);
  const partialContent = JSON.stringify(partial.owner);
  await partial.handle.writeFile(partialContent);
  const partialIdentity = await handleIdentitySnapshot(partial.handle, partialContent);
  const bothPartialProcessesLive = isRunning(process.pid) && isRunning(candidateOwner.pid);
  candidateOwner.stdin.end("close\n");
  const partialClosed = await nextJson(candidateOwner, 10_000);
  const partialCandidateExit = await waitForExit(candidateOwner, 10_000);
  await partial.handle.close();
  await baselineRelease(lockPath, partial.owner);
  await unlinkIfPresent(lockPath);

  const staleOwner = { pid: 2_147_483_647, token: "stale-inode-a" };
  const staleHandle = await open(lockPath, "wx", 0o600);
  await staleHandle.writeFile(JSON.stringify(staleOwner));
  await staleHandle.close();
  const candidateReadIdentity = await identitySnapshot(lockPath);
  await unlink(lockPath);
  const liveBaselineOwner = { pid: process.pid, token: randomUUID() };
  const liveBaselineHandle = await open(lockPath, "wx", 0o600);
  await liveBaselineHandle.writeFile(JSON.stringify(liveBaselineOwner));
  await liveBaselineHandle.close();
  const liveBaselineIdentity = await identitySnapshot(lockPath);

  const modeledCandidate = startCandidate(candidateBinary, database, "pause-before-open");
  await nextJson(modeledCandidate, 10_000);
  await unlink(lockPath);
  const replacementOwner = {
    pid: modeledCandidate.pid,
    token: randomUUID(),
    protocol: "os-file-lock-v1",
  };
  const replacementHandle = await open(lockPath, "wx", 0o600);
  await replacementHandle.writeFile(JSON.stringify(replacementOwner));
  await replacementHandle.close();
  const replacementIdentity = await identitySnapshot(lockPath);
  const bothToctouProcessesLive = isRunning(process.pid) && isRunning(modeledCandidate.pid);
  await forceStop(modeledCandidate);
  removeActive(modeledCandidate);
  await unlinkIfPresent(lockPath);

  process.stdout.write(`${JSON.stringify({
    baseline: {
      commit: baselineCommit,
      sourceSha256,
      sourceHashExact: true,
      sourceSpellingsExact: true,
      disposableSourceReadOnly: (sourceMode & 0o222) === 0,
      execution: "handwritten-lock-operation-model",
      pinnedDatabaseRuntimeExecuted: false,
    },
    candidatePausedBeforeOwner: {
      scope: "actual-candidate-process-paused-before-guard",
      pause,
      baselineOpened: true,
      ownerBeforeCandidateResume,
      candidateRejected: rejected,
      rejectedExit,
      ownerAfterCandidateExit,
      completedLiveOwnerPreserved: sameIdentity(ownerBeforeCandidateResume, ownerAfterCandidateExit),
    },
    actualCandidateAfterGuardUnitTest: {
      test: "directory_lock::tests::completed_legacy_owner_wins_while_candidate_is_paused_after_guard",
      assertion: "completed live owner inode and content preserved",
    },
    baselinePausedAfterExclusiveCreate: {
      scope: "handwritten-pinned-operation-model",
      emptyOwnerFileCreated: true,
      candidateReady,
      baselineWriteCompleted: true,
      partialIdentity,
      candidateIdentity,
      distinctInodes: partialIdentity.inode !== candidateIdentity.inode,
      visibleOwnerProtocol: JSON.parse(candidateIdentity.content).protocol,
      bothProcessesLive: bothPartialProcessesLive,
      closed: partialClosed,
      candidateExit: partialCandidateExit,
    },
    completedLiveRecordStaleUnlinkToctou: {
      scope: "deterministic-handwritten-operation-model",
      candidateReadIdentity,
      liveBaselineIdentity,
      replacementIdentity,
      allInodesDistinct: new Set([
        candidateReadIdentity.inode,
        liveBaselineIdentity.inode,
        replacementIdentity.inode,
      ]).size === 3,
      baselineOwnerWasLive: liveBaselineOwner.pid === process.pid && isRunning(process.pid),
      completedLiveRecordDeletedByModeledCandidate: liveBaselineIdentity.content !== replacementIdentity.content,
      bothProcessesLive: bothToctouProcessesLive,
    },
    conclusion: {
      exclusiveCandidateCreate: "implemented",
      parity: "not-claimed",
      residualExceptions: [
        "paused-incomplete-writer",
        "completed-live-record-stale-unlink-toctou",
      ],
      mitigationBoundary: "identity recheck narrows but cannot make path unlink conditional and atomic",
    },
  }, null, 2)}\n`);
}

function startCandidate(candidateBinary, database, mode) {
  const child = spawn(candidateBinary, [mode, database], {
    detached: process.platform !== "win32",
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.name = `candidate-${mode}`;
  child.ownedPids = [child.pid];
  child.lines = createInterface({ input: child.stdout });
  child.stderrText = "";
  child.stderr.on("data", (chunk) => (child.stderrText += chunk));
  child.exitPromise = new Promise((resolve) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  active.push(child);
  return child;
}

async function baselineAcquireComplete(lockPath) {
  const owner = { pid: process.pid, token: randomUUID() };
  const handle = await open(lockPath, "wx", 0o600);
  await handle.writeFile(JSON.stringify(owner));
  await handle.close();
  return owner;
}

async function baselineCreatePartial(lockPath) {
  const owner = { pid: process.pid, token: randomUUID() };
  const handle = await open(lockPath, "wx", 0o600);
  return { handle, owner };
}

async function baselineRelease(lockPath, owner) {
  try {
    const visible = JSON.parse(await readFile(lockPath, "utf8"));
    if (visible.token === owner.token) await unlink(lockPath);
  } catch (error) {
    if (error.code !== "ENOENT" && !(error instanceof SyntaxError)) throw error;
  }
}

async function identitySnapshot(path) {
  const metadata = await stat(path);
  return { inode: metadata.ino, content: await readFile(path, "utf8") };
}

async function handleIdentitySnapshot(handle, content) {
  const metadata = await handle.stat();
  return { inode: metadata.ino, content };
}

function sameIdentity(left, right) {
  return left.inode === right.inode && left.content === right.content;
}

async function unlinkIfPresent(path) {
  try {
    await unlink(path);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
}

function nextJson(child, timeoutMs) {
  return withTimeout(new Promise((resolve, reject) => {
    child.lines.once("line", (line) => resolve(JSON.parse(line)));
    child.once("exit", (code, signal) => reject(new Error(
      `${child.name} exited before event: code=${code} signal=${signal} stderr=${child.stderrText}`,
    )));
  }), timeoutMs, `${child.name} event`);
}

async function waitForExit(child, timeoutMs) {
  const outcome = await withTimeout(child.exitPromise, timeoutMs, `${child.name} exit`)
    .catch(async (error) => {
      await forceStop(child);
      throw error;
    });
  if (!(await waitForOwnedGroupGone(child, 2_000))) {
    await forceStop(child);
    throw new Error(`${child.name} left its owned process group alive`);
  }
  removeActive(child);
  return outcome;
}

async function forceStop(child) {
  if (!(await ownedProcessAlive(child))) {
    await withTimeout(child.exitPromise, 2_000, `${child.name} reap`);
    return;
  }
  signalOwnedProcess(child, "SIGTERM");
  if (!(await waitForOwnedGroupGone(child, 2_000))) signalOwnedProcess(child, "SIGKILL");
  if (!(await waitForOwnedGroupGone(child, 2_000))) {
    throw new Error(`owned process group ${child.pid} survived SIGKILL`);
  }
  await withTimeout(child.exitPromise, 2_000, `${child.name} reap`);
}

async function cleanupActive() {
  if (cleanupPromise) return cleanupPromise;
  const children = [...active];
  cleanupPromise = (async () => {
    const outcomes = await Promise.allSettled(children.map(forceStop));
    active = active.filter((child) => !children.includes(child));
    const failures = outcomes
      .filter((outcome) => outcome.status === "rejected")
      .map((outcome) => outcome.reason);
    if (failures.length > 0) throw new AggregateError(failures, "owned process cleanup failed");
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
  const exactProcessAlive = child.ownedPids.some(isRunning);
  if (process.platform === "win32") return exactProcessAlive;
  try {
    process.kill(-child.pid, 0);
    return true;
  } catch (error) {
    if (error.code === "ESRCH" || error.code === "EPERM") return exactProcessAlive;
    throw error;
  }
}

function signalOwnedProcess(child, signal) {
  try {
    if (process.platform === "win32") child.kill(signal);
    else process.kill(-child.pid, signal);
  } catch (error) {
    if (error.code === "ESRCH") return;
    if (error.code !== "EPERM") throw error;
    for (const pid of child.ownedPids) {
      try {
        process.kill(pid, signal);
      } catch (pidError) {
        if (pidError.code !== "ESRCH") throw pidError;
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

function removeActive(child) {
  active = active.filter((entry) => entry !== child);
}

function withTimeout(promise, timeoutMs, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function isRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error.code === "ESRCH") return false;
    if (error.code === "EPERM") return true;
    throw error;
  }
}

async function selfTestSignalCleanup() {
  if (process.platform === "win32") throw new Error("signal cleanup self-test requires Unix");
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
  owner.exitPromise = new Promise((resolve) => {
    owner.once("exit", (code, signal) => resolve({ code, signal }));
  });
  active.push(owner);
  const descendantPid = Number(await withTimeout(new Promise((resolve) => {
    owner.stdout.once("data", (chunk) => resolve(chunk.toString().trim()));
  }), 2_000, "cleanup descendant readiness"));
  if (!Number.isSafeInteger(descendantPid) || descendantPid <= 1) {
    throw new Error(`invalid cleanup descendant PID: ${descendantPid}`);
  }
  owner.ownedPids.push(descendantPid);
  process.kill(descendantPid, "SIGSTOP");
  const fixture = { ownerPid: owner.pid, descendantPid };
  if (process.argv[3]) {
    await writeFile(process.argv[3], `${JSON.stringify(fixture)}\n`);
  }
  if (process.argv[4] === "delay-ready") {
    await delay(60_000);
  }
  process.stdout.write(`${JSON.stringify(fixture)}\n`);
  await new Promise(() => {});
}
