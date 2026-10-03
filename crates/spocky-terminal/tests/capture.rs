//! Terminal capture against the pinned build: `strip-ansi` 7.1.2 over
//! seeded strings built from escape-sequence pieces, and the pinned
//! `captureTerminalLines` over seeded terminal states with line ranges that
//! are negative, past the ends, fractional, or non-finite.

mod support;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::capture::{CaptureOptions, capture_lines, strip_ansi};
use spocky_wire::{TerminalCell, TerminalCursor, TerminalState};

#[test]
fn strips_the_common_sequences() {
    assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b"), "ab");
    assert_eq!(strip_ansi("a\u{1b}]0;title\u{1b}\\b"), "ab");
    assert_eq!(strip_ansi("\u{1b}[38;2;1;2;3mx"), "x");
    assert_eq!(strip_ansi("plain"), "plain");
}

#[test]
fn captures_ranges_like_the_baseline() {
    let state = state_of(&["one", "two", "three", ""]);
    let all = capture_lines(&state, &CaptureOptions::default());
    assert_eq!(all.lines, ["one", "two", "three", ""]);
    let tail = capture_lines(
        &state,
        &CaptureOptions {
            start: Some(-2.0),
            ..CaptureOptions::default()
        },
    );
    assert_eq!(tail.lines, ["three", ""]);
    let reversed = capture_lines(
        &state,
        &CaptureOptions {
            start: Some(3.0),
            end: Some(1.0),
            ..CaptureOptions::default()
        },
    );
    assert!(reversed.lines.is_empty());
    assert_eq!(reversed.total_lines, 4);
}

fn state_of(lines: &[&str]) -> TerminalState {
    let row = |text: &str| -> Vec<TerminalCell> {
        text.chars()
            .map(|c| TerminalCell::new(c.to_string()))
            .collect()
    };
    TerminalState {
        rows: 1.0,
        cols: 1.0,
        grid: lines.iter().map(|line| row(line)).collect(),
        scrollback: Vec::new(),
        cursor: TerminalCursor {
            row: 0.0,
            col: 0.0,
            hidden: None,
            style: None,
            blink: None,
        },
        title: None,
        grid_wrapped: None,
        scrollback_wrapped: None,
    }
}

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

const PIECES: &[&str] = &[
    "a",
    "b c",
    "\u{4e2d}",
    "\u{1f600}",
    "\n",
    "\u{1b}",
    "\u{9b}",
    "\u{9c}",
    "\u{7}",
    "[",
    "]",
    "(",
    ")",
    "#",
    ";",
    ":",
    "?",
    "0",
    "1",
    "12",
    "12345",
    "31",
    "m",
    "H",
    "J",
    "K",
    "Q",
    "U",
    "y",
    "~",
    "=",
    ">",
    "<",
    "\u{1b}[",
    "\u{1b}]",
    "\u{1b}\\",
    "\u{1b}[31m",
    "\u{1b}]0;t\u{7}",
    "\u{1b}]8;;http://x\u{1b}\\",
    "\u{1b}[?25h",
    "\u{1b}(B",
    "\u{1b}#8",
    "\u{9b}2K",
    "\u{1b}[1;2:3m",
    "\u{1b}[;;m",
    "\u{1b}[1234567m",
    "\u{1b}]no terminator",
    "\u{1b}]\u{9c}",
    " ",
    "\t",
];

fn strings(count: usize, seed: u64) -> Vec<String> {
    let mut rng = Seeded(seed);
    (0..count)
        .map(|_| {
            (0..=rng.next(10))
                .map(|_| PIECES[rng.next(PIECES.len())])
                .collect()
        })
        .collect()
}

/// `{"$":"NaN"}` style markers keep numbers JSON cannot hold.
const REVIVE: &str = r#"
const revive = (v) => Array.isArray(v) ? v.map(revive)
  : v && typeof v === "object" ? (v.$ === "NaN" ? NaN : v.$ === "Infinity" ? Infinity : v.$ === "-Infinity" ? -Infinity
    : Object.fromEntries(Object.entries(v).map(([k, x]) => [k, revive(x)])))
  : v;
"#;

const STRIP_NODE_SCRIPT: &str = r"
const [terminalDir, inputJson] = process.argv.slice(1);
const { default: stripAnsi } = await import(`${terminalDir}/../../../node_modules/strip-ansi/index.js`);
process.stdout.write(JSON.stringify(JSON.parse(inputJson).map((text) => stripAnsi(text))));
";

#[test]
fn strip_ansi_matches_the_pinned_package() {
    let Some(pinned) = support::pinned("strip-ansi differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let inputs = strings(3000, 0x5712);
    let input = stringify(&JsValue::Array(
        inputs.iter().cloned().map(JsValue::String).collect(),
    ));
    let expected = support::run_node(&pinned, STRIP_NODE_SCRIPT, &[&input]);
    let actual = stringify(&JsValue::Array(
        inputs
            .iter()
            .map(|text| JsValue::String(strip_ansi(text)))
            .collect(),
    ));
    assert_eq!(actual, expected);
}

const CAPTURE_NODE_SCRIPT: &str = r"
const [terminalDir, casesJson] = process.argv.slice(1);
const { captureTerminalLines } = await import(`${terminalDir}/terminal-capture.js`);
__REVIVE__
const cases = revive(JSON.parse(casesJson));
const out = cases.map(({ state, options }) =>
  captureTerminalLines({ getState: () => state }, options));
process.stdout.write(JSON.stringify(out));
";

fn random_state(rng: &mut Seeded, lines: &[String]) -> (TerminalState, JsValue) {
    let rows = |slice: &[String]| -> Vec<Vec<TerminalCell>> {
        slice
            .iter()
            .map(|line| {
                line.chars()
                    .map(|c| TerminalCell::new(c.to_string()))
                    .collect()
            })
            .collect()
    };
    let split = rng.next(lines.len() + 1);
    let mut state = state_of(&[]);
    state.scrollback = rows(&lines[..split]);
    state.grid = rows(&lines[split..]);
    let json = parse(
        &String::from_utf8(spocky_wire::encode_terminal_snapshot(&state).expect("json"))
            .expect("utf8"),
    )
    .expect("state json");
    (state, json)
}

fn option_cases() -> Vec<(CaptureOptions, &'static str)> {
    let some = |start, end, strip| CaptureOptions {
        start,
        end,
        strip_ansi: strip,
    };
    vec![
        (CaptureOptions::default(), "{}"),
        (some(Some(0.0), Some(0.0), None), r#"{"start":0,"end":0}"#),
        (some(Some(-1.0), None, None), r#"{"start":-1}"#),
        (some(None, Some(-2.0), None), r#"{"end":-2}"#),
        (some(Some(2.0), Some(1.0), None), r#"{"start":2,"end":1}"#),
        (
            some(Some(-100.0), Some(100.0), None),
            r#"{"start":-100,"end":100}"#,
        ),
        (
            some(Some(1.5), Some(3.5), None),
            r#"{"start":1.5,"end":3.5}"#,
        ),
        (
            some(Some(-1.5), Some(-0.5), None),
            r#"{"start":-1.5,"end":-0.5}"#,
        ),
        (some(Some(f64::NAN), None, None), r#"{"start":{"$":"NaN"}}"#),
        (
            some(None, Some(f64::INFINITY), None),
            r#"{"end":{"$":"Infinity"}}"#,
        ),
        (
            some(Some(f64::NEG_INFINITY), None, None),
            r#"{"start":{"$":"-Infinity"}}"#,
        ),
        (
            some(Some(1.0), Some(2.0), Some(false)),
            r#"{"start":1,"end":2,"stripAnsi":false}"#,
        ),
        (some(None, None, Some(true)), r#"{"stripAnsi":true}"#),
    ]
}

#[test]
fn capture_matches_the_pinned_build() {
    let Some(pinned) = support::pinned("capture differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut rng = Seeded(0xCA97);
    let mut cases = Vec::new();
    let mut expected_inputs = Vec::new();
    let option_list = option_cases();
    for count in [0usize, 1, 2, 4, 9] {
        let lines = strings(count, 0x100 + count as u64);
        for (options, json) in &option_list {
            let (state, state_json) = random_state(&mut rng, &lines);
            let mut case = JsObject::new();
            case.insert("state", state_json);
            case.insert("options", parse(json).expect("options"));
            expected_inputs.push(JsValue::Object(case));
            cases.push((state, *options));
        }
    }
    let input = stringify(&JsValue::Array(expected_inputs));
    let script = CAPTURE_NODE_SCRIPT.replace("__REVIVE__", REVIVE);
    let expected = support::run_node(&pinned, &script, &[&input]);
    let actual = stringify(&JsValue::Array(
        cases
            .iter()
            .map(|(state, options)| {
                let result = capture_lines(state, options);
                let mut object = JsObject::new();
                object.insert(
                    "lines",
                    JsValue::Array(result.lines.into_iter().map(JsValue::String).collect()),
                );
                #[allow(clippy::cast_precision_loss)]
                object.insert("totalLines", JsValue::Number(result.total_lines as f64));
                JsValue::Object(object)
            })
            .collect(),
    ));
    assert_eq!(actual, expected);
}
