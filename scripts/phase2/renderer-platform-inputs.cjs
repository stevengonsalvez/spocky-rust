// Records what produced a renderer gate run: commit, CI run, OS, tool versions, the
// CEF archive and host binary hashes, the Dioxus bundle file list, and the shipped
// app's asar and exported web bundle. Hashes only; nothing is filtered or masked.
//
// env: SPOCKY_INPUTS_OUT (output json), SPOCKY_BUNDLE (dir), SPOCKY_CEF_HOST (file),
//      SPOCKY_APP_ASAR (file, optional), SPOCKY_WEB_EXPORT (dir, optional),
//      SPOCKY_HOSTB (dir with electron and playwright-core, optional),
//      SPOCKY_REF (reference checkout, optional), plus any of CEF_ARCHIVE, CEF_SHA256,
//      REFERENCE_COMMIT, IMAGE_ID, BUNDLE_SHA256, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT,
//      GITHUB_SHA, GITHUB_WORKFLOW, ImageVersion, RUNNER_OS, RUNNER_ARCH.
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const sha256File = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
function listFiles(dir, prefix = "") {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const rel = path.posix.join(prefix, entry.name);
    return entry.isDirectory() ? listFiles(path.join(dir, entry.name), rel) : [rel];
  });
}
// Same digest as `find . -type f | LC_ALL=C sort | xargs sha256sum | sha256sum`.
function treeDigest(dir) {
  const files = listFiles(dir).map((rel) => `./${rel}`).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
  const lines = files.map((rel) => `${sha256File(path.join(dir, rel))}  ${rel}`);
  return {
    fileCount: files.length,
    digest: crypto.createHash("sha256").update(lines.map((l) => `${l}\n`).join("")).digest("hex"),
    files: Object.fromEntries(files.map((rel, i) => [rel, lines[i].slice(0, 64)])),
  };
}
const optional = (name) => (process.env[name] && fs.existsSync(process.env[name]) ? process.env[name] : null);
const run = (command, args) => {
  const r = spawnSync(command, args, { encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : null;
};
const readVersion = (dir, pkg) => {
  try {
    return JSON.parse(fs.readFileSync(path.join(dir, "node_modules", pkg, "package.json"), "utf8")).version;
  } catch {
    return null;
  }
};

const out = process.env.SPOCKY_INPUTS_OUT;
if (!out) throw new Error("SPOCKY_INPUTS_OUT is required");
const bundle = optional("SPOCKY_BUNDLE");
const cefHost = optional("SPOCKY_CEF_HOST");
const asar = optional("SPOCKY_APP_ASAR");
const webExport = optional("SPOCKY_WEB_EXPORT");
const hostB = optional("SPOCKY_HOSTB");
const env = process.env;
const bundleTree = bundle ? treeDigest(bundle) : null;
const record = {
  commit: env.GITHUB_SHA ?? run("git", ["rev-parse", "HEAD"]),
  ci: env.GITHUB_RUN_ID
    ? { runId: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT ?? null, workflow: env.GITHUB_WORKFLOW ?? null,
        runner: { os: env.RUNNER_OS ?? null, arch: env.RUNNER_ARCH ?? null, image: env.ImageVersion ?? null } }
    : null,
  os: { uname: run("uname", ["-a"]) ?? process.platform, sw_vers: run("sw_vers", []) },
  node: process.version,
  electron: hostB ? readVersion(hostB, "electron") : null,
  playwrightCore: hostB ? readVersion(hostB, "playwright-core") : null,
  referenceCommit: env.REFERENCE_COMMIT ?? null,
  gateImageId: env.IMAGE_ID ?? null,
  cef: { archive: env.CEF_ARCHIVE ?? null, archiveSha256: env.CEF_SHA256 ?? null, hostBinarySha256: cefHost ? sha256File(cefHost) : null },
  dioxusBundle: bundleTree && { expectedDigest: env.BUNDLE_SHA256 ?? null, matchesExpected: env.BUNDLE_SHA256 ? env.BUNDLE_SHA256 === bundleTree.digest : null, ...bundleTree },
  shippedApp: { appAsarSha256: asar ? sha256File(asar) : null, webExport: webExport ? (({ files, ...rest }) => rest)(treeDigest(webExport)) : null },
};
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, `${JSON.stringify(record, null, 2)}\n`);
console.log(`inputs recorded: ${out}`);
