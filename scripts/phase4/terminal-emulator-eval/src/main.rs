//! Replays the terminal emulator corpus through a candidate Rust emulator and
//! compares every observable output with the pinned @xterm/headless capture.
//!
//! Usage:
//!   cargo run --offline --release -- <corpus.json> <xterm-capture.json> <report.json>
//!
//! The candidate state is built in the exact shape the capture uses (the
//! pinned `getState({ includeWrapFlags: true })` cell grid), then compared by
//! `JSON.stringify` text per cell, per row flag, and per section.

use std::fmt::Write as _;
use std::process::ExitCode;

use spocky_contracts::js_value::{self, JsObject, JsValue};

const SCROLLBACK: usize = 1000;
const EXIT_LINE_LIMIT: usize = 12;

#[derive(Default)]
struct Events {
    titles: Vec<String>,
    responses: Vec<String>,
    command_finished: Vec<Option<i64>>,
}

impl vt100::Callbacks for Events {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.titles.push(String::from_utf8_lossy(title).into_owned());
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied();
        let (row, col) = screen.cursor_position();
        match (i1, c) {
            (None, 'c') if params.is_empty() || (params.len() == 1 && first == Some(0)) => {
                self.responses.push("\x1b[?62;4;22c".to_owned());
            }
            (None, 'n') if params.len() == 1 && first == Some(5) => {
                self.responses.push("\x1b[0n".to_owned());
            }
            (None, 'n') if params.len() == 1 && first == Some(6) => {
                self.responses.push(format!("\x1b[{};{}R", row + 1, col + 1));
            }
            (Some(b'?'), 'n') if params.len() == 1 && first == Some(6) => {
                self.responses.push(format!("\x1b[?{};{}R", row + 1, col + 1));
            }
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let Some(code) = params.first() else { return };
        let rest: Vec<String> = params[1..]
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        let response = match *code {
            b"10" => Some("rgb:e6e6/e6e6/e6e6"),
            b"11" => Some("rgb:0b0b/0b0b/0b0b"),
            b"12" => Some("rgb:e6e6/e6e6/e6e6"),
            _ => None,
        };
        if let Some(response) = response {
            if rest.join(";").trim() == "?" {
                let code = String::from_utf8_lossy(code);
                self.responses.push(format!("\x1b]{code};{response}\x1b\\"));
            }
            return;
        }
        if *code == b"633" && rest.first().map(String::as_str) == Some("D") {
            match rest.len() {
                1 => self.command_finished.push(None),
                2 => {
                    if let Ok(code) = rest[1].parse::<i64>() {
                        self.command_finished.push(Some(code));
                    }
                }
                _ => {}
            }
        }
    }
}

fn num(value: f64) -> JsValue {
    JsValue::Number(value)
}

fn color(color: vt100::Color) -> Option<(f64, f64)> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) if index < 16 => Some((f64::from(index), 1.0)),
        vt100::Color::Idx(index) => Some((f64::from(index), 2.0)),
        vt100::Color::Rgb(r, g, b) => Some((
            f64::from((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)),
            3.0,
        )),
    }
}

fn cell_value(cell: Option<&vt100::Cell>) -> JsValue {
    let mut object = JsObject::new();
    let Some(cell) = cell else {
        object.insert("char", JsValue::String(" ".to_owned()));
        return JsValue::Object(object);
    };
    let text = cell.contents();
    object.insert(
        "char",
        JsValue::String(if text.is_empty() { " " } else { text }.to_owned()),
    );
    let fg = color(cell.fgcolor());
    let bg = color(cell.bgcolor());
    if let Some((value, _)) = fg {
        object.insert("fg", num(value));
    }
    if let Some((value, _)) = bg {
        object.insert("bg", num(value));
    }
    if let Some((_, mode)) = fg {
        object.insert("fgMode", num(mode));
    }
    if let Some((_, mode)) = bg {
        object.insert("bgMode", num(mode));
    }
    object.insert("bold", JsValue::Bool(cell.bold()));
    object.insert("italic", JsValue::Bool(cell.italic()));
    object.insert("underline", JsValue::Bool(cell.underline()));
    object.insert("dim", JsValue::Bool(cell.dim()));
    object.insert("inverse", JsValue::Bool(cell.inverse()));
    object.insert("strikethrough", JsValue::Bool(false));
    JsValue::Object(object)
}

struct RowText {
    text: String,
    wraps_to_next: bool,
}

fn visible_row(screen: &vt100::Screen, row: u16, cols: u16) -> (JsValue, RowText) {
    // xterm translateToString(true) trims only trailing cells without content;
    // a written space keeps its column.
    let mut cells = Vec::new();
    let mut text = String::new();
    let mut trimmed_len = 0;
    for col in 0..cols {
        let cell = screen.cell(row, col);
        cells.push(cell_value(cell));
        match cell {
            Some(cell) if cell.is_wide_continuation() => {}
            Some(cell) if cell.has_contents() => {
                text.push_str(cell.contents());
                trimmed_len = text.len();
            }
            _ => text.push(' '),
        }
    }
    text.truncate(trimmed_len);
    let trimmed = text;
    (
        JsValue::Array(cells),
        RowText {
            text: trimmed,
            wraps_to_next: screen.row_wrapped(row),
        },
    )
}

fn last_output_lines(rows: &[RowText]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let continuation = index > 0 && rows[index - 1].wraps_to_next;
        if continuation && let Some(last) = merged.last_mut() {
            last.push_str(&row.text);
            continue;
        }
        merged.push(row.text.clone());
    }
    while merged.first().is_some_and(|line| line.trim().is_empty()) {
        merged.remove(0);
    }
    while merged.last().is_some_and(|line| line.trim().is_empty()) {
        merged.pop();
    }
    let skip = merged.len().saturating_sub(EXIT_LINE_LIMIT);
    merged.split_off(skip)
}

fn op_bytes(op: &JsValue) -> Option<Vec<u8>> {
    if let Some(text) = op.get("text").and_then(JsValue::as_str) {
        return Some(text.as_bytes().to_vec());
    }
    if let Some(hex) = op.get("hex").and_then(JsValue::as_str) {
        return Some(
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
                .collect(),
        );
    }
    if let Some(repeat) = op.get("repeat") {
        let template = repeat.get("text").and_then(JsValue::as_str).expect("repeat text");
        let count = as_usize(repeat.get("count").expect("repeat count"));
        let mut text = String::new();
        for index in 0..count {
            text.push_str(&template.replace("%d", &index.to_string()));
        }
        return Some(text.into_bytes());
    }
    None
}

fn as_usize(value: &JsValue) -> usize {
    match value {
        JsValue::Number(number) if *number >= 0.0 && number.fract() == 0.0 => {
            format!("{number}").parse().expect("integer")
        }
        _ => panic!("expected a non-negative integer"),
    }
}

fn as_u16(value: &JsValue) -> u16 {
    u16::try_from(as_usize(value)).expect("u16")
}

fn strings(values: &[String]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}

fn run_vt100(scenario: &JsValue) -> JsObject {
    let rows = as_u16(scenario.get("rows").expect("rows"));
    let cols = as_u16(scenario.get("cols").expect("cols"));
    let mut parser = vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, Events::default());
    for op in scenario.get("ops").and_then(JsValue::as_array).expect("ops") {
        if let Some(resize) = op.get("resize").and_then(JsValue::as_array) {
            parser
                .screen_mut()
                .set_size(as_u16(&resize[1]), as_u16(&resize[0]));
            continue;
        }
        parser.process(&op_bytes(op).expect("known op"));
    }

    let (rows, cols) = parser.screen().size();
    parser.screen_mut().set_scrollback(usize::MAX);
    let scrollback_len = parser.screen().scrollback();
    let mut scrollback = Vec::new();
    let mut scrollback_wrapped = Vec::new();
    let mut texts = Vec::new();
    for line in 0..scrollback_len {
        parser.screen_mut().set_scrollback(scrollback_len - line);
        let (cells, text) = visible_row(parser.screen(), 0, cols);
        scrollback.push(cells);
        scrollback_wrapped.push(JsValue::Bool(text.wraps_to_next));
        texts.push(text);
    }
    parser.screen_mut().set_scrollback(0);
    let screen = parser.screen();
    let mut grid = Vec::new();
    let mut grid_wrapped = Vec::new();
    for row in 0..rows {
        let (cells, text) = visible_row(screen, row, cols);
        grid.push(cells);
        grid_wrapped.push(JsValue::Bool(text.wraps_to_next));
        texts.push(text);
    }
    let (cursor_row, cursor_col) = screen.cursor_position();
    let mut cursor = JsObject::new();
    cursor.insert("row", num(f64::from(cursor_row)));
    cursor.insert("col", num(f64::from(cursor_col)));
    if screen.hide_cursor() {
        cursor.insert("hidden", JsValue::Bool(true));
    }

    let mut state = JsObject::new();
    state.insert("rows", num(f64::from(rows)));
    state.insert("cols", num(f64::from(cols)));
    state.insert("grid", JsValue::Array(grid));
    state.insert("scrollback", JsValue::Array(scrollback));
    state.insert("cursor", JsValue::Object(cursor));
    state.insert("gridWrapped", JsValue::Array(grid_wrapped));
    state.insert("scrollbackWrapped", JsValue::Array(scrollback_wrapped));

    let events = parser.callbacks();
    let mut result = JsObject::new();
    result.insert("state", JsValue::Object(state));
    result.insert("lastOutputLines", strings(&last_output_lines(&texts)));
    result.insert("titles", strings(&events.titles));
    result.insert("responses", strings(&events.responses));
    result.insert(
        "commandFinished",
        JsValue::Array(
            events
                .command_finished
                .iter()
                .map(|code| {
                    let mut object = JsObject::new();
                    #[allow(clippy::cast_precision_loss)]
                    let value = code.map_or(JsValue::Null, |code| num(code as f64));
                    object.insert("exitCode", value);
                    JsValue::Object(object)
                })
                .collect(),
        ),
    );
    result
}

/// Counts equal entries of two arrays of cells (rows of cells when nested).
fn count_cells(expected: Option<&JsValue>, actual: Option<&JsValue>) -> (usize, usize) {
    let expected_rows = expected.and_then(JsValue::as_array).unwrap_or(&[]);
    let actual_rows = actual.and_then(JsValue::as_array).unwrap_or(&[]);
    let mut matched = 0;
    let mut total = 0;
    for (index, row) in expected_rows.iter().enumerate() {
        let cells = row.as_array().unwrap_or(&[]);
        let other = actual_rows.get(index).and_then(JsValue::as_array).unwrap_or(&[]);
        for (col, cell) in cells.iter().enumerate() {
            total += 1;
            if other
                .get(col)
                .is_some_and(|candidate| js_value::stringify(candidate) == js_value::stringify(cell))
            {
                matched += 1;
            }
        }
    }
    (matched, total)
}

fn same(expected: Option<&JsValue>, actual: Option<&JsValue>) -> bool {
    match (expected, actual) {
        (Some(a), Some(b)) => js_value::stringify(a) == js_value::stringify(b),
        (None, None) => true,
        _ => false,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: eval <corpus.json> <xterm-capture.json> <report.json>");
        return ExitCode::from(2);
    }
    let corpus = js_value::parse(&std::fs::read_to_string(&args[1]).expect("corpus")).expect("corpus json");
    let capture = js_value::parse(&std::fs::read_to_string(&args[2]).expect("capture")).expect("capture json");
    let expected_by_name = capture.get("scenarios").and_then(JsValue::as_array).expect("scenarios");

    let mut table = String::new();
    let mut report = Vec::new();
    let mut full_matches = 0;
    let scenarios = corpus.get("scenarios").and_then(JsValue::as_array).expect("scenarios");
    for scenario in scenarios {
        let name = scenario.get("name").and_then(JsValue::as_str).expect("name");
        let expected = expected_by_name
            .iter()
            .find(|entry| entry.get("name").and_then(JsValue::as_str) == Some(name))
            .expect("captured scenario");
        let actual = JsValue::Object(run_vt100(scenario));
        let mut expected_without_name = JsObject::new();
        if let JsValue::Object(object) = expected {
            for (key, value) in object.iter() {
                if key != "name" {
                    expected_without_name.insert(key, value.clone());
                }
            }
        }
        let full = js_value::stringify(&JsValue::Object(expected_without_name)) == js_value::stringify(&actual);
        if full {
            full_matches += 1;
        }
        let es = expected.get("state");
        let acs = actual.get("state");
        let (grid_ok, grid_total) = count_cells(es.and_then(|s| s.get("grid")), acs.and_then(|s| s.get("grid")));
        let (sb_ok, sb_total) = count_cells(
            es.and_then(|s| s.get("scrollback")),
            acs.and_then(|s| s.get("scrollback")),
        );
        let checks = [
            ("cursor", same(es.and_then(|s| s.get("cursor")), acs.and_then(|s| s.get("cursor")))),
            ("gridWrapped", same(es.and_then(|s| s.get("gridWrapped")), acs.and_then(|s| s.get("gridWrapped")))),
            (
                "scrollbackWrapped",
                same(es.and_then(|s| s.get("scrollbackWrapped")), acs.and_then(|s| s.get("scrollbackWrapped"))),
            ),
            ("lastOutputLines", same(expected.get("lastOutputLines"), actual.get("lastOutputLines"))),
            ("titles", same(expected.get("titles"), actual.get("titles"))),
            ("responses", same(expected.get("responses"), actual.get("responses"))),
            ("commandFinished", same(expected.get("commandFinished"), actual.get("commandFinished"))),
        ];
        let passed = checks.iter().filter(|(_, ok)| *ok).count();
        let failed: Vec<&str> = checks.iter().filter(|(_, ok)| !*ok).map(|(key, _)| *key).collect();
        writeln!(
            table,
            "| {name} | {} | {grid_ok}/{grid_total} | {sb_ok}/{sb_total} | {passed}/{} | {} |",
            if full { "yes" } else { "no" },
            checks.len(),
            if failed.is_empty() { "-".to_owned() } else { failed.join(", ") },
        )
        .expect("write");

        let mut entry = JsObject::new();
        entry.insert("name", JsValue::String(name.to_owned()));
        entry.insert("fullMatch", JsValue::Bool(full));
        #[allow(clippy::cast_precision_loss)]
        {
            entry.insert("gridCellsMatched", num(grid_ok as f64));
            entry.insert("gridCells", num(grid_total as f64));
            entry.insert("scrollbackCellsMatched", num(sb_ok as f64));
            entry.insert("scrollbackCells", num(sb_total as f64));
            entry.insert("checksPassed", num(passed as f64));
            entry.insert("checks", num(checks.len() as f64));
        }
        entry.insert("failedChecks", strings(&failed.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()));
        entry.insert("candidate", actual);
        report.push(JsValue::Object(entry));
    }

    let mut out = JsObject::new();
    out.insert("candidate", JsValue::String("vt100 0.16.2".to_owned()));
    #[allow(clippy::cast_precision_loss)]
    out.insert("fullMatches", num(f64::from(u32::try_from(full_matches).expect("count"))));
    out.insert("scenarios", JsValue::Array(report));
    std::fs::write(&args[3], format!("{}\n", js_value::stringify(&JsValue::Object(out)))).expect("report");

    println!("| scenario | full match | grid cells | scrollback cells | checks | failed checks |");
    println!("|---|---|---|---|---|---|");
    print!("{table}");
    println!("full matches: {full_matches}/{}", scenarios.len());
    ExitCode::SUCCESS
}
