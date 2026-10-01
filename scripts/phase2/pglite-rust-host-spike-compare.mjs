// Compares the Node and Rust spike reports. Wall-clock mtimes are compared
// only by class: the tar handoff writes seconds where milliseconds are
// expected, so those files land in January 1970; every other mtime is a
// real wall-clock value. Nothing else is normalized.
import { readFileSync } from "node:fs";

const [nodePath, rustPath] = process.argv.slice(2);
const node = JSON.parse(readFileSync(nodePath, "utf8"));
const rust = JSON.parse(readFileSync(rustPath, "utf8"));

const EPOCH_QUIRK_LIMIT_MS = 1e10;
const mtimeClass = (entry) => (entry.mtimeMs < EPOCH_QUIRK_LIMIT_MS ? "tar-seconds-as-ms" : "wall-clock");
const index = (tree) => new Map((tree ?? []).map((entry) => [entry.path, entry]));
const nodeTree = index(node.tree);
const rustTree = index(rust.tree);

const onlyNode = [...nodeTree.keys()].filter((path) => !rustTree.has(path));
const onlyRust = [...rustTree.keys()].filter((path) => !nodeTree.has(path));
const differences = { type: [], size: [], mode: [], mtimeClass: [], content: [] };
let sameContent = 0;
for (const [path, left] of nodeTree) {
  const right = rustTree.get(path);
  if (!right) continue;
  if (left.type !== right.type) differences.type.push({ path, node: left.type, rust: right.type });
  if (left.size !== right.size) differences.size.push({ path, node: left.size, rust: right.size });
  if (left.mode !== right.mode) differences.mode.push({ path, node: left.mode, rust: right.mode });
  if (mtimeClass(left) !== mtimeClass(right)) {
    differences.mtimeClass.push({ path, node: mtimeClass(left), rust: mtimeClass(right) });
  }
  if (left.type === "file") {
    if (left.sha256 === right.sha256) sameContent += 1;
    else differences.content.push(path);
  }
}

const step = (report, name) => (report.steps ?? []).find((entry) => entry.step === name) ?? null;
const comparison = {
  tree: {
    nodeEntries: nodeTree.size,
    rustEntries: rustTree.size,
    onlyNode,
    onlyRust,
    filesWithSameBytes: sameContent,
    differences,
  },
  steps: Object.fromEntries(
    ["select1", "plpgsqlException", "structuredError", "settings"].map((name) => [
      name,
      { node: step(node, name), rust: step(rust, name) },
    ]),
  ),
  nodeCounters: node.counters,
  rustCounters: rust.counters,
  rustOpen: step(rust, "open"),
  rustCompile: step(rust, "compile"),
  rustReopen: rust.reopen ?? null,
};
process.stdout.write(`${JSON.stringify(comparison, null, 2)}\n`);
