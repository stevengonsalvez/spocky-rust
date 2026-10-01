import { spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { open, readFile, stat, unlink } from "node:fs/promises";
import { createInterface } from "node:readline";
import { join } from "node:path";

const [candidateBinary, database, baselineSource, baselineCommit] = process.argv.slice(2);
if (!baselineCommit) throw new Error("four arguments are required");

const lockPath = join(database, ".paseo-hub.lock");
const source = await readFile(baselineSource, "utf8");
const sourceMode = (await stat(baselineSource)).mode;
for (const spelling of ['open(path, "wx", 0o600)', "OWNER_READ_ATTEMPTS = 10", "unlink(path)"]) {
  if (!source.includes(spelling)) throw new Error(`pinned baseline spelling missing: ${spelling}`);
}

const children = new Set();
try {
  const candidatePaused = startCandidate("pause-before-open");
  const pause = await nextJson(candidatePaused, 10_000);
  const baselineOwner = await baselineAcquireComplete();
  candidatePaused.stdin.write("open\n");
  const rejected = await nextJson(candidatePaused, 10_000);
  const rejectedExit = await waitForExit(candidatePaused, 10_000);
  await baselineRelease(baselineOwner);

  const partial = await baselineCreatePartial();
  const candidateOwner = startCandidate("hold");
  const candidateReady = await nextJson(candidateOwner, 10_000);
  const candidateInode = (await stat(lockPath)).ino;
  await partial.handle.writeFile(JSON.stringify(partial.owner));
  const partialInode = (await partial.handle.stat()).ino;
  const visibleOwner = JSON.parse(await readFile(lockPath, "utf8"));
  const bothProcessesLive = isRunning(process.pid) && isRunning(candidateOwner.pid);
  candidateOwner.stdin.write("close\n");
  const closed = await nextJson(candidateOwner, 10_000);
  const candidateExit = await waitForExit(candidateOwner, 10_000);
  await partial.handle.close();
  await baselineRelease(partial.owner);

  process.stdout.write(`${JSON.stringify({
    baseline: {
      commit: baselineCommit,
      sourceSha256: createHash("sha256").update(source).digest("hex"),
      disposableSourceReadOnly: (sourceMode & 0o222) === 0,
    },
    candidatePausedBeforeOwner: {
      pause,
      baselineOpened: true,
      candidateRejected: rejected,
      candidateExit,
    },
    baselinePausedAfterExclusiveCreate: {
      emptyOwnerFileCreated: true,
      candidateReady,
      baselineWriteCompleted: true,
      partialInode,
      candidateInode,
      distinctInodes: partialInode !== candidateInode,
      visibleOwnerProtocol: visibleOwner.protocol,
      bothProcessesLive,
      closed,
      candidateExit,
    },
    conclusion: {
      exclusiveCandidateCreate: "implemented",
      pausedBaselineAfterCreate: "inherent-unresolved-race",
      reason: "pinned baseline treats an incomplete record as stale after bounded retries",
    },
  }, null, 2)}\n`);
} finally {
  await Promise.allSettled([...children].map(forceStop));
}

function startCandidate(mode) {
  const child = spawn(candidateBinary, [mode, database], {
    detached: true,
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.lines = createInterface({ input: child.stdout });
  child.stderrText = "";
  child.stderr.on("data", (chunk) => (child.stderrText += chunk));
  children.add(child);
  return child;
}

async function baselineAcquireComplete() {
  const owner = { pid: process.pid, token: randomUUID() };
  const handle = await open(lockPath, "wx", 0o600);
  await handle.writeFile(JSON.stringify(owner));
  await handle.close();
  return owner;
}

async function baselineCreatePartial() {
  const owner = { pid: process.pid, token: randomUUID() };
  const handle = await open(lockPath, "wx", 0o600);
  return { handle, owner };
}

async function baselineRelease(owner) {
  try {
    const visible = JSON.parse(await readFile(lockPath, "utf8"));
    if (visible.token === owner.token) await unlink(lockPath);
  } catch (error) {
    if (error.code !== "ENOENT" && !(error instanceof SyntaxError)) throw error;
  }
}

function nextJson(child, timeoutMs) {
  return withTimeout(new Promise((resolve, reject) => {
    child.lines.once("line", (line) => resolve(JSON.parse(line)));
    child.once("exit", (code, signal) => reject(new Error(
      `candidate exited before event: code=${code} signal=${signal} stderr=${child.stderrText}`,
    )));
  }), timeoutMs, "candidate event");
}

async function waitForExit(child, timeoutMs) {
  const outcome = child.exitCode !== null || child.signalCode !== null
    ? { code: child.exitCode, signal: child.signalCode }
    : await withTimeout(new Promise((resolve) => {
      child.once("exit", (code, signal) => resolve({ code, signal }));
    }), timeoutMs, "candidate exit");
  children.delete(child);
  return outcome;
}

async function forceStop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  try { process.kill(-child.pid, "SIGKILL"); } catch (error) {
    if (error.code !== "ESRCH") throw error;
  }
}

function withTimeout(promise, timeoutMs, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), timeoutMs)),
  ]);
}

function isRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
