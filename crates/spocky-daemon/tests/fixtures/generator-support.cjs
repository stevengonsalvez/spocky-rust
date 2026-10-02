// Shared by the gen-original-*.cjs and .mjs generators in this directory.
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const sha256 = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");

// What a fixture was captured from: the Node binary, the build marker
// scripts/phase3/build-original.sh writes into the build root (commit, lock
// and Node digests), and the digests of the dist files the capture ran.
exports.provenance = function provenance(root, distFiles = []) {
  const files = [
    "packages/cli/dist/index.js",
    "packages/server/dist/server/server/bootstrap.js",
    "packages/server/dist/server/server/websocket-server.js",
    ...distFiles,
  ];
  return {
    node: process.version,
    nodeSha256: sha256(process.execPath),
    buildMarker: fs.readFileSync(path.join(root, ".spocky-build"), "utf8"),
    dist: Object.fromEntries([...new Set(files)].map((file) => [file, sha256(path.join(root, file))])),
  };
};

const ps = (field, pid) => {
  try {
    return execFileSync("/bin/ps", ["-o", `${field}=`, "-p", String(pid)], { encoding: "utf8" }).trim();
  } catch {
    return null;
  }
};

// A PID read from a file the daemon wrote may belong to an unrelated process
// by the time it is signalled. `claimPid` accepts it only while the process is
// `ancestor` or one of its descendants, and records its start time; `claimHolds`
// is true only for that same process, so a reused PID is never signalled.
// Claim while the process this script started is still alive.
exports.claimPid = function claimPid(pid, ancestor) {
  let current = pid;
  for (let depth = 0; depth < 32 && current > 1; depth++) {
    if (current === ancestor) {
      const started = ps("lstart", pid);
      return started ? { pid, started } : null;
    }
    const parent = Number(ps("ppid", current));
    if (!Number.isInteger(parent) || parent <= 0) return null;
    current = parent;
  }
  return null;
};

exports.claimHolds = function claimHolds(claim) {
  return claim !== null && ps("lstart", claim.pid) === claim.started;
};
