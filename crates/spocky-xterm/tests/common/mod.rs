//! Shared differential harness: runs a corpus through the pinned
//! `@xterm/headless` with `scripts/phase4/xterm-capture.mjs`, and through
//! `spocky-xterm` with the same handlers and extraction, as
//! `JSON.stringify` text per scenario.
//!
//! Needs `SPOCKY_PINNED_NODE` (Node 22.20.0). The pinned Paseo build root
//! defaults to the p3 slice harness checkout; `SPOCKY_XTERM_PASEO_ROOT`
//! overrides it. Without node the tests FAIL unless `SPOCKY_ALLOW_SKIP=1`
//! (exactly).

use std::cell::RefCell;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_xterm::{CellView, CursorStyle, Param, Terminal, Utf8Decoder};

const DEFAULT_PASEO_ROOT: &str = "/private/tmp/spocky-targets/p3_slice_harness/paseo-original-5de45e208690b0efc51c59a585ae9729325a9204";
const XTERM_HEADLESS_JS: &str = "node_modules/@xterm/headless/lib-headless/xterm-headless.js";
const XTERM_HEADLESS_SHA256: &str =
    "17a90b650cf6b77cce2b98c4063884d43545e4ce177a54b76ccfc906f1aacaed";
const EXIT_OUTPUT_LINE_LIMIT: usize = 12;
const OSC_COLOR_QUERY_RESPONSES: [(u32, &str); 3] = [
    (10, "rgb:e6e6/e6e6/e6e6"),
    (11, "rgb:0b0b/0b0b/0b0b"),
    (12, "rgb:e6e6/e6e6/e6e6"),
];

/// The pinned node binary and Paseo build root, or `None` when skipping.
pub fn pinned() -> Option<(OsString, PathBuf)> {
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        assert!(
            std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1"),
            "set SPOCKY_PINNED_NODE (or SPOCKY_ALLOW_SKIP=1)"
        );
        eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: xterm differential not run");
        return None;
    };
    let root = std::env::var_os("SPOCKY_XTERM_PASEO_ROOT")
        .map_or_else(|| PathBuf::from(DEFAULT_PASEO_ROOT), PathBuf::from);
    assert_eq!(
        sha256_hex(&std::fs::read(root.join(XTERM_HEADLESS_JS)).expect("pinned xterm-headless.js")),
        XTERM_HEADLESS_SHA256,
        "{XTERM_HEADLESS_JS} is not the pinned build"
    );
    Some((node, root))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

pub fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Runs the capture script over `corpus` and returns one line per scenario.
pub fn capture(
    node: &OsString,
    paseo_root: &Path,
    corpus: &Path,
    out: &Path,
    timeout_secs: u32,
) -> Vec<String> {
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", &timeout_secs.to_string()])
        .arg(node)
        .arg(repo_path("scripts/phase4/xterm-capture.mjs"))
        .arg("--paseo-root")
        .arg(paseo_root)
        .arg("--corpus")
        .arg(corpus)
        .arg("--out")
        .arg(out)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(out).expect("capture output");
    text.lines().skip(1).map(str::to_owned).collect()
}

fn number(value: i64) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    JsValue::Number(value as f64)
}

fn string(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// Whether JavaScript's `String.prototype.trim` leaves nothing.
fn is_js_blank(text: &str) -> bool {
    js_trim(text).is_empty()
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(|character| {
        matches!(
            character,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

fn cell_of(cell: &CellView) -> JsValue {
    let fg_mode = cell.fg_color_mode() >> 24;
    let bg_mode = cell.bg_color_mode() >> 24;
    let chars = cell.chars();
    let mut object = JsObject::new();
    object.insert("char", string(if chars.is_empty() { " " } else { &chars }));
    let optional = |mode: u32, value: i64| {
        if mode == 0 {
            JsValue::Undefined
        } else {
            number(value)
        }
    };
    object.insert("fg", optional(fg_mode, cell.fg_color()));
    object.insert("bg", optional(bg_mode, cell.bg_color()));
    object.insert("fgMode", optional(fg_mode, i64::from(fg_mode)));
    object.insert("bgMode", optional(bg_mode, i64::from(bg_mode)));
    object.insert("bold", JsValue::Bool(cell.is_bold()));
    object.insert("italic", JsValue::Bool(cell.is_italic()));
    object.insert("underline", JsValue::Bool(cell.is_underline()));
    object.insert("dim", JsValue::Bool(cell.is_dim()));
    object.insert("inverse", JsValue::Bool(cell.is_inverse()));
    object.insert("strikethrough", JsValue::Bool(cell.is_strikethrough()));
    JsValue::Object(object)
}

fn extract_row(terminal: &Terminal, row: i64) -> JsValue {
    let line = terminal.buffer().get_line(row);
    let cells = (0..terminal.cols())
        .map(|col| {
            if let Some(cell) = line.as_ref().and_then(|line| line.get_cell(col)) {
                cell_of(&cell)
            } else {
                let mut object = JsObject::new();
                object.insert("char", string(" "));
                object.insert("fg", JsValue::Undefined);
                object.insert("bg", JsValue::Undefined);
                JsValue::Object(object)
            }
        })
        .collect();
    JsValue::Array(cells)
}

fn continues_to_next(terminal: &Terminal, row: i64) -> JsValue {
    JsValue::Bool(
        terminal
            .buffer()
            .get_line(row + 1)
            .is_some_and(|line| line.is_wrapped()),
    )
}

fn extract_cursor(terminal: &Terminal) -> JsValue {
    let mut cursor = JsObject::new();
    cursor.insert("row", number(terminal.buffer().cursor_y()));
    cursor.insert("col", number(terminal.buffer().cursor_x()));
    if terminal.is_cursor_hidden() {
        cursor.insert("hidden", JsValue::Bool(true));
    }
    if let Some(style) = terminal.cursor_style() {
        let name = match style {
            CursorStyle::Block => "block",
            CursorStyle::Underline => "underline",
            CursorStyle::Bar => "bar",
        };
        cursor.insert("style", string(name));
    }
    if let Some(blink) = terminal.cursor_blink() {
        cursor.insert("blink", JsValue::Bool(blink));
    }
    JsValue::Object(cursor)
}

fn extract_state(terminal: &Terminal) -> JsValue {
    let base_y = terminal.buffer().base_y();
    let grid = (0..terminal.rows())
        .map(|row| extract_row(terminal, base_y + row))
        .collect();
    let grid_wrapped = (0..terminal.rows())
        .map(|row| continues_to_next(terminal, base_y + row))
        .collect();
    let scrollback = (0..base_y).map(|row| extract_row(terminal, row)).collect();
    let scrollback_wrapped = (0..base_y)
        .map(|row| continues_to_next(terminal, row))
        .collect();
    let mut state = JsObject::new();
    state.insert("rows", number(terminal.rows()));
    state.insert("cols", number(terminal.cols()));
    state.insert("grid", JsValue::Array(grid));
    state.insert("scrollback", JsValue::Array(scrollback));
    state.insert("cursor", extract_cursor(terminal));
    state.insert("gridWrapped", JsValue::Array(grid_wrapped));
    state.insert("scrollbackWrapped", JsValue::Array(scrollback_wrapped));
    JsValue::Object(state)
}

fn extract_last_output_lines(terminal: &Terminal) -> JsValue {
    let buffer = terminal.buffer();
    let mut merged: Vec<String> = Vec::new();
    for row in 0..buffer.length() {
        let Some(line) = buffer.get_line(row) else {
            continue;
        };
        let text = line.translate_to_string(true);
        if line.is_wrapped()
            && let Some(last) = merged.last_mut()
        {
            last.push_str(&text);
            continue;
        }
        merged.push(text);
    }
    let start = merged
        .iter()
        .position(|line| !is_js_blank(line))
        .unwrap_or(merged.len());
    let end = merged
        .iter()
        .rposition(|line| !is_js_blank(line))
        .map_or(start, |index| index + 1);
    let kept = &merged[start..end.max(start)];
    let tail = &kept[kept.len().saturating_sub(EXIT_OUTPUT_LINE_LIMIT)..];
    JsValue::Array(tail.iter().map(|line| string(line)).collect())
}

fn op_bytes(op: &JsValue) -> Option<Vec<u8>> {
    if let Some(text) = op.get("text").and_then(JsValue::as_str) {
        return Some(text.as_bytes().to_vec());
    }
    if let Some(hex) = op.get("hex").and_then(JsValue::as_str) {
        return Some(
            (0..hex.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex"))
                .collect(),
        );
    }
    let repeat = op.get("repeat")?;
    let text = repeat
        .get("text")
        .and_then(JsValue::as_str)
        .expect("repeat text");
    let count = as_u32(repeat.get("count").expect("repeat count"));
    let mut out = String::new();
    for index in 0..count {
        out.push_str(&text.replace("%d", &index.to_string()));
    }
    Some(out.into_bytes())
}

fn as_u32(value: &JsValue) -> u32 {
    match value {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        JsValue::Number(number) => *number as u32,
        _ => panic!("expected a number"),
    }
}

/// `/^-?\d+$/` without the `u` flag: ASCII digits only.
fn is_integer_text(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

/// Replays one corpus scenario through `spocky-xterm` with the capture
/// script's handlers and extraction, as `JSON.stringify` text.
#[allow(clippy::too_many_lines)]
pub fn replay(scenario: &JsValue) -> String {
    let rows = as_u32(scenario.get("rows").expect("rows"));
    let cols = as_u32(scenario.get("cols").expect("cols"));
    let mut terminal = Terminal::new(cols, rows);
    let responses: Rc<RefCell<Vec<String>>> = Rc::default();
    let titles: Rc<RefCell<Vec<String>>> = Rc::default();
    let command_finished: Rc<RefCell<Vec<JsValue>>> = Rc::default();

    let sink = Rc::clone(&responses);
    terminal
        .register_csi_handler(None, b'c', move |params, _| {
            if params.is_empty() || (params.len() == 1 && params[0] == Param::Value(0)) {
                sink.borrow_mut().push("\x1b[?62;4;22c".to_owned());
                return true;
            }
            false
        })
        .expect("csi c");
    let sink = Rc::clone(&responses);
    terminal
        .register_csi_handler(None, b'n', move |params, cursor| {
            if params.len() != 1 {
                return false;
            }
            if params[0] == Param::Value(5) {
                sink.borrow_mut().push("\x1b[0n".to_owned());
                return true;
            }
            if params[0] == Param::Value(6) {
                sink.borrow_mut()
                    .push(format!("\x1b[{};{}R", cursor.y + 1, cursor.x + 1));
                return true;
            }
            false
        })
        .expect("csi n");
    let sink = Rc::clone(&responses);
    terminal
        .register_csi_handler(Some(b'?'), b'n', move |params, cursor| {
            if params.len() != 1 || params[0] != Param::Value(6) {
                return false;
            }
            sink.borrow_mut()
                .push(format!("\x1b[?{};{}R", cursor.y + 1, cursor.x + 1));
            true
        })
        .expect("csi ?n");
    for (code, response) in OSC_COLOR_QUERY_RESPONSES {
        let sink = Rc::clone(&responses);
        terminal.register_osc_handler(code, move |data| {
            if js_trim(data) != "?" {
                return false;
            }
            sink.borrow_mut()
                .push(format!("\x1b]{code};{response}\x1b\\"));
            true
        });
    }
    let sink = Rc::clone(&titles);
    terminal.on_title_change(move |title| sink.borrow_mut().push(title.to_owned()));
    let sink = Rc::clone(&command_finished);
    terminal.register_osc_handler(633, move |data| {
        let parts: Vec<&str> = data.split(';').collect();
        let mut finished = JsObject::new();
        if parts[0] == "D" && parts.len() == 1 {
            finished.insert("exitCode", JsValue::Null);
            sink.borrow_mut().push(JsValue::Object(finished));
        } else if parts[0] == "D" && parts.len() == 2 && is_integer_text(parts[1]) {
            finished.insert(
                "exitCode",
                JsValue::Number(parts[1].parse().expect("number")),
            );
            sink.borrow_mut().push(JsValue::Object(finished));
        }
        true
    });

    let mut wedged = false;
    let mut resize_errors = 0;
    let mut decoder = Utf8Decoder::new();
    for op in scenario
        .get("ops")
        .and_then(JsValue::as_array)
        .expect("ops")
    {
        if let Some([cols, rows]) = op.get("resize").and_then(JsValue::as_array) {
            if terminal.resize(as_u32(cols), as_u32(rows)).is_err() {
                resize_errors += 1;
            }
            continue;
        }
        let text = decoder.write(&op_bytes(op).expect("known op"));
        if !text.is_empty() && !wedged && terminal.write(&text).is_err() {
            wedged = true;
        }
    }
    if !wedged && terminal.write("").is_err() {
        wedged = true;
    }

    let mut result = JsObject::new();
    result.insert("name", scenario.get("name").expect("name").clone());
    result.insert("state", extract_state(&terminal));
    result.insert("lastOutputLines", extract_last_output_lines(&terminal));
    result.insert(
        "titles",
        JsValue::Array(titles.borrow().iter().map(|title| string(title)).collect()),
    );
    result.insert(
        "responses",
        JsValue::Array(
            responses
                .borrow()
                .iter()
                .map(|response| string(response))
                .collect(),
        ),
    );
    result.insert(
        "commandFinished",
        JsValue::Array(command_finished.borrow().clone()),
    );
    if wedged {
        result.insert("wedged", JsValue::Bool(true));
    }
    if resize_errors > 0 {
        result.insert("resizeErrors", number(resize_errors));
    }
    stringify(&JsValue::Object(result))
}

/// The first byte offset where two texts differ, with some context.
pub fn first_difference(expected: &str, actual: &str) -> String {
    let offset = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| expected.len().min(actual.len()));
    let window = |text: &str| {
        let start = text.floor_char_boundary(offset.saturating_sub(120));
        let end = text.ceil_char_boundary((offset + 120).min(text.len()));
        text[start..end].to_owned()
    };
    format!(
        "first difference at byte {offset}\n  xterm: ...{}...\n  rust:  ...{}...",
        window(expected),
        window(actual)
    )
}
