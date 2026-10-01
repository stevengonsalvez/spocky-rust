import { readdir, readFile, stat } from "node:fs/promises";
import { join } from "node:path";

const packageRoot = process.argv[2];
if (!packageRoot) throw new Error("PGlite package root is required");

const distributionRoot = join(packageRoot, "dist");
const packageJson = JSON.parse(await readFile(join(packageRoot, "package.json"), "utf8"));

async function wasmObservation(name) {
  const bytes = await readFile(join(distributionRoot, name));
  const module = await WebAssembly.compile(bytes);
  const imports = WebAssembly.Module.imports(module);
  const byModule = Object.fromEntries(
    [...new Set(imports.map((entry) => entry.module))]
      .sort()
      .map((moduleName) => [
        moduleName,
        imports.filter((entry) => entry.module === moduleName).length,
      ]),
  );
  return { bytes: bytes.length, imports: { total: imports.length, byModule } };
}

async function fileBytes(name) {
  return (await stat(join(distributionRoot, name))).size;
}

async function packageObservation(directory) {
  let bytes = 0;
  let fileCount = 0;
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      const child = await packageObservation(path);
      bytes += child.bytes;
      fileCount += child.fileCount;
    } else if (entry.isFile()) {
      bytes += (await stat(path)).size;
      fileCount += 1;
    }
  }
  return { bytes, fileCount };
}

const packageFiles = await packageObservation(packageRoot);

process.stdout.write(
  `${JSON.stringify({
    package: packageJson.name,
    version: packageJson.version,
    packageBytes: packageFiles.bytes,
    packageFileCount: packageFiles.fileCount,
    exports: packageJson.exports,
    mainWasm: await wasmObservation("pglite.wasm"),
    initdbWasm: await wasmObservation("initdb.wasm"),
    glue: {
      moduleBytes: await fileBytes("pglite.js"),
      dataBytes: await fileBytes("pglite.data"),
    },
  })}\n`,
);
