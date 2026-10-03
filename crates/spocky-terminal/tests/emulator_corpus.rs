//! The terminal emulator corpus through `spocky-xterm` and this crate's state
//! extraction, against the capture of the pinned `@xterm/headless` 6.0.0
//! (`scripts/phase4/terminal-emulator-capture.mjs`, run here on the pinned
//! Node). Every scenario must match as one object: state with wrap flags,
//! last output lines, title events, PTY replies, and OSC 633 events.

mod support;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::exit_lines::EXIT_OUTPUT_LINE_LIMIT;
use spocky_terminal::handlers::register_handlers;
use spocky_terminal::restore::SnapshotOptions;
use spocky_terminal::snapshot::{extract_state, last_output_lines};
use spocky_terminal::utf8_decoder::Utf8Decoder;
use spocky_wire::encode_terminal_snapshot;
use spocky_xterm::Terminal;

/// SHA-256 of the pinned `@xterm/headless` bundle the capture runs.
const XTERM_HEADLESS_SHA256: &str =
    "17a90b650cf6b77cce2b98c4063884d43545e4ce177a54b76ccfc906f1aacaed";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn as_u32(value: &JsValue) -> u32 {
    format!("{}", value.as_f64().expect("number"))
        .parse()
        .expect("u32")
}

fn op_bytes(op: &JsValue) -> Vec<u8> {
    if let Some(text) = op.get("text").and_then(JsValue::as_str) {
        return text.as_bytes().to_vec();
    }
    if let Some(hex) = op.get("hex").and_then(JsValue::as_str) {
        return (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect();
    }
    let repeat = op.get("repeat").expect("repeat");
    let template = repeat.get("text").and_then(JsValue::as_str).expect("text");
    let count = as_u32(repeat.get("count").expect("count"));
    (0..count)
        .map(|index| template.replace("%d", &index.to_string()))
        .collect::<String>()
        .into_bytes()
}

fn strings(values: &[String]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}

/// One scenario as the capture reports it, minus its name.
fn run_scenario(scenario: &JsValue) -> JsValue {
    let cols = as_u32(scenario.get("cols").expect("cols"));
    let rows = as_u32(scenario.get("rows").expect("rows"));
    let mut terminal = Terminal::new(cols, rows);
    let events = register_handlers(&mut terminal);
    let titles = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = Rc::clone(&titles);
    terminal.on_title_change(move |title| sink.borrow_mut().push(title.to_owned()));

    let mut decoder = Utf8Decoder::new();
    let mut replies = Vec::new();
    let mut command_finished = Vec::new();
    for op in scenario
        .get("ops")
        .and_then(JsValue::as_array)
        .expect("ops")
    {
        if let Some([cols, rows]) = op.get("resize").and_then(JsValue::as_array) {
            terminal.resize(as_u32(cols), as_u32(rows)).expect("resize");
            continue;
        }
        let text = decoder.write(&op_bytes(op));
        if !text.is_empty() {
            terminal.write(&text).expect("write");
        }
        let queued = events.take();
        replies.extend(queued.replies);
        command_finished.extend(queued.command_finished);
    }

    let options = SnapshotOptions {
        scrollback_lines: None,
        include_wrap_flags: true,
    };
    let state = extract_state(&terminal, &options, None);
    let state =
        parse(&String::from_utf8(encode_terminal_snapshot(&state).expect("json")).expect("utf8"))
            .expect("state json");
    let mut object = JsObject::new();
    object.insert("state", state);
    object.insert(
        "lastOutputLines",
        strings(&last_output_lines(&terminal, EXIT_OUTPUT_LINE_LIMIT)),
    );
    object.insert("titles", strings(&titles.borrow()));
    object.insert("responses", strings(&replies));
    object.insert(
        "commandFinished",
        JsValue::Array(
            command_finished
                .into_iter()
                .map(|code| {
                    let mut entry = JsObject::new();
                    entry.insert("exitCode", code.map_or(JsValue::Null, JsValue::Number));
                    JsValue::Object(entry)
                })
                .collect(),
        ),
    );
    JsValue::Object(object)
}

#[test]
fn corpus_matches_the_pinned_xterm_capture() {
    let Some(pinned) = support::pinned("emulator corpus differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let paseo_root = pinned.terminal_dir.join("../../../../..");
    let bundle = std::fs::read(
        paseo_root.join("node_modules/@xterm/headless/lib-headless/xterm-headless.js"),
    )
    .expect("pinned xterm bundle");
    assert_eq!(
        support::sha256_hex(&bundle),
        XTERM_HEADLESS_SHA256,
        "@xterm/headless is not the pinned build"
    );

    let scripts = repo_root().join("scripts/phase4");
    let out =
        std::env::temp_dir().join(format!("spocky-xterm-capture-{}.json", std::process::id()));
    let output = support::run_node_file(
        &pinned,
        &scripts.join("terminal-emulator-capture.mjs"),
        &[
            "--paseo-root".as_ref(),
            paseo_root.as_os_str(),
            "--out".as_ref(),
            out.as_os_str(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let capture = parse(&std::fs::read_to_string(&out).expect("capture")).expect("capture json");
    let _ = std::fs::remove_file(&out);

    let corpus = parse(
        &std::fs::read_to_string(scripts.join("terminal-emulator-corpus.json")).expect("corpus"),
    )
    .expect("corpus json");
    let expected = capture
        .get("scenarios")
        .and_then(JsValue::as_array)
        .expect("scenarios");
    let scenarios = corpus
        .get("scenarios")
        .and_then(JsValue::as_array)
        .expect("scenarios");
    assert_eq!(expected.len(), scenarios.len());
    let mut failures = Vec::new();
    for (scenario, captured) in scenarios.iter().zip(expected) {
        let name = scenario
            .get("name")
            .and_then(JsValue::as_str)
            .expect("name");
        assert_eq!(captured.get("name").and_then(JsValue::as_str), Some(name));
        let mut want = JsObject::new();
        if let JsValue::Object(object) = captured {
            for (key, value) in object.iter().filter(|(key, _)| *key != "name") {
                want.insert(key, value.clone());
            }
        }
        if stringify(&run_scenario(scenario)) != stringify(&JsValue::Object(want)) {
            failures.push(name.to_owned());
        }
    }
    assert!(
        failures.is_empty(),
        "scenarios differ from pinned xterm: {failures:?}"
    );
}
