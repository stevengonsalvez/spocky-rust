// Regenerates date-parse-vectors.json: `Date.parse` of startedAt-like strings
// under Node 22.20.0, the call at pid-lock.ts:59 (`precedesThisBoot`). In
// `cases` every string names its own zone, so the values do not depend on TZ.
// `localCases` are strings with no zone, which V8 reads in local time, recorded
// under each of `zones`: UTC, which the harness pins (TZ=UTC), and Europe/London,
// where the offset changes and two local times are skipped or repeated. A string
// V8 rejects is recorded as null (NaN).
// Usage: node gen-date-parse-vectors.cjs > date-parse-vectors.json
const crypto = require("node:crypto");
const fs = require("node:fs");

const inputs = [
  "2026-10-01T14:00:00.000Z",
  "2026-10-01T14:00:00Z",
  "2026-10-01T14:00Z",
  "2026-10-01T14:00:00.123456Z",
  "2026-10-01",
  "2026-10",
  "2026",
  "2026-10-01T14:00:00+01:00",
  "2026-10-01T14:00:00-0530",
  "2026-10-01 14:00:00Z",
  "+002026-10-01T14:00:00Z",
  "-000001-01-01T00:00:00Z",
  "Thu, 01 Oct 2026 15:17:04 GMT",
  "Thu Oct 01 2026 14:00:00 GMT+0000 (Coordinated Universal Time)",
  "Oct 1 2026 15:17:04 GMT+0100",
  "1 October 2026 14:00 GMT",
  "October 1, 2026 14:00:00 UTC",
  "2026/10/01 14:00:00 UTC",
  "10/01/2026 14:00:00 GMT",
  "2026-10-01T14:00:00.000z",
  " 2026-10-01T14:00:00.000Z ",
  "",
  "not a date",
  "2026-13-01T00:00:00Z",
  "2026-10-32T00:00:00Z",
  "2026-10-01T25:00:00Z",
  "2026-10-01T00:00:60Z",
  "2026-02-30T00:00:00Z",
  "2026-10-01T14:00:00.Z",
  "2026-10-01T",
  "T14:00:00Z",
];
const zones = ["UTC", "Europe/London"];
const localInputs = [
  "2026-10-01T14:00:00",
  "2026-10-01T14:00",
  "2026-10-01T14:00:00.123",
  "2026-01-15T14:00:00",
  "2026-03-29T00:59:59",
  "2026-03-29T01:30:00",
  "2026-03-29T02:00:00",
  "2026-10-25T00:59:59",
  "2026-10-25T01:30:00",
  "2026-10-25T02:00:00",
  "2026-10-01 14:00:00",
  "Oct 1 2026",
  "Oct 1 2026 15:17:04",
  "Thu Oct 01 2026 14:00:00",
  "1 October 2026 14:00",
  "2026/10/01",
  "2026/10/01 14:00:00",
  "10/01/2026",
  "10/01/2026 14:00:00",
];
const localCases = [];
for (const zone of zones) {
  // Node reads TZ again when it is assigned.
  process.env.TZ = zone;
  for (const text of localInputs) {
    const value = Date.parse(text);
    localCases.push({ zone, text, ms: Number.isNaN(value) ? null : value });
  }
}
process.env.TZ = "UTC";
const out = {
  node: process.version,
  nodeSha256: crypto.createHash("sha256").update(fs.readFileSync(process.execPath)).digest("hex"),
  cases: inputs.map((text) => {
    const value = Date.parse(text);
    return { text, ms: Number.isNaN(value) ? null : value };
  }),
  localCases,
};
process.stdout.write(JSON.stringify(out, null, 1));
