#!/usr/bin/env node
// Captures golden JSON fixtures for spocky-contracts from the pinned Paseo
// protocol package. Read-only against the runtime checkout.
//
// Usage (Node 22.20.0, the binary pinned by the slice harness):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase3/contracts-capture.mjs \
//     --runtime <paseo-runtime> --server-dist <built paseo root> [--check]
//
// --server-dist is a disposable build of the same commit (the slice harness
// builds one with scripts/phase3/build-original.sh). Cases with a `build`
// function get their input from pinned server code there, such as
// toAgentPayload and buildStoredAgentPayload, instead of hand-written JSON.
//
// For every case in contracts-cases.mjs it records the exact input text and
// what the pinned validators produce:
//   inbound  -> WSInboundMessageSchema.safeParse (what the daemon accepts,
//               and error.message for a rejection)
//   outbound -> validateWSOutboundMessage (the client's zod-aot validator)
//               and WSOutboundMessageSchema.safeParse (plain zod)
// Outputs are JSON.stringify of the parsed data, so key order, defaults,
// stripped keys, and transforms are captured exactly.
//
// --check regenerates in memory and exits 1 if the committed fixture differs.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { CASES } from "./contracts-cases.mjs";

const PASEO_COMMIT = "5de45e208690b0efc51c59a585ae9729325a9204";
const NODE_VERSION = "v22.20.0";
// Same binary the p3_slice_harness lane pins in scripts/phase3/pins.sh
// (P3_NODE_BINARY_SHA256): nvm install of node-v22.20.0-darwin-x64.
const NODE_BINARY_SHA256 = "1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const fixturePath = join(repoRoot, "crates/spocky-contracts/tests/fixtures/g1-golden.json");

function fail(message) {
  process.stderr.write(`contracts-capture: ${message}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const args = { runtime: null, serverRoot: null, check: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--runtime") {
      args.runtime = argv[index + 1];
      index += 1;
    } else if (arg === "--server-dist") {
      args.serverRoot = argv[index + 1];
      index += 1;
    } else if (arg === "--check") {
      args.check = true;
    } else {
      fail(`unknown argument ${arg}`);
    }
  }
  if (!args.runtime) fail("--runtime <paseo-runtime checkout> is required");
  if (!args.serverRoot) fail("--server-dist <built paseo root> is required");
  return args;
}

function git(runtime, ...args) {
  return execFileSync("git", ["-C", runtime, ...args], { encoding: "utf8" }).trim();
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function packageVersion(runtime, name) {
  const manifest = join(runtime, "node_modules", name, "package.json");
  return JSON.parse(readFileSync(manifest, "utf8")).version;
}

function outcome(result) {
  if (result.success) {
    return { success: true, output: JSON.stringify(result.data) };
  }
  return {
    success: false,
    issues: result.error.issues.map((issue) => ({
      code: issue.code,
      path: issue.path.map(String),
    })),
  };
}

function caseInput(testCase, pinned) {
  if (typeof testCase.raw === "string") return testCase.raw;
  if (typeof testCase.build === "function") return JSON.stringify(testCase.build(pinned));
  return JSON.stringify(testCase.input);
}

async function loadPinnedServer(serverRoot) {
  const marker = readFileSync(join(serverRoot, ".spocky-build"), "utf8");
  if (!marker.includes(`commit=${PASEO_COMMIT}\n`)) fail(`${serverRoot} is not a build of ${PASEO_COMMIT}`);
  if (!marker.includes(`node=${NODE_BINARY_SHA256}\n`)) fail(`${serverRoot} was not built with the pinned node`);
  const dist = join(serverRoot, "packages/server/dist/server/server");
  const projectionsPath = join(dist, "agent/agent-projections.js");
  const placementPath = join(dist, "workspace-registry-model.js");
  const projections = await import(pathToFileURL(projectionsPath).href);
  const placement = await import(pathToFileURL(placementPath).href);
  return {
    api: {
      toAgentPayload: projections.toAgentPayload,
      buildStoredAgentPayload: projections.buildStoredAgentPayload,
      checkoutFromPersistedWorkspacePlacement: placement.checkoutFromPersistedWorkspacePlacement,
    },
    provenance: {
      buildMarkerSha256: createHash("sha256").update(marker).digest("hex"),
      agentProjectionsJsSha256: sha256(projectionsPath),
      workspaceRegistryModelJsSha256: sha256(placementPath),
    },
  };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (process.version !== NODE_VERSION) {
    fail(`expected node ${NODE_VERSION}, running ${process.version}`);
  }
  const nodeDigest = sha256(process.execPath);
  if (nodeDigest !== NODE_BINARY_SHA256) {
    fail(`node binary ${process.execPath} has SHA-256 ${nodeDigest}, expected ${NODE_BINARY_SHA256}`);
  }
  const runtime = resolve(args.runtime);
  const head = git(runtime, "rev-parse", "HEAD");
  if (head !== PASEO_COMMIT) fail(`runtime HEAD ${head} is not ${PASEO_COMMIT}`);
  const dirty = git(runtime, "status", "--porcelain", "--untracked-files=no");
  if (dirty !== "") fail(`runtime has tracked modifications:\n${dirty}`);

  const dist = join(runtime, "packages/protocol/dist");
  const messagesPath = join(dist, "messages.js");
  const aotPath = join(dist, "validation/ws-outbound.js");
  const aotGeneratedPath = join(dist, "generated/validation/ws-outbound.aot.js");
  const messages = await import(pathToFileURL(messagesPath).href);
  const server = await loadPinnedServer(resolve(args.serverRoot));
  const { validateWSOutboundMessage } = await import(pathToFileURL(aotPath).href);

  const seen = new Set();
  const cases = CASES.map((testCase) => {
    if (seen.has(testCase.id)) fail(`duplicate case id ${testCase.id}`);
    seen.add(testCase.id);
    const input = caseInput(testCase, server.api);
    const record = {
      id: testCase.id,
      direction: testCase.direction,
      source: testCase.source,
      input,
    };
    let value;
    try {
      value = JSON.parse(input);
    } catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
      // The daemon never reaches zod: JSON.parse throws first.
      record.zod = { success: false, syntaxError: true, issues: [] };
      return record;
    }
    if (testCase.direction === "inbound") {
      const result = messages.WSInboundMessageSchema.safeParse(value);
      record.zod = outcome(result);
      // The daemon's rejection text is `Invalid message: ${error.message}`.
      if (!result.success) record.zod.message = result.error.message;
    } else if (testCase.direction === "outbound") {
      record.aot = outcome(validateWSOutboundMessage(value));
      record.zod = outcome(messages.WSOutboundMessageSchema.safeParse(value));
    } else {
      fail(`case ${testCase.id} has unknown direction ${testCase.direction}`);
    }
    return record;
  });

  const fixture = {
    provenance: {
      paseoCommit: PASEO_COMMIT,
      node: process.version,
      nodeBinarySha256: NODE_BINARY_SHA256,
      zod: packageVersion(runtime, "zod"),
      zodAot: packageVersion(runtime, "zod-aot"),
      messagesJsSha256: sha256(messagesPath),
      wsOutboundAotJsSha256: sha256(aotGeneratedPath),
      casesSha256: sha256(join(here, "contracts-cases.mjs")),
      pinnedServer: server.provenance,
    },
    cases,
  };
  const text = `${JSON.stringify(fixture, null, 2)}\n`;

  if (args.check) {
    const committed = readFileSync(fixturePath, "utf8");
    if (committed !== text) {
      process.stderr.write("contracts-capture: fixture is stale; rerun without --check\n");
      process.exit(1);
    }
    process.stdout.write(`contracts-capture: ${cases.length} cases match ${fixturePath}\n`);
    return;
  }
  writeFileSync(fixturePath, text);
  process.stdout.write(`contracts-capture: wrote ${cases.length} cases to ${fixturePath}\n`);
}

await main();
