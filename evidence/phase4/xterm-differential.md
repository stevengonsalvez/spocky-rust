# spocky-xterm differential against pinned @xterm/headless 6.0.0

Status: corpus 62 of 62 and fuzz 5000 of 5000 seeds match byte for byte.
Recorded 2026-10-01 by the p4_xterm_core writer (Claude, `claude-opus-5-5`,
under the 2026-10-01 routing revision).

## Specification

- Pinned build root:
  `/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204`.
- `node_modules/@xterm/headless/lib-headless/xterm-headless.js` SHA-256
  `17a90b650cf6b77cce2b98c4063884d43545e4ce177a54b76ccfc906f1aacaed`,
  version 6.0.0, xterm.js commit `f447274f430fd22513f6adbf9862d19524471c04`.
  The tests check this digest before every run.
- The TypeScript sources come from `sourcesContent` of that package's
  `xterm-headless.js.map`; each ported Rust file names its source file and
  keeps the MIT notice. `crates/spocky-xterm/LICENSE-xterm.js` holds the
  license text.
- Node v22.20.0, binary SHA-256
  `1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931`.
- Paseo `5de45e2` `packages/server/src/terminal/terminal.ts` defines the
  options, handlers and extraction the harness reproduces.

## What is ported

| Area | Rust file | Source |
|---|---|---|
| UTF-8 PTY decoding (Node `StringDecoder`) | `decoder.rs` | Node v22.20.0 `string_decoder.cc` behaviour |
| Parameters and sub parameters | `params.rs` | `parser/Params.ts` |
| VT500 transition table, OSC sub parser | `parser.rs` | `parser/EscapeSequenceParser.ts`, `parser/OscParser.ts` |
| Parse loop, CSI, ESC, execute and OSC dispatch, custom handler registration, `StringToUtf32`, RIS | `terminal.rs` | `EscapeSequenceParser.ts`, `InputHandler.ts`, `CoreTerminal.ts`, `headless/Terminal.ts`, `input/TextDecoder.ts`, `services/CoreService.ts` |
| InputHandler actions | `input_handler.rs` | `InputHandler.ts` |
| Buffer, BufferSet, scroll, resize, reflow, tab stops | `buffer.rs` | `buffer/Buffer.ts`, `buffer/BufferReflow.ts`, `buffer/BufferSet.ts`, `services/BufferService.ts` |
| Buffer lines | `buffer_line.rs` | `buffer/BufferLine.ts` |
| Circular scrollback ring (1000 lines) | `circular_list.rs` | `CircularList.ts` |
| Attributes and cells | `attributes.rs` | `buffer/AttributeData.ts`, `buffer/CellData.ts`, `buffer/Constants.ts` |
| Unicode 6 widths and join state | `unicode.rs` | `input/UnicodeV6.ts`, `services/UnicodeService.ts` |
| Charsets incl. DEC special graphics | `charsets.rs` | `data/Charsets.ts` |
| Public read views | `view.rs` | `public/BufferApiView.ts`, `public/BufferLineApiView.ts` |

Faithfulness details that the differential exercises: ring slots past
`length` keep stale lines, as the JavaScript array does; stale combined
strings resurface through `copyCellsFrom`; typed-array reads outside a line
give 0 and writes are ignored; `eraseInDisplay(1)` at the last column
indexes `lines.get(y + 1)` without `ybase`; a read of an `undefined` line
throws at the same statement.

Exceptions: when xterm throws while parsing a write, the exception escapes
its write timer, Paseo's terminal worker keeps running, and xterm's write
queue never parses again. `Terminal::write` returns `Exception` and every
later write is not parsed (`is_wedged`). Corpus scenario
`branch-erase-display-throws` triggers this (ED 1 at the bottom right of a
fresh screen) and both sides agree.

Left out, each without effect on anything Paseo reads: replies xterm itself
sends through `onData` (DA, DSR, DECRQM, DECRQSS, window reports; Paseo never
subscribes to `terminal.onData`), notifications (bell, colors, focus, mouse,
keypad, bracketed paste, synchronized output), markers and OSC 8 link data
(link ids are only tested for zero), async parser handlers (none are
registered), the DCS payload (its only handler answers through `onData`),
and logging.

## Harness

- `scripts/phase4/xterm-capture.mjs`: the memo capture script with the
  corpus path as an argument, one `JSON.stringify` line per scenario, and
  exception handling (`wedged`, `resizeErrors`, present only when they
  happen). On the 16 memo scenarios its lines are byte-identical to the
  original `terminal-emulator-capture.mjs` output, whose digest reproduces as
  `bf163d53283d959bb39e3786f1e468edcdfc685b2beb72a3275899192417eb77`.
- `crates/spocky-xterm/tests/common/mod.rs`: the same handlers and
  extraction in Rust over `spocky-xterm`, written with
  `spocky_contracts::js_value::stringify`; compares whole scenario lines.
- `scripts/phase4/xterm-corpus.json`: the 16 memo scenarios first
  (semantically unchanged; reformatted), then 46 scenarios, one per ported
  branch group: wrapping and joining of wide and combining characters,
  insert mode, convert EOL, scroll regions, reverse wraparound, tab stops,
  SO and SI with G1 to G3, IND, NEL, RI, every cursor movement, origin mode,
  ED 0 to 3, DECSED and DECSEL with DECSCA, EL, IL, DL, ICH, DCH, ECH, SU, SD,
  SL, SR, DECIC, DECDC, REP, SM and RM, DECSET and DECRST, the three alternate
  screen modes, SGR including colon forms and underline styles, OSC 8
  underline, DECSTR, DECSCUSR, DECSTBM edges, cursor save and restore, RIS,
  every charset designation, DECALN, OSC variants (C1 OSC and ST, CAN, SUB,
  leading zeros, huge ids), query handlers, parser edges (param and sub param
  limits, C0 and DEL inside CSI, SOS, PM, APC, DCS), BOM and C1 controls,
  reflow larger and smaller with wide characters, the cursor line, scrollback,
  alternate buffer resize, row changes with scrollback, minimum sizes, the
  throwing ED 1, and a one-row alternate screen.
- `crates/spocky-xterm/tests/xterm_fuzz.rs`: SplitMix64 seeded generator of
  PTY byte streams (text, wide and combining characters, invalid UTF-8, C0
  and C1 controls, CSI, SGR, DEC modes, ESC, OSC, DCS, scrollback floods)
  cut into random chunks with resizes between them.
- A mutation check (charset mapping disabled in `print`) made the corpus
  test fail on `insert-and-charset`, so the comparison is live.

Nothing is normalized: each scenario's full captured state, last output
lines, titles, PTY responses, command-finished events and exception flags
are compared as text.

## Results

| Run | Scenarios | Full matches | Failures | Wedged | Resize errors |
|---|---|---|---|---|---|
| corpus, memo part | 16 | 16 | 0 | 0 | 0 |
| corpus, whole | 62 | 62 | 0 | 1 | 0 |
| fuzz seeds 0..400 (default test) | 400 | 400 | 0 | 1 | 0 |
| fuzz seeds 0..5000 | 5000 | 5000 | 0 | 1 | 0 |

Fuzz corpus digests (the generated corpus JSON): seeds 0..400
`445461229d2d7faee860ae60bfc7e1957272d7ef5ca81e19c255d06dcaf2907d`, seeds
0..5000 `d49039e31e46942669d173946373bb3ccdc21217e6d6a81f73075c9ba81168aa`.

## Commands

```sh
node=$HOME/.nvm/versions/node/v22.20.0/bin/node
export CARGO_TARGET_DIR=/private/tmp/spocky-targets/p4_xterm_core CARGO_BUILD_JOBS=3
gate=/private/tmp/spocky-targets/build-gate.sh
SPOCKY_PINNED_NODE=$node $gate cargo test --locked -p spocky-xterm -- --nocapture
SPOCKY_XTERM_FUZZ_SEEDS=5000 SPOCKY_PINNED_NODE=$node $gate \
  cargo test --locked -p spocky-xterm --test xterm_fuzz -- --nocapture
$gate cargo clippy --locked -p spocky-xterm --all-targets -- -D warnings
$gate cargo fmt --package spocky-xterm -- --check
$node scripts/phase4/xterm-capture.mjs \
  --paseo-root /private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204 \
  --corpus scripts/phase4/xterm-corpus.json \
  --out evidence/raw/phase4/xterm-corpus-capture.jsonl
```

Acceptance on rustc 1.94.0: 18 unit tests, the corpus test and the 400-seed
fuzz test pass; clippy with `-D warnings` and the format check are clean.

## Evidence digests (SHA-256)

| artifact | digest |
|---|---|
| `scripts/phase4/xterm-capture.mjs` | `8884ed5c6f2555975af213ba363c6c3961f2773a9ffb2f29ace020c4fac6b152` |
| `scripts/phase4/xterm-corpus.json` | `70e706aa58d994c5c877eaff783d273cef8e0f7595cfbf10897264192c861f26` |
| `crates/spocky-xterm/tests/common/mod.rs` | `7694108cc3119649ee27777e1fa1f7ca86dc847dbdebcc8c7699337c045d67dd` |
| `crates/spocky-xterm/tests/xterm_corpus.rs` | `2b677d62bdd158fd6b1ee0503c67032955c8c32161274a4a0dca75bcb3168929` |
| `crates/spocky-xterm/tests/xterm_fuzz.rs` | `cb5959671cb46d6bb243b7e92c392278987d1380ff91a7d4d668261d469360d1` |
| `evidence/raw/phase4/xterm-corpus-capture.jsonl` (untracked, two runs identical) | `773f864b0d43de2c8a48c004deaee16489c83eb2983aabc06951b14da855d7e1` |

## Gaps

- Write timing: xterm parses writes from a timer queue in 12 ms slices, while
  `terminal.resize` applies at once, so in Paseo a resize can land before
  queued PTY output is parsed. `Terminal::write` parses at once, which equals
  xterm whenever the caller awaits each write callback (as this harness and
  every `getState` read after `write("")` do). The integrating lane must keep
  Paseo's ordering of writes, callbacks and resizes.
- Branches not reached by any run: a resize that throws (`resizeErrors` stayed
  0; analysis suggests the reflow throw needs a layout the buffer cannot
  reach), payloads above the 10,000,000 unit OSC limit, and REP or loop counts
  in the billions (the fuzz bounds loop counts to keep node within its time
  limit).
- The corpus and fuzz drive only the API surface Paseo uses; other xterm
  public API (`scrollLines`, `clear`, markers, `onData`) is not ported.
- macOS x64 only; other platforms are not run here.
