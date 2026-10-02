// Runs a corpus of Codex thread items through the pinned Paseo
// `mapFileChangeItem` (via `CodexThreadItemSchema` and
// `mapThreadItemToNormalizedEnvelope`, the internal path
// `mapCodexToolCallFromThreadItem` takes) and prints one JSON line per case:
// the envelope, or null when the schema rejects the item.
//
// The functions are not exported by the pinned build, so a patched COPY of
// `codex/tool-call-mapper.js` is written to a scratch directory, with its
// relative imports pointing back into the pinned tree and one added `export`.
// The pinned tree itself is not touched.
//
// argv: <pinned codex/tool-call-mapper.js> <scratch dir> <corpus json>
import { existsSync, mkdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import process from "node:process";

const [pinnedFile, scratch, corpusPath] = process.argv.slice(2);
const pinnedUrl = pathToFileURL(pinnedFile);

let source = readFileSync(pinnedFile, "utf8");
source = source.replace(/from "(\.{1,2}\/[^"]+)"/g, (_match, relative) => {
  return `from "${new URL(relative, pinnedUrl).href}"`;
});
source += "\nexport { CodexThreadItemSchema, mapThreadItemToNormalizedEnvelope };\n";

// Bare imports ("zod", "@getpaseo/...") must resolve from the copy: link each
// package into a node_modules beside it, found by walking up from the pinned
// file through every ancestor's node_modules, as Node's own lookup does.
function findPackage(name) {
  for (let dir = dirname(pinnedFile); ; dir = dirname(dir)) {
    const candidate = join(dir, "node_modules", name);
    if (existsSync(candidate)) return candidate;
    if (dirname(dir) === dir) throw new Error(`cannot find ${name} above the pinned build`);
  }
}
const packages = new Set(
  [...source.matchAll(/from "([^./][^"]*)"/g)]
    .filter(([, specifier]) => !/^(file|node):/.test(specifier))
    .map(([, specifier]) =>
    specifier.startsWith("@") ? specifier.split("/").slice(0, 2).join("/") : specifier.split("/")[0],
  ),
);
mkdirSync(scratch, { recursive: true });
for (const name of packages) {
  const link = join(scratch, "node_modules", name);
  if (existsSync(link)) continue;
  mkdirSync(dirname(link), { recursive: true });
  symlinkSync(findPackage(name), link);
}
const copy = join(scratch, "tool-call-mapper.mjs");
writeFileSync(copy, source);

const { CodexThreadItemSchema, mapThreadItemToNormalizedEnvelope } = await import(
  pathToFileURL(copy).href
);

const corpus = JSON.parse(readFileSync(corpusPath, "utf8")).cases;
const lines = [];
for (const { item, cwd } of corpus) {
  const parsed = CodexThreadItemSchema.safeParse(item);
  let line = "null";
  if (parsed.success) {
    const options = cwd === undefined ? undefined : { cwd };
    const envelope = mapThreadItemToNormalizedEnvelope(parsed.data, options);
    line = JSON.stringify(envelope ?? null);
  }
  lines.push(line);
}
// A pipe is written asynchronously: exit only once everything is flushed.
process.stdout.write(`${lines.join("\n")}\n`, () => process.exit(0));
