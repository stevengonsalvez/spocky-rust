import { fileURLToPath, pathToFileURL } from "node:url";

const baselineRoot = process.env.PASEO_REFERENCE_ROOT;
if (!baselineRoot) throw new Error("PASEO_REFERENCE_ROOT is required");

const pluginSource = (path: string) =>
  fileURLToPath(new URL(`packages/plugin/src/${path}`, pathToFileURL(`${baselineRoot}/`)));

export default {
  resolve: {
    alias: [
      {
        find: /^@paseo-settings-source$/,
        replacement: fileURLToPath(
          new URL(
            "packages/server/src/server/plugins/settings/index.ts",
            pathToFileURL(`${baselineRoot}/`),
          ),
        ),
      },
      { find: /^@getpaseo\/plugin$/, replacement: pluginSource("index.ts") },
      { find: /^@getpaseo\/plugin\/server$/, replacement: pluginSource("server/index.ts") },
    ],
  },
};
