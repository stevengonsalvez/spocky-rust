//! Differential check of `readClaudeWorkflowResultFile` and
//! `parseClaudeWorkflowResult` against the pinned build on real files: the
//! 1 MiB cutoff, lossy UTF-8, the 100000-unit truncation, a surrogate split
//! by it, and unreadable paths. The files live in a disposable directory.

mod support;

use std::path::{Path, PathBuf};

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_provider_claude::subagents::workflow_output::{
    parse_claude_workflow_result, read_claude_workflow_result_file,
};

const MAX_BYTES: usize = 1024 * 1024;

const NODE_SCRIPT: &str = r#"
import { readFileSync } from "node:fs";
const [dist, inputFile] = process.argv.slice(1);
const input = JSON.parse(readFileSync(inputFile, "utf8"));
const { readClaudeWorkflowResultFile, parseClaudeWorkflowResult } = await import(
  `${dist}/server/agent/providers/claude/subagents/workflow-output.js`);
const out = [];
const put = (value) => out.push(value === undefined ? "undefined" : JSON.stringify(value));
for (const path of input.paths) put(readClaudeWorkflowResultFile(path));
for (const contents of input.contents) put(parseClaudeWorkflowResult(contents));
process.stdout.write(out.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/providers/claude/subagents/workflow-output.js",
    "ad0143ea0b6dbe7101548dab78780ed8530f89f6f60fbb944a937beedce2f3ee",
)];

/// A disposable directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spocky-workflow-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&path).expect("scratch dir");
        Self(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `{"result":"<body>"}` padded with `a` inside the string to `total` bytes.
fn padded(total: usize) -> Vec<u8> {
    let head = br#"{"result":""#;
    let tail = br#""}"#;
    let mut bytes = head.to_vec();
    bytes.resize(total - tail.len(), b'a');
    bytes.extend_from_slice(tail);
    assert_eq!(bytes.len(), total);
    bytes
}

fn result_json(body: &str) -> Vec<u8> {
    format!(
        r#"{{"result":{}}}"#,
        stringify(&JsValue::String(body.to_owned()))
    )
    .into_bytes()
}

fn files(scratch: &Scratch) -> Vec<String> {
    let mut paths = vec![scratch.write("small.json", br#"{"result":{"summary":"ok"}}"#)];
    paths.push(scratch.write("at-limit.json", &padded(MAX_BYTES)));
    paths.push(scratch.write("over-limit.json", &padded(MAX_BYTES + 1)));
    paths.push(scratch.write("far-over.json", &padded(MAX_BYTES * 2 + 17)));
    paths.push(scratch.write("empty.json", b""));
    paths.push(scratch.write("not-json.json", b"not json"));
    paths.push(scratch.write("array.json", b"[1,2,3]"));
    paths.push(scratch.write("no-result.json", br#"{"other":1}"#));
    paths.push(scratch.write("null-result.json", br#"{"result":null}"#));
    paths.push(scratch.write("bom.json", b"\xef\xbb\xbf{\"result\":\"after bom\"}"));
    paths.push(scratch.write("nul.json", b"{\"result\":\"a\0b\"}"));
    // Lossy UTF-8: stray continuation, truncated sequences, overlongs,
    // encoded surrogates, and values past U+10FFFF.
    for (index, invalid) in [
        &b"\xff"[..],
        b"\xc3",
        b"\xc3(",
        b"\xe2\x82",
        b"\xe2\x82(",
        b"\xf0\x9f\x98",
        b"\xf0\x9f\x98(",
        b"\xc0\xaf",
        b"\xe0\x80\xaf",
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xf8\x88\x80\x80\x80",
        b"\x80\x80",
        b"ok \xe2\x82\xac \xff end",
    ]
    .iter()
    .enumerate()
    {
        let mut bytes = br#"{"result":"x"#.to_vec();
        bytes.extend_from_slice(invalid);
        bytes.extend_from_slice(br#"y"}"#);
        paths.push(scratch.write(&format!("lossy-{index}.json"), &bytes));
    }
    // Truncation at 100000 UTF-16 units, with and without a surrogate pair
    // on the cut.
    for (name, body) in [
        ("cut-exact", "x".repeat(100_000)),
        ("cut-over", "x".repeat(100_001)),
        ("cut-far", "x".repeat(250_000)),
        ("cut-padded", format!("  {}  ", "x".repeat(99_999))),
        (
            "cut-surrogate-split",
            format!("{}\u{1f600}{}", "x".repeat(99_999), "y".repeat(10)),
        ),
        (
            "cut-surrogate-before",
            format!("{}\u{1f600}{}", "x".repeat(99_998), "y".repeat(10)),
        ),
        (
            "cut-surrogate-after",
            format!("{}\u{1f600}{}", "x".repeat(100_000 - 2), "y"),
        ),
        ("cut-wide", "\u{20ac}".repeat(100_001)),
    ] {
        paths.push(scratch.write(&format!("{name}.json"), &result_json(&body)));
    }
    // Truncated pretty-printed JSON of an object (the fenced form).
    let wide: String = (0..4000)
        .map(|index| format!(r#""k{index}":"{}""#, "v".repeat(40)))
        .collect::<Vec<_>>()
        .join(",");
    paths.push(scratch.write(
        "cut-fenced.json",
        format!(r#"{{"result":{{{wide}}}}}"#).as_bytes(),
    ));
    // Unreadable paths: missing, a directory, no permission.
    paths.push(
        scratch
            .0
            .join("missing.json")
            .to_string_lossy()
            .into_owned(),
    );
    paths.push(scratch.0.to_string_lossy().into_owned());
    let denied = scratch.write("denied.json", br#"{"result":"secret"}"#);
    set_mode(Path::new(&denied), 0o000);
    paths.push(denied);
    paths.push(String::new());
    paths
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod fixture");
}

/// A JSON string literal with every byte of `bytes` (ASCII) escaped as needed.
fn ascii_literal(bytes: &[u8]) -> String {
    let body: String = bytes
        .iter()
        .map(|byte| match byte {
            b'"' => "\\\"".to_owned(),
            other => char::from(*other).to_string(),
        })
        .collect();
    format!("\"{body}\"")
}

/// Contents for `parseClaudeWorkflowResult`, as JSON text of an array. Text
/// read from a file never holds a lone surrogate (UTF-8 decoding replaces
/// it), so the byte count is exercised with ASCII and with multi-byte text.
fn contents_json() -> String {
    let mut items: Vec<String> = Vec::new();
    for total in [MAX_BYTES - 1, MAX_BYTES, MAX_BYTES + 1] {
        // Two-byte characters make the byte length twice the unit length.
        let overhead = br#"{"result":""#.len() + br#""}"#.len();
        let body = "\u{e9}".repeat((total - overhead) / 2);
        let pad = "a".repeat((total - overhead) % 2);
        items.push(stringify(&JsValue::String(format!(
            r#"{{"result":"{body}{pad}"}}"#
        ))));
    }
    items.push(ascii_literal(&padded(MAX_BYTES)));
    items.push(ascii_literal(&padded(MAX_BYTES + 1)));
    for text in [
        r#"{"result":"\ud83d"}"#,
        r#"{"result":"😀"}"#,
        r#"{"result":{"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":{"i":"deep"}}}}}}}}}}"#,
        r#"{"result":{"a":{"b":{"c":{"d":{"e":{"f":{"g":"seven"}}}}}}}}"#,
        r#"{"result":true}"#,
        r#"{"result":-0}"#,
        r#"{"result":1e21}"#,
        r#"{"result":[1,2]}"#,
        r#"{"result":{"a":1,"b":2}}"#,
        r#"{"result":{"x":null}}"#,
        r#"{"result":"   "}"#,
        r#"{"result":""}"#,
        r#"{"__proto__":{"result":"inherited"}}"#,
        r#"{"result":{"__proto__":"p"}}"#,
        r#"{"result":{"a":"1"},"result":"dup"}"#,
        "  {\"result\": \"spaced\"}  \n",
        "{\"result\": \"\u{feff}bom inside\"}",
        "\u{feff}{\"result\": \"leading bom\"}",
    ] {
        items.push(stringify(&JsValue::String(text.to_owned())));
    }
    format!("[{}]", items.join(","))
}

/// Mirrors the script's `put`: `undefined` is the word, others stringify.
fn line(value: Option<String>) -> String {
    value.map_or_else(
        || "undefined".to_owned(),
        |text| stringify(&JsValue::String(text)),
    )
}

#[test]
fn workflow_output_files_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = Scratch::new();
    let paths = files(&scratch);
    let contents = contents_json();
    // The inputs go through a file: the 1 MiB contents do not fit in argv.
    let input = format!(
        r#"{{"paths":{},"contents":{contents}}}"#,
        stringify(&JsValue::Array(
            paths.iter().cloned().map(JsValue::String).collect(),
        ))
    );
    let input_file = scratch.write("input.json.txt", input.as_bytes());
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[input_file]);
    let mut actual: Vec<String> = paths
        .iter()
        .map(|path| line(read_claude_workflow_result_file(path)))
        .collect();
    for item in parse(&contents)
        .expect("contents JSON")
        .as_array()
        .expect("contents array")
    {
        actual.push(line(parse_claude_workflow_result(
            item.as_str().expect("contents are strings"),
        )));
    }
    let actual = actual.join("\n") + "\n";
    // Restore access so the scratch directory can be removed.
    for path in &paths {
        if path.ends_with("denied.json") {
            set_mode(Path::new(path), 0o600);
        }
    }
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "case {index} differs");
    }
    assert_eq!(actual, expected);
}
