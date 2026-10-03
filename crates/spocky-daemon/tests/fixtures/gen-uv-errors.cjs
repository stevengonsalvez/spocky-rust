// Regenerates uv-errors.json: libuv's error table as Node 22.20.0 reports it,
// `util.getSystemErrorMap()`: errno, name and the description that
// `UVExceptionWithHostPort` puts into messages such as
// "listen EACCES: permission denied 127.0.0.1:80". Platform specific: the
// errno numbers are the ones of the platform that ran it (darwin for the pin).
// Usage: node gen-uv-errors.cjs > uv-errors.json
const crypto = require("node:crypto");
const fs = require("node:fs");
const util = require("node:util");

const errors = [...util.getSystemErrorMap().entries()]
  .map(([errno, [name, message]]) => [errno, name, message])
  .sort((a, b) => b[0] - a[0]);
process.stdout.write(
  JSON.stringify(
    {
      node: process.version,
      nodeSha256: crypto.createHash("sha256").update(fs.readFileSync(process.execPath)).digest("hex"),
      platform: process.platform,
      errors,
    },
    null,
    1,
  ),
);
