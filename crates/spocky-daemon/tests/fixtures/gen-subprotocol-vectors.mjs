// Regenerates subprotocol-vectors.json from the pinned ws library.
// Usage: node gen-subprotocol-vectors.mjs <paseo-runtime-root> > subprotocol-vectors.json
import { createRequire } from "node:module";
import path from "node:path";

const root = process.argv[2];
const require = createRequire(path.join(root, "package.json"));
const { parse } = require(path.join(root, "node_modules/ws/lib/subprotocol.js"));
const alphabet = ["a", "b", "A", "9", "-", ".", ",", ",", " ", "\t", ";", "/", "é", "_", "!", "\u007f", "paseo.bearer.x"];
let seed = 12345;
const random = () => {
  seed = (seed * 1103515245 + 12345) & 0x7fffffff;
  return seed / 0x7fffffff;
};
const rows = [];
const seen = new Set();
for (let i = 0; i < 20000; i++) {
  const length = Math.floor(random() * 8);
  let header = "";
  for (let j = 0; j < length; j++) header += alphabet[Math.floor(random() * alphabet.length)];
  if (seen.has(header)) continue;
  seen.add(header);
  try {
    rows.push([header, { ok: [...parse(header)] }]);
  } catch (error) {
    rows.push([header, { err: error.message }]);
  }
}
process.stdout.write(JSON.stringify(rows));
