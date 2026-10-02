// Imports a patched COPY of a module from the pinned Paseo build, so a test
// can reach functions the build does not export or observe an internal value,
// without touching the pinned tree.
//
// The copy lands in a scratch directory with:
//   * every relative import rewritten to an absolute file URL into the
//     pinned tree (or to the URL in `rewrites`, keyed by the specifier);
//   * every bare import ("zod", "@getpaseo/...") resolvable, by linking the
//     package into a node_modules beside the copy, found by walking up from
//     the pinned file through every ancestor's node_modules as Node does;
//   * `transform(source)` applied last (to add an export or a capture).
import { existsSync, mkdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

function findPackage(pinnedFile, name) {
  for (let dir = dirname(pinnedFile); ; dir = dirname(dir)) {
    const candidate = join(dir, "node_modules", name);
    if (existsSync(candidate)) return candidate;
    if (dirname(dir) === dir) throw new Error(`cannot find ${name} above the pinned build`);
  }
}

export function patchedCopyUrl({ pinnedFile, scratch, name, rewrites = {}, transform = (s) => s }) {
  const pinnedUrl = pathToFileURL(pinnedFile);
  let source = readFileSync(pinnedFile, "utf8");
  source = source.replace(/from "(\.{1,2}\/[^"]+)"/g, (_match, relative) => {
    return `from "${rewrites[relative] ?? new URL(relative, pinnedUrl).href}"`;
  });
  source = transform(source);

  const packages = new Set(
    [...source.matchAll(/from "([^./][^"]*)"/g)]
      .filter(([, specifier]) => !/^(file|node):/.test(specifier))
      .map(([, specifier]) =>
        specifier.startsWith("@")
          ? specifier.split("/").slice(0, 2).join("/")
          : specifier.split("/")[0],
      ),
  );
  mkdirSync(scratch, { recursive: true });
  for (const packageName of packages) {
    const link = join(scratch, "node_modules", packageName);
    if (existsSync(link)) continue;
    mkdirSync(dirname(link), { recursive: true });
    symlinkSync(findPackage(pinnedFile, packageName), link);
  }
  const copy = join(scratch, `${name}.mjs`);
  writeFileSync(copy, source);
  return pathToFileURL(copy).href;
}
