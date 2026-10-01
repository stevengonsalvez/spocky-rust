#!/usr/bin/env node
// Captures pinned Paseo terminal emulator output for every scenario of a
// corpus, one JSON.stringify line per scenario. Read-only against the pinned
// build. Derived from scripts/phase4/terminal-emulator-capture.mjs (branch
// p4-terminal); the extraction and handlers are unchanged.
//
// Usage (Node 22.20.0):
//   ~/.nvm/versions/node/v22.20.0/bin/node scripts/phase4/xterm-capture.mjs \
//     --paseo-root <built paseo checkout at 5de45e2> --corpus <corpus.json> \
//     --out <capture.jsonl>
//
// The emulator is the @xterm/headless package that packages/server resolves,
// constructed with the options createTerminal uses (scrollback 1000,
// allowProposedApi). PTY bytes are decoded with a utf8 StringDecoder, which is
// what node-pty's socket.setEncoding("utf8") does before onData. The custom
// CSI and OSC handlers and the cell, scrollback, wrap, cursor, and last output
// line extraction are copied from packages/server/src/terminal/terminal.ts at
// 5de45e2, so the captured state is what getState({ includeWrapFlags: true })
// returns. Handler responses are the bytes createTerminal writes to the PTY.
//
// Exceptions: when xterm throws while parsing a write, the exception escapes
// its write timer and Paseo's terminal worker keeps running (its
// uncaughtException handler logs and continues). xterm's write queue then
// never parses again and write callbacks never run. This script keeps that
// behaviour: it records "wedged": true, keeps calling terminal.write without
// waiting, and still captures the state. A resize that throws is counted in
// "resizeErrors". Both keys appear only when it happened, so scenarios without
// exceptions match the original capture script byte for byte.
//
// Output: line 1 is a header object; each further line is one scenario.

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";

const PASEO_COMMIT = "5de45e208690b0efc51c59a585ae9729325a9204";
const TERMINAL_EXIT_OUTPUT_LINE_LIMIT = 12;
const TERMINAL_OSC_COLOR_QUERY_RESPONSES = new Map([
  [10, "rgb:e6e6/e6e6/e6e6"],
  [11, "rgb:0b0b/0b0b/0b0b"],
  [12, "rgb:e6e6/e6e6/e6e6"],
]);

// xterm logs every parse error with console.error; logging changes no state.
console.error = () => {};

let onUncaught = null;
process.on("uncaughtException", (error) => {
  if (!onUncaught) {
    throw error;
  }
  onUncaught(error);
});

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
  if (!args["paseo-root"] || !args.corpus || !args.out) {
    throw new Error("usage: --paseo-root <dir> --corpus <file> --out <file>");
  }
  return args;
}

function cellOf(cell) {
  const fgMode = cell.getFgColorMode() >> 24;
  const bgMode = cell.getBgColorMode() >> 24;
  return {
    char: cell.getChars() || " ",
    fg: fgMode !== 0 ? cell.getFgColor() : undefined,
    bg: bgMode !== 0 ? cell.getBgColor() : undefined,
    fgMode: fgMode !== 0 ? fgMode : undefined,
    bgMode: bgMode !== 0 ? bgMode : undefined,
    bold: cell.isBold() !== 0,
    italic: cell.isItalic() !== 0,
    underline: cell.isUnderline() !== 0,
    dim: cell.isDim() !== 0,
    inverse: cell.isInverse() !== 0,
    strikethrough: cell.isStrikethrough() !== 0,
  };
}

function extractRow(terminal, row) {
  const cells = [];
  const line = terminal.buffer.active.getLine(row);
  for (let col = 0; col < terminal.cols; col++) {
    const cell = line?.getCell(col);
    cells.push(cell ? cellOf(cell) : { char: " ", fg: undefined, bg: undefined });
  }
  return cells;
}

function continuesToNext(terminal, row) {
  return terminal.buffer.active.getLine(row + 1)?.isWrapped === true;
}

function extractCursor(terminal) {
  const coreService = terminal._core?.coreService;
  const cursorStyle = coreService?.decPrivateModes?.cursorStyle;
  const style =
    cursorStyle === "block" || cursorStyle === "underline" || cursorStyle === "bar"
      ? cursorStyle
      : undefined;
  const blink =
    typeof coreService?.decPrivateModes?.cursorBlink === "boolean"
      ? coreService.decPrivateModes.cursorBlink
      : undefined;
  const hidden = Boolean(coreService?.isCursorHidden);
  return {
    row: terminal.buffer.active.cursorY,
    col: terminal.buffer.active.cursorX,
    ...(hidden ? { hidden: true } : {}),
    ...(style ? { style } : {}),
    ...(typeof blink === "boolean" ? { blink } : {}),
  };
}

function extractState(terminal) {
  const baseY = terminal.buffer.active.baseY;
  const grid = [];
  const gridWrapped = [];
  for (let row = 0; row < terminal.rows; row++) {
    grid.push(extractRow(terminal, baseY + row));
    gridWrapped.push(continuesToNext(terminal, baseY + row));
  }
  const scrollback = [];
  const scrollbackWrapped = [];
  for (let row = 0; row < baseY; row++) {
    scrollback.push(extractRow(terminal, row));
    scrollbackWrapped.push(continuesToNext(terminal, row));
  }
  return {
    rows: terminal.rows,
    cols: terminal.cols,
    grid,
    scrollback,
    cursor: extractCursor(terminal),
    gridWrapped,
    scrollbackWrapped,
  };
}

function extractLastOutputLines(terminal, limit) {
  const buffer = terminal.buffer.active;
  const merged = [];
  for (let row = 0; row < buffer.length; row++) {
    const line = buffer.getLine(row);
    if (!line) {
      continue;
    }
    const text = line.translateToString(true);
    if (line.isWrapped === true && merged.length > 0) {
      merged[merged.length - 1] += text;
      continue;
    }
    merged.push(text);
  }
  while (merged.length > 0 && merged[0]?.trim().length === 0) {
    merged.shift();
  }
  while (merged.length > 0 && merged[merged.length - 1]?.trim().length === 0) {
    merged.pop();
  }
  return merged.slice(-limit);
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

// Resolves "ok" when the write callback runs, or "threw" when xterm throws
// while parsing it.
function writeAsync(terminal, data) {
  return new Promise((resolveWrite) => {
    onUncaught = () => resolveWrite("threw");
    terminal.write(data, () => resolveWrite("ok"));
  });
}

async function runScenario(Terminal, scenario) {
  const terminal = new Terminal({
    rows: scenario.rows,
    cols: scenario.cols,
    scrollback: 1000,
    allowProposedApi: true,
  });
  const responses = [];
  const titles = [];
  const commandFinished = [];
  const write = (data) => responses.push(data);

  terminal.parser.registerCsiHandler({ final: "c" }, (params) => {
    if (params.length === 0 || (params.length === 1 && params[0] === 0)) {
      write("\x1b[?62;4;22c");
      return true;
    }
    return false;
  });
  terminal.parser.registerCsiHandler({ final: "n" }, (params) => {
    if (params.length !== 1) {
      return false;
    }
    if (params[0] === 5) {
      write("\x1b[0n");
      return true;
    }
    if (params[0] === 6) {
      const buffer = terminal.buffer.active;
      write(`\x1b[${buffer.cursorY + 1};${buffer.cursorX + 1}R`);
      return true;
    }
    return false;
  });
  terminal.parser.registerCsiHandler({ prefix: "?", final: "n" }, (params) => {
    if (params.length !== 1 || params[0] !== 6) {
      return false;
    }
    const buffer = terminal.buffer.active;
    write(`\x1b[?${buffer.cursorY + 1};${buffer.cursorX + 1}R`);
    return true;
  });
  for (const [code, response] of TERMINAL_OSC_COLOR_QUERY_RESPONSES) {
    terminal.parser.registerOscHandler(code, (data) => {
      if (data.trim() !== "?") {
        return false;
      }
      write(`\x1b]${code};${response}\x1b\\`);
      return true;
    });
  }
  terminal.onTitleChange((title) => titles.push(title));
  terminal.parser.registerOscHandler(633, (data) => {
    const parts = data.split(";");
    if (parts[0] === "D" && parts.length === 1) {
      commandFinished.push({ exitCode: null });
    } else if (parts[0] === "D" && parts.length === 2 && /^-?\d+$/.test(parts[1])) {
      commandFinished.push({ exitCode: Number(parts[1]) });
    }
    return true;
  });

  let wedged = false;
  let resizeErrors = 0;
  const decoder = new StringDecoder("utf8");
  for (const op of scenario.ops) {
    if (op.resize !== undefined) {
      try {
        terminal.resize(op.resize[0], op.resize[1]);
      } catch {
        resizeErrors++;
      }
      continue;
    }
    const bytes = chunksOf(op);
    if (bytes === null) {
      throw new Error(`unknown op in ${scenario.name}`);
    }
    const text = decoder.write(bytes);
    if (text.length > 0) {
      if (wedged) {
        terminal.write(text);
      } else if ((await writeAsync(terminal, text)) === "threw") {
        wedged = true;
      }
    }
  }
  if (!wedged && (await writeAsync(terminal, "")) === "threw") {
    wedged = true;
  }
  onUncaught = null;

  const result = {
    name: scenario.name,
    state: extractState(terminal),
    lastOutputLines: extractLastOutputLines(terminal, TERMINAL_EXIT_OUTPUT_LINE_LIMIT),
    titles,
    responses,
    commandFinished,
    ...(wedged ? { wedged: true } : {}),
    ...(resizeErrors > 0 ? { resizeErrors } : {}),
  };
  terminal.dispose();
  return result;
}

const args = parseArgs(process.argv.slice(2));
const paseoRoot = resolve(args["paseo-root"]);
const serverRequire = createRequire(join(paseoRoot, "packages/server/package.json"));
const xtermPackagePath = serverRequire.resolve("@xterm/headless/package.json");
const xtermVersion = JSON.parse(readFileSync(xtermPackagePath, "utf8")).version;
const { Terminal } = serverRequire("@xterm/headless");

const corpusText = readFileSync(args.corpus, "utf8");
const corpus = JSON.parse(corpusText);
const lines = [
  JSON.stringify({
    paseoCommit: PASEO_COMMIT,
    node: process.version,
    xtermHeadless: xtermVersion,
    corpusSha256: createHash("sha256").update(corpusText).digest("hex"),
    scenarios: corpus.scenarios.length,
  }),
];
for (const scenario of corpus.scenarios) {
  lines.push(JSON.stringify(await runScenario(Terminal, scenario)));
}
writeFileSync(args.out, `${lines.join("\n")}\n`);
