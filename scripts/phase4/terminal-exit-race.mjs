// Measures whether pinned Paseo delivers a terminal's last output to a
// subscriber when the shell exits right after printing it.
//
// Usage (Node 22.20.0, pinned build):
//   env -i PATH=/usr/bin:/bin HOME=/tmp node scripts/phase4/terminal-exit-race.mjs \
//     <dist/server/terminal> <trials> <sh -c script>
//
// Each trial runs `createTerminal` with `/bin/sh -c <script>`, subscribes, waits for
// the exit event, then reports whether the marker `END` reached the
// subscriber's output messages and whether it is in the exit info lines.
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
const [terminalDir, trialsText, script] = process.argv.slice(2);
const { createTerminal } = await import(`${terminalDir}/terminal.js`);
const cwd = mkdtempSync(join(tmpdir(), "race-"));
const trials = Number(trialsText);
const tally = { delivered: 0, dropped: 0, exitLinesHave: 0, exitLinesMiss: 0 };
for (let i = 0; i < trials; i++) {
  const session = await createTerminal({ cwd, workspaceId: "ws", command: "/bin/sh", args: ["-c", script], rows: 5, cols: 30 });
  let out = "";
  session.subscribe((m) => { if (m.type === "output") out += m.data; }, { initialSnapshot: "state" });
  const info = await new Promise((resolve) => session.onExit(resolve));
  await new Promise((r) => setTimeout(r, 150));
  if (out.includes("END")) tally.delivered++; else tally.dropped++;
  if (info.lastOutputLines.join("\n").includes("END")) tally.exitLinesHave++; else tally.exitLinesMiss++;
}
process.stdout.write(JSON.stringify(tally) + "\n", () => process.exit(0));
