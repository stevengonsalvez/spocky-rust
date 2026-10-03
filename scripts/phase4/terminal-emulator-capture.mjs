#!/usr/bin/env node
// Captures pinned Paseo terminal emulator output for every scenario in
// terminal-emulator-corpus.json by running the pinned
// packages/server/dist/server/terminal/terminal.js, so the CSI and OSC
// handlers, the cell, scrollback, wrap, cursor and last-output-line
// extraction, the input-mode replies and the exit info are the pinned code.
// Read-only against the pinned build.
//
// Usage (Node 22.20.0):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase4/terminal-emulator-capture.mjs \
//     --paseo-root <built paseo checkout at 5de45e2> --out <capture.json>
//
// terminal.js is loaded with two loader substitutions (see
// terminal-emulator-hooks.mjs): node-pty becomes a PTY with no process behind
// it, which this script feeds with the scenario bytes decoded with a utf8
// StringDecoder (what node-pty's socket.setEncoding("utf8") does before
// onData), and @xterm/headless is the pinned package with its Terminal
// remembered so the raw title events can be read. Everything a scenario
// reports comes from terminal.js: getState({ includeWrapFlags: true }), the
// exit info's lastOutputLines, onCommandFinished, and the bytes it writes to
// the PTY (handler replies and input-mode replies).

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire, register } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";
import { fileURLToPath, pathToFileURL } from "node:url";

const PASEO_COMMIT = "5de45e208690b0efc51c59a585ae9729325a9204";

const here = dirname(fileURLToPath(import.meta.url));

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined) {
      throw new Error(`bad argument: ${key}`);
    }
    args[key.slice(2)] = value;
  }
  if (!args["paseo-root"] || !args.out) {
    throw new Error("usage: --paseo-root <dir> --out <file>");
  }
  return args;
}

function chunksOf(op) {
  if (op.text !== undefined) {
    return Buffer.from(op.text, "utf8");
  }
  if (op.hex !== undefined) {
    return Buffer.from(op.hex, "hex");
  }
  if (op.repeat !== undefined) {
    let text = "";
    for (let index = 0; index < op.repeat.count; index++) {
      text += op.repeat.text.replaceAll("%d", String(index));
    }
    return Buffer.from(text, "utf8");
  }
  return null;
}

function flushed(terminal) {
  return new Promise((resolveWrite) => terminal.write("", resolveWrite));
}

async function runScenario(createTerminal, scenario) {
  const session = await createTerminal({
    cwd: tmpdir(),
    workspaceId: "capture",
    rows: scenario.rows,
    cols: scenario.cols,
  });
  const pty = globalThis.__spockyFakePtys.at(-1);
  const terminal = globalThis.__spockyTerminals.at(-1);
  const titles = [];
  const commandFinished = [];
  let exitInfo = null;
  terminal.onTitleChange((title) => titles.push(title));
  session.onCommandFinished((info) => commandFinished.push(info));
  session.onExit((info) => {
    exitInfo = info;
  });

  const decoder = new StringDecoder("utf8");
  for (const op of scenario.ops) {
    if (op.resize !== undefined) {
      session.send({ type: "resize", cols: op.resize[0], rows: op.resize[1] });
      continue;
    }
    const bytes = chunksOf(op);
    if (bytes === null) {
      throw new Error(`unknown op in ${scenario.name}`);
    }
    const text = decoder.write(bytes);
    if (text.length > 0) {
      pty.emitData(text);
      await flushed(terminal);
    }
  }
  await flushed(terminal);

  const state = session.getState({ includeWrapFlags: true });
  if (state.title !== undefined) {
    // The session title follows the emulator's after a debounce; a state read
    // that already carries it would differ from the corpus expectation.
    throw new Error(`scenario ${scenario.name}: state read after the title debounce`);
  }
  pty.emitExit({ exitCode: 0, signal: 0 });
  const result = {
    name: scenario.name,
    state,
    lastOutputLines: exitInfo.lastOutputLines,
    titles,
    responses: pty.written,
    commandFinished,
  };
  return result;
}

const args = parseArgs(process.argv.slice(2));
const paseoRoot = resolve(args["paseo-root"]);
process.env.SPOCKY_CAPTURE_PASEO_ROOT = paseoRoot;
globalThis.__spockyFakePtys = [];
globalThis.__spockyTerminals = [];
register(pathToFileURL(join(here, "terminal-emulator-hooks.mjs")), {
  data: {
    fakePty: pathToFileURL(join(here, "terminal-emulator-fake-pty.mjs")).href,
    recorder: pathToFileURL(join(here, "terminal-emulator-xterm-recorder.mjs")).href,
  },
});
const terminalModule = join(paseoRoot, "packages/server/dist/server/terminal/terminal.js");
const { createTerminal } = await import(pathToFileURL(terminalModule).href);
const serverRequire = createRequire(join(paseoRoot, "packages/server/package.json"));
const xtermVersion = JSON.parse(
  readFileSync(serverRequire.resolve("@xterm/headless/package.json"), "utf8"),
).version;

const corpusText = readFileSync(join(here, "terminal-emulator-corpus.json"), "utf8");
const corpus = JSON.parse(corpusText);
const scenarios = [];
for (const scenario of corpus.scenarios) {
  scenarios.push(await runScenario(createTerminal, scenario));
}

const capture = {
  paseoCommit: PASEO_COMMIT,
  node: process.version,
  xtermHeadless: xtermVersion,
  terminalJsSha256: createHash("sha256").update(readFileSync(terminalModule)).digest("hex"),
  corpusSha256: createHash("sha256").update(corpusText).digest("hex"),
  scenarios,
};
writeFileSync(args.out, `${JSON.stringify(capture)}\n`);
// The pinned session keeps timers alive; the capture is finished.
process.exit(0);
