import { fileURLToPath } from "node:url";

const pluginSource = (path: string) =>
  fileURLToPath(
    new URL(`../../.baselines/paseo-runtime/packages/plugin/src/${path}`, import.meta.url),
  );

export default {
  resolve: {
    alias: [
      { find: /^@getpaseo\/plugin$/, replacement: pluginSource("index.ts") },
      { find: /^@getpaseo\/plugin\/server$/, replacement: pluginSource("server/index.ts") },
    ],
  },
};
