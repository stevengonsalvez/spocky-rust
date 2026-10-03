//! Titles, OSC 633 payloads, and exit text: the pinned `terminal.test.ts`
//! title cases and a differential against the pinned build. Exported
//! `normalizeProcessTitle` and `humanizeProcessTitle` are imported; the
//! module-private `parseCommandFinishedOsc`, `stripAnsiSequences`, and
//! `extractLastOutputLinesFromText` are evaluated from their source text in
//! the digest-pinned `terminal.js`.

mod support;

use spocky_contracts::js_value::{JsValue, stringify};
use spocky_terminal::exit_lines::{
    EXIT_OUTPUT_CHAR_LIMIT, RecentOutput, last_output_lines_from_text, strip_ansi_sequences,
};
use spocky_terminal::process_title::{
    humanize_process_title, initial_title, normalize_process_title, parse_command_finished_osc,
};

#[test]
fn keeps_full_process_titles_while_stripping_path_prefixes() {
    assert_eq!(
        normalize_process_title("  /usr/local/bin/node   /tmp/dev-server.js --port=3000  ")
            .as_deref(),
        Some("node dev-server.js --port=3000")
    );
}

#[test]
fn humanizes_interpreter_backed_package_manager_commands() {
    assert_eq!(
        humanize_process_title("/usr/bin/node /opt/npm/bin/npm-cli.js run dev").as_deref(),
        Some("npm run dev")
    );
    assert_eq!(
        humanize_process_title("node /x/yarn.js").as_deref(),
        Some("yarn")
    );
}

#[test]
fn drops_common_interpreter_prefixes_for_direct_scripts() {
    assert_eq!(
        humanize_process_title("env FOO=1 python3 /srv/app/main.py --debug").as_deref(),
        Some("main.py --debug")
    );
    assert_eq!(
        humanize_process_title("node --inspect app.js").as_deref(),
        Some("node --inspect app.js")
    );
}

#[test]
fn prefers_a_trimmed_preset_title() {
    assert_eq!(
        initial_title(Some("  mine "), Some("node"), &[]).as_deref(),
        Some("mine")
    );
    assert_eq!(
        initial_title(
            Some("   "),
            Some("/usr/bin/node"),
            &["/x/npx-cli.js".to_owned(), "vite".to_owned()]
        )
        .as_deref(),
        Some("npx vite")
    );
    assert_eq!(initial_title(None, Some(""), &[]), None);
}

#[test]
fn recent_output_keeps_at_least_the_last_char_limit() {
    let mut recent = RecentOutput::default();
    let mut all = String::new();
    for index in 0..4000 {
        let chunk = format!("line {index} \u{1f600}\r\n");
        recent.push(&chunk);
        all.push_str(&chunk);
    }
    let units: Vec<u16> = all.encode_utf16().collect();
    let expected = String::from_utf16_lossy(&units[units.len() - EXIT_OUTPUT_CHAR_LIMIT..]);
    assert_eq!(recent.tail(), expected);

    let mut single = RecentOutput::default();
    single.push(&"x".repeat(EXIT_OUTPUT_CHAR_LIMIT + 5));
    assert_eq!(single.tail().len(), EXIT_OUTPUT_CHAR_LIMIT);
}

const TITLES: &[&str] = &[
    "",
    "   ",
    "\u{feff}\u{3000}",
    "node",
    "/usr/bin/node /a/b/c.js",
    "\"/usr/bin/node\" '/a/b/c.js' \"x",
    "\" ' \"\"",
    "FOO=/a/b BAR=/ BAZ= QUX=x/ =/a 1A=/b",
    "env A=1 B=2 env C=/x node /p/npm-cli.js install",
    "env",
    "env A=1",
    "bash",
    "bash -lc 'echo hi'",
    "tsx /w/scripts/run.ts\t--flag\n--other",
    "deno /d/bun.js",
    "python3 /usr/lib/pnpm.cjs",
    "/usr/bin/",
    "a/ b// /",
    "zsh -i",
    "sh\u{a0}/x/y\u{2028}z",
    "ruby\u{85}/x/y",
];

const OSC_PAYLOADS: &[&str] = &[
    "D",
    "D;0",
    "D;-0",
    "D;007",
    "D;-12",
    "D;1;2",
    "D;",
    "D;x",
    "D;+1",
    "A",
    "",
    ";D",
    "D;99999999999999999999",
];

const TEXTS: &[&str] = &[
    "",
    "\r\n\r\n  \r\n",
    "a\r\nb\rc\nd  \t\r\n\r\n",
    "\u{1b}[31mred\u{1b}[0m\u{1b}]0;title\u{7}after\u{1b}=\u{1b}[?25h\u{1b}(B",
    "\u{1b}[1;2;3 q\u{1b}[\u{1b}x\u{1b}",
    "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n",
    "\u{3000}lead\n\u{feff}\ntrail\u{a0}\u{a0}",
];

const NODE_SCRIPT: &str = r#"
import { readFileSync } from "node:fs";
const [terminalDir, inputJson] = process.argv.slice(1);
const module = await import(`${terminalDir}/terminal.js`);
const source = readFileSync(`${terminalDir}/terminal.js`, "utf8");
const take = (start, end) => {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from);
  if (from < 0 || to < 0) throw new Error(`missing ${start}`);
  return source.slice(from, to);
};
const privateSource =
  take("function parseCommandFinishedOsc(", "\nexport ") +
  take("const ESC = String.fromCharCode(0x1b);", "export async function createTerminal(");
const helpers = new Function(
  `${privateSource}; return { parseCommandFinishedOsc, stripAnsiSequences, extractLastOutputLinesFromText };`,
)();
const input = JSON.parse(inputJson);
process.stdout.write(JSON.stringify([
  input.titles.map((title) => [module.normalizeProcessTitle(title) ?? "undefined", module.humanizeProcessTitle(title) ?? "undefined"]),
  input.osc.map((data) => helpers.parseCommandFinishedOsc(data) ?? "null"),
  input.texts.map((text) => [helpers.stripAnsiSequences(text), helpers.extractLastOutputLinesFromText(text, 12)]),
]));
"#;

fn strings(values: &[&str]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|v| JsValue::String((*v).to_owned()))
            .collect(),
    )
}

fn optional(value: Option<String>) -> JsValue {
    JsValue::String(value.unwrap_or_else(|| "undefined".to_owned()))
}

#[test]
fn titles_and_exit_text_match_the_pinned_build() {
    let Some(pinned) = support::pinned("title and exit text differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut input = spocky_contracts::js_value::JsObject::new();
    input.insert("titles", strings(TITLES));
    input.insert("osc", strings(OSC_PAYLOADS));
    input.insert("texts", strings(TEXTS));
    let expected = support::run_node(&pinned, NODE_SCRIPT, &[&stringify(&JsValue::Object(input))]);

    let titles = TITLES
        .iter()
        .map(|title| {
            JsValue::Array(vec![
                optional(normalize_process_title(title)),
                optional(humanize_process_title(title)),
            ])
        })
        .collect();
    let osc = OSC_PAYLOADS
        .iter()
        .map(|data| match parse_command_finished_osc(data) {
            None => JsValue::String("null".to_owned()),
            Some(code) => {
                let mut object = spocky_contracts::js_value::JsObject::new();
                object.insert("exitCode", code.map_or(JsValue::Null, JsValue::Number));
                JsValue::Object(object)
            }
        })
        .collect();
    let texts = TEXTS
        .iter()
        .map(|text| {
            JsValue::Array(vec![
                JsValue::String(strip_ansi_sequences(text)),
                JsValue::Array(
                    last_output_lines_from_text(text, 12)
                        .into_iter()
                        .map(JsValue::String)
                        .collect(),
                ),
            ])
        })
        .collect();
    let actual = stringify(&JsValue::Array(vec![
        JsValue::Array(titles),
        JsValue::Array(osc),
        JsValue::Array(texts),
    ]));
    assert_eq!(actual, expected);
}
