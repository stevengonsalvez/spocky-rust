import path from "node:path";

const root = process.env.PASEO_REFERENCE_ROOT;
if (!root) throw new Error("PASEO_REFERENCE_ROOT is required");

const pluginSource = (entry: string) => path.join(root, "packages/plugin/src", entry);
const stub = path.join(import.meta.dirname, "plugin-hook-client-stub.ts");

export default {
  resolve: {
    alias: [
      { find: /^@getpaseo\/client$/, replacement: stub },
      { find: /^@getpaseo\/client\/internal\/daemon-client$/, replacement: stub },
      { find: /^@getpaseo\/plugin$/, replacement: pluginSource("index.ts") },
      { find: /^@getpaseo\/plugin\/server$/, replacement: pluginSource("server/index.ts") },
      {
        find: /^@getpaseo\/plugin\/server\/provider$/,
        replacement: pluginSource("server/provider.ts"),
      },
      {
        find: /^@getpaseo\/plugin\/server\/usage$/,
        replacement: pluginSource("server/usage.ts"),
      },
      {
        find: /^@getpaseo\/plugin\/server\/acp$/,
        replacement: pluginSource("server/acp.ts"),
      },
    ],
  },
};
