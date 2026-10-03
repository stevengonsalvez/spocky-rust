//! Input-mode tracking: the pinned `terminal-input-mode.test.ts` behaviors
//! and a differential against the pinned `TerminalInputModeTracker` over
//! seeded PTY output split at arbitrary points. Every feed result, state,
//! and preamble must match.

mod support;

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_terminal::input_mode::InputModeTracker;

#[test]
fn tracks_kitty_flags_with_push_pop_and_query() {
    let mut tracker = InputModeTracker::new();
    assert!(tracker.feed("\u{1b}[>1u").changed);
    assert!(tracker.feed("\u{1b}[>5u").changed);
    assert_eq!(tracker.feed("\u{1b}[?u").responses, ["\u{1b}[?5u"]);
    assert!(tracker.feed("\u{1b}[<u").changed);
    assert_eq!(tracker.preamble(), "\u{1b}[=1;1u");
    assert!(tracker.feed("\u{1b}[<9u").changed);
    assert_eq!(tracker.preamble(), "");
}

#[test]
fn holds_a_sequence_split_across_chunks() {
    let mut tracker = InputModeTracker::new();
    assert!(!tracker.feed("text\u{1b}[?20").changed);
    assert!(tracker.feed("04h").changed);
    assert!(tracker.state().bracketed_paste);
    assert!(tracker.feed("\u{1b}[?1;9001h").changed);
    assert!(tracker.supports_modified_enter());
    assert_eq!(tracker.preamble(), "\u{1b}[?9001h\u{1b}[?1h\u{1b}[?2004h");
    tracker.reset();
    assert_eq!(tracker.preamble(), "");
}

const PIECES: &[&str] = &[
    "\u{1b}[>1u",
    "\u{1b}[>u",
    "\u{1b}[>15u",
    "\u{1b}[=5;1u",
    "\u{1b}[=5;0u",
    "\u{1b}[=7u",
    "\u{1b}[=;2u",
    "\u{1b}[<u",
    "\u{1b}[<2u",
    "\u{1b}[<0u",
    "\u{1b}[?u",
    "\u{1b}[u",
    "\u{1b}[?1h",
    "\u{1b}[?1l",
    "\u{1b}[?2004;9001h",
    "\u{1b}[?9001;x;1l",
    "\u{1b}[?;1h",
    "\u{1b}[?01h",
    "\u{1b}[>99999999999999999999u",
    "\u{1b}[31m",
    "\u{1b}[",
    "\u{1b}",
    "?2004",
    ";",
    "h",
    "u",
    "plain \u{4e2d}",
    "\r\n",
];

struct Seeded(u64);

impl Seeded {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((self.0 >> 33) % u64::try_from(bound).expect("bound")).expect("index")
    }
}

/// 200 streams of pieces, each split into chunks at character boundaries.
fn generated_streams() -> Vec<Vec<String>> {
    let mut rng = Seeded(0x00c5_1a7e);
    (0..200)
        .map(|_| {
            let mut text = String::new();
            for _ in 0..=rng.next(20) {
                text.push_str(PIECES[rng.next(PIECES.len())]);
            }
            let chars: Vec<char> = text.chars().collect();
            let mut chunks = Vec::new();
            let mut index = 0;
            while index < chars.len() {
                let take = 1 + rng.next(8);
                let end = (index + take).min(chars.len());
                chunks.push(chars[index..end].iter().collect());
                index = end;
            }
            chunks
        })
        .collect()
}

const NODE_SCRIPT: &str = r"
const [, protocolDir, streamsJson] = process.argv.slice(1);
const { TerminalInputModeTracker } = await import(`${protocolDir}/terminal-input-mode.js`);
const out = JSON.parse(streamsJson).map((chunks) => {
  const tracker = new TerminalInputModeTracker();
  return chunks.map((chunk) => {
    const result = tracker.feed(chunk);
    return [result.changed, result.responses, tracker.getState(), tracker.getPreamble(), tracker.supportsModifiedEnter()];
  });
});
process.stdout.write(JSON.stringify(out));
";

fn rust_output(streams: &[Vec<String>]) -> String {
    let streams = streams
        .iter()
        .map(|chunks| {
            let mut tracker = InputModeTracker::new();
            JsValue::Array(
                chunks
                    .iter()
                    .map(|chunk| {
                        let result = tracker.feed(chunk);
                        let state = tracker.state();
                        let mut object = JsObject::new();
                        object.insert(
                            "kittyKeyboardFlags",
                            JsValue::Number(state.kitty_keyboard_flags),
                        );
                        object.insert("win32InputMode", JsValue::Bool(state.win32_input_mode));
                        object.insert(
                            "applicationCursorKeys",
                            JsValue::Bool(state.application_cursor_keys),
                        );
                        object.insert("bracketedPaste", JsValue::Bool(state.bracketed_paste));
                        JsValue::Array(vec![
                            JsValue::Bool(result.changed),
                            JsValue::Array(
                                result.responses.into_iter().map(JsValue::String).collect(),
                            ),
                            JsValue::Object(object),
                            JsValue::String(tracker.preamble()),
                            JsValue::Bool(tracker.supports_modified_enter()),
                        ])
                    })
                    .collect(),
            )
        })
        .collect();
    stringify(&JsValue::Array(streams))
}

#[test]
fn input_mode_tracking_matches_pinned_tracker() {
    let Some(pinned) = support::pinned("input mode differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let streams = generated_streams();
    let input = stringify(&JsValue::Array(
        streams
            .iter()
            .map(|chunks| JsValue::Array(chunks.iter().cloned().map(JsValue::String).collect()))
            .collect(),
    ));
    let protocol_dir = pinned.protocol_dir.to_string_lossy().into_owned();
    let expected = support::run_node(&pinned, NODE_SCRIPT, &[&protocol_dir, &input]);
    assert_eq!(rust_output(&streams), expected);
}
