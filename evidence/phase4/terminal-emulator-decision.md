# Terminal emulator decision (DTRM-001, DTRM-002)

Status: DECISION NEEDED. Recorded 2026-10-01 by the p4_terminal writer
(Claude, `claude-opus-5-5`, under the 2026-10-01 routing revision).

## What has to match

Pinned Paseo `5de45e2` feeds PTY output into `@xterm/headless` 6.0.0
(`new Terminal({ rows, cols, scrollback: 1000, allowProposedApi: true })`) in
`packages/server/src/terminal/terminal.ts`. Every observable terminal output
except raw output frames comes from that emulator:

- snapshots: `getState()` walks `buffer.active` cell by cell (char, fg, bg,
  color modes, bold, italic, underline, dim, inverse, strikethrough), the
  scrollback above `baseY`, per-row `isWrapped` flags, and the cursor position,
  visibility, style, and blink read from `_core.coreService`;
- restore frames: `renderTerminalSnapshotToAnsi` of that state;
- exit info: `lastOutputLines` from `translateToString(true)` over the buffer;
- title events: `onTitleChange` (OSC 0 and 2);
- PTY replies: the CSI `c`, `n`, `?n` and OSC 10, 11, 12 handlers write bytes
  back to the PTY, and OSC 633 drives `onCommandFinished`, so the parser must
  dispatch the same sequences with the same parameters and cursor state;
- capture: `captureTerminal` lines come from the same cell grid.

So the emulator must reproduce xterm's buffer model byte for byte as JSON: its
Unicode version 6 width table, wide-char placement, color mode encoding,
pending-wrap cursor, reflow on resize, charsets, and invalid UTF-8 handling.

## Method

1. `scripts/phase4/terminal-emulator-corpus.json` defines 16 scenarios as PTY
   byte chunks plus resizes: plain text, wide chars and combining marks, SGR
   colors (16, 256, RGB, colon form, attribute resets, BCE), cursor movement and
   editing, alternate screen active and exited, scrollback overflow past 1000
   lines, resize reflow, row shrink, partial UTF-8 across chunks including
   invalid bytes, titles and PTY queries, cursor style, autowrap edges, insert
   mode and DEC line drawing, and erase in display.
2. `scripts/phase4/terminal-emulator-capture.mjs` replays the corpus through the
   pinned `packages/server/dist/server/terminal/terminal.js` itself, so the
   handlers, the state and last-output-line extraction, the input-mode replies
   and the exit info are the pinned code. Two loader substitutions
   (`terminal-emulator-hooks.mjs`) give it a PTY with no process behind it
   (`terminal-emulator-fake-pty.mjs`) and the pinned `@xterm/headless` with its
   `Terminal` remembered (`terminal-emulator-xterm-recorder.mjs`) so the raw
   title events can be read. Bytes are decoded with a utf8 `StringDecoder`
   exactly as node-pty's `socket.setEncoding("utf8")` does before `onData`.
   An earlier version transcribed the handlers and extractors from
   `terminal.ts`; its capture and this one agree on all 16 scenarios. Two runs
   produce the same digest.
3. `scripts/phase4/terminal-emulator-eval` (its own Cargo workspace, so no
   candidate crate enters the Spocky lockfile) replays the same bytes through a
   candidate, maps its screen to the same JSON shape, and compares
   `JSON.stringify` text per cell, per row flag, and per section with
   `spocky_contracts::js_value`.

Commands:

```sh
node=$HOME/.nvm/versions/node/v22.20.0/bin/node
dist=/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204
$node scripts/phase4/terminal-emulator-capture.mjs --paseo-root "$dist" \
  --out evidence/raw/phase4/terminal-emulator-xterm.json
CARGO_TARGET_DIR=/private/tmp/spocky-targets/p4_terminal_eval CARGO_BUILD_JOBS=2 \
  /private/tmp/spocky-targets/build-gate.sh cargo build --offline --release \
  --manifest-path scripts/phase4/terminal-emulator-eval/Cargo.toml
/private/tmp/spocky-targets/p4_terminal_eval/release/spocky-terminal-emulator-eval \
  scripts/phase4/terminal-emulator-corpus.json \
  evidence/raw/phase4/terminal-emulator-xterm.json \
  evidence/raw/phase4/terminal-emulator-vt100-report.json
```

## Results: vt100 0.16.2

A scenario is a full match only when the whole captured object is identical.
Checks are cursor, gridWrapped, scrollbackWrapped, lastOutputLines, titles,
responses, and commandFinished.

| scenario | full match | grid cells | scrollback cells | checks | failed checks |
|---|---|---|---|---|---|
| plain-text | yes | 120/120 | 0/0 | 7/7 | - |
| wide-chars | no | 117/120 | 20/20 | 5/7 | gridWrapped, lastOutputLines |
| sgr-colors | no | 171/192 | 0/0 | 7/7 | - |
| cursor-moves | no | 156/160 | 0/0 | 6/7 | lastOutputLines |
| alternate-screen-active | yes | 120/120 | 0/0 | 7/7 | - |
| alternate-screen-exited | yes | 120/120 | 0/0 | 7/7 | - |
| scrollback-overflow | yes | 100/100 | 20000/20000 | 7/7 | - |
| resize-reflow | no | 140/200 | 0/0 | 4/7 | cursor, gridWrapped, lastOutputLines |
| partial-utf8 | no | 71/80 | 0/0 | 5/7 | cursor, lastOutputLines |
| title-and-queries | yes | 80/80 | 0/0 | 7/7 | - |
| cursor-style | no | 80/80 | 0/0 | 6/7 | cursor |
| cursor-style-bar | no | 80/80 | 0/0 | 6/7 | cursor |
| autowrap-edges | no | 39/60 | 0/0 | 4/7 | cursor, gridWrapped, lastOutputLines |
| insert-and-charset | no | 68/80 | 0/0 | 5/7 | cursor, lastOutputLines |
| erase-display | yes | 60/60 | 0/0 | 7/7 | - |
| resize-shrink-rows | no | 76/84 | 0/0 | 5/7 | cursor, lastOutputLines |

Full matches: 6 of 16. Observed divergences, each from the first differing
cells of the report:

- wide-chars: xterm's Unicode 6 table gives U+1F600 width 1; vt100 uses
  `unicode-width` and gives it width 2, shifting the rest of the row.
- sgr-colors: `38;5;1` is `fgMode` 2 in xterm and indistinguishable from SGR 31
  (`fgMode` 1) in vt100's `Color::Idx`; vt100 has no strikethrough attribute.
- cursor-moves: horizontal tab output differs (xterm places `a`, `c` at the tab
  stops; vt100 leaves those cells empty).
- resize-reflow and resize-shrink-rows: vt100 `set_size` truncates and does not
  reflow or push rows to scrollback; xterm reflows wrapped lines and keeps the
  cursor row anchored to the bottom.
- partial-utf8: xterm, after Node's `StringDecoder`, shows U+FFFD for each
  invalid or truncated sequence (`A\uFFFDB\uFFFD(C`, `\uFFFD\uFFFD\uFFFDD`);
  vt100 drops them (`AB(C`, `D`).
- autowrap-edges: vt100 ignores DECAWM off and wraps `NOWRAPPING-HERE`, where
  xterm overwrites the last column (`NOWRAPPINE`); backspace from the
  pending-wrap position lands on column 8 in xterm (`01234567x9`) and column 9
  in vt100 (`012345678x`).
- insert-and-charset: vt100 does not implement IRM insert mode (`XYcdef`
  against `XYabcdef`), the DEC special graphics charset (`lqqk` against
  `\u250c\u2500\u2500\u2510`), or HTS and TBC tab stop edits.
- cursor-style, cursor-style-bar: vt100 does not track DECSCUSR style or blink.

## Candidates not evaluated

`alacritty_terminal` and `wezterm-term` are not in the local registry cache and
the lane forbids network access beyond loopback, so neither was built.
`termwiz` 0.23.3 is cached but has no VT screen model with scrollback, reflow,
and parser hooks. Inference, not measured: both alacritty_terminal and
wezterm-term size characters with `unicode-width` (Unicode 15 widths), so the
wide-chars divergence above would recur, and each has its own reflow algorithm.
A corpus run would falsify this if either matched; the evaluator takes another
`run_*` function per candidate.

## Recommendation

Port the needed `@xterm/headless` 6.0.0 core faithfully into
`crates/spocky-terminal` (MIT, attribution preserved): the escape sequence
parser state machine, the InputHandler subset that the pinned build reaches,
`Buffer` and `BufferLine` with the 1000-line circular scrollback, `reflow`, the
Unicode 6 width table, charsets, DEC private modes, and the core service fields
that `extractCursorState` reads, plus a Rust `StringDecoder` equivalent for
PTY bytes. Qualify it with this corpus grown to every InputHandler branch and a
seeded byte fuzz differential against the pinned xterm, both byte for byte.

Why not the alternatives:

- No crate matches; vt100 reaches 6 of 16 and fails on structural model
  differences (width table, color mode, reflow), not missing escape codes.
- An interim Node host for `@xterm/headless` would keep a JavaScript runtime in
  the daemon's hottest path (every PTY chunk) with IPC per chunk, the latency
  `docs/terminal-performance.md` warns about. The PGlite exception was accepted
  for a storage engine that has no Rust equivalent; xterm's buffer logic is a
  bounded, deterministic algorithm that ports.

Cost and risk: an estimated 4000 to 6000 lines of Rust, and residual risk in
rarely used sequences. The fuzz differential bounds that risk.

## Evidence digests (SHA-256)

| artifact | digest |
|---|---|
| `scripts/phase4/terminal-emulator-corpus.json` | `f5504c7e8d933ca60cf32b1d61898c64688e1cbc89bfbe04b041ce6b0275faf8` |
| `scripts/phase4/terminal-emulator-capture.mjs` | `d91071aff9898b95cb9d7509b41118390aa2f39b92f04e58eefa32689bcac189` |
| `scripts/phase4/terminal-emulator-hooks.mjs` | `3c13ce694d2e57a839874cdf195a1dc14f4fa8ed21dc1c2653d4ff8af03b6fb0` |
| `scripts/phase4/terminal-emulator-fake-pty.mjs` | `6f5a5949c50a754d02df16c209ef80157ff94ba0287a8c0aedda6d366a17ff95` |
| `scripts/phase4/terminal-emulator-xterm-recorder.mjs` | `9c625bf926a9d580c2b71065fe5e8ebfd4e327cd18ba718cbb6661b66389708c` |
| `scripts/phase4/terminal-emulator-eval/Cargo.toml` | `8a190a28efee41ff6cbc544063cf3c26b0b3cd2d30e9bca2415d2cd1d5677bd9` |
| `scripts/phase4/terminal-emulator-eval/src/main.rs` | `c036e4b3b3cb6b5f173e3a08e3b73017263d51d1e75a6e21f7877c0891396791` |
| `evidence/raw/phase4/terminal-emulator-xterm.json` (untracked) | `a300da69cb913f0e2df8ac6cd6517a983f304c362340b4da29a2db33a2824a60` |
| `evidence/raw/phase4/terminal-emulator-vt100-report.json` (untracked) | `f6dc81aabe046fef76bd015ac5bc2a20a5461577e5f4e8975535d602e08ba094` |
| pinned `@xterm/headless/lib-headless/xterm-headless.js` | `17a90b650cf6b77cce2b98c4063884d43545e4ce177a54b76ccfc906f1aacaed` |
| Node v22.20.0 binary | `1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931` |

Toolchain: rustc 1.94.0. Pinned build root:
`/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204`.
