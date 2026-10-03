//! Terminal state extraction, following pinned
//! `packages/server/src/terminal/terminal.ts`: `extractCell`, `extractGrid`,
//! `extractScrollback`, the soft-wrap flags, `extractCursorState`, `getState`
//! and `extractLastOutputLines`, read from the [`spocky_xterm::Terminal`]
//! buffer the same way the baseline reads `@xterm/headless`.

use spocky_wire::{TerminalCell, TerminalCursor, TerminalCursorStyle, TerminalState};
use spocky_xterm::{CursorStyle, Terminal};

use crate::restore::SnapshotOptions;

/// `' '` cell the baseline returns when a line or cell is missing.
fn blank_cell() -> TerminalCell {
    TerminalCell::new(" ")
}

#[allow(clippy::cast_precision_loss)]
fn number(value: i64) -> f64 {
    value as f64
}

fn extract_cell(terminal: &Terminal, row: i64, col: i64) -> TerminalCell {
    let Some(cell) = terminal
        .buffer()
        .get_line(row)
        .and_then(|line| line.get_cell(col))
    else {
        return blank_cell();
    };
    // Color modes: 0 default, 1 16 colors, 2 256 colors, 3 RGB, in the upper
    // byte of the packed value.
    let fg_mode = cell.fg_color_mode() >> 24;
    let bg_mode = cell.bg_color_mode() >> 24;
    let chars = cell.chars();
    TerminalCell {
        char: if chars.is_empty() {
            " ".to_owned()
        } else {
            chars
        },
        fg: (fg_mode != 0).then(|| number(cell.fg_color())),
        bg: (bg_mode != 0).then(|| number(cell.bg_color())),
        fg_mode: (fg_mode != 0).then(|| f64::from(fg_mode)),
        bg_mode: (bg_mode != 0).then(|| f64::from(bg_mode)),
        bold: Some(cell.is_bold()),
        italic: Some(cell.is_italic()),
        underline: Some(cell.is_underline()),
        dim: Some(cell.is_dim()),
        inverse: Some(cell.is_inverse()),
        strikethrough: Some(cell.is_strikethrough()),
    }
}

fn extract_row(terminal: &Terminal, row: i64) -> Vec<TerminalCell> {
    (0..terminal.cols())
        .map(|col| extract_cell(terminal, row, col))
        .collect()
}

/// First scrollback row to include for `scrollbackLines`.
fn scrollback_start(base_y: i64, options: &SnapshotOptions) -> i64 {
    options
        .scrollback_lines
        .map_or(0, |lines| (base_y - i64::from(lines)).max(0))
}

/// xterm marks a line `isWrapped` when it continues the PREVIOUS line; the
/// snapshot carries the inverse, tmux-style flag: row y's flag is whether
/// line y+1 is a wrapped continuation.
fn line_continues_to_next(terminal: &Terminal, absolute_row: i64) -> bool {
    terminal
        .buffer()
        .get_line(absolute_row + 1)
        .is_some_and(|line| line.is_wrapped())
}

fn extract_cursor(terminal: &Terminal) -> TerminalCursor {
    let buffer = terminal.buffer();
    TerminalCursor {
        row: number(buffer.cursor_y()),
        col: number(buffer.cursor_x()),
        hidden: terminal.is_cursor_hidden().then_some(true),
        style: terminal.cursor_style().map(|style| match style {
            CursorStyle::Block => TerminalCursorStyle::Block,
            CursorStyle::Underline => TerminalCursorStyle::Underline,
            CursorStyle::Bar => TerminalCursorStyle::Bar,
        }),
        blink: terminal.cursor_blink(),
    }
}

/// `getState(snapshotOptions)`; `title` is the session title, omitted when
/// absent or empty.
#[must_use]
pub fn extract_state(
    terminal: &Terminal,
    options: &SnapshotOptions,
    title: Option<&str>,
) -> TerminalState {
    let base_y = terminal.buffer().base_y();
    let grid = (0..terminal.rows())
        .map(|row| extract_row(terminal, base_y + row))
        .collect();
    let start = scrollback_start(base_y, options);
    let scrollback = (start..base_y)
        .map(|row| extract_row(terminal, row))
        .collect();
    let (grid_wrapped, scrollback_wrapped) = if options.include_wrap_flags {
        (
            Some(
                (0..terminal.rows())
                    .map(|row| line_continues_to_next(terminal, base_y + row))
                    .collect(),
            ),
            Some(
                (start..base_y)
                    .map(|row| line_continues_to_next(terminal, row))
                    .collect(),
            ),
        )
    } else {
        (None, None)
    };
    TerminalState {
        rows: number(terminal.rows()),
        cols: number(terminal.cols()),
        grid,
        scrollback,
        cursor: extract_cursor(terminal),
        title: title.filter(|title| !title.is_empty()).map(str::to_owned),
        grid_wrapped,
        scrollback_wrapped,
    }
}

/// `extractLastOutputLines(terminal, limit)`: the buffer's logical lines
/// with soft-wrapped rows joined, blank edges dropped, last `limit` kept.
#[must_use]
pub fn last_output_lines(terminal: &Terminal, limit: usize) -> Vec<String> {
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
    let blank = |line: &String| {
        line.trim_matches(crate::process_title::is_js_whitespace)
            .is_empty()
    };
    while merged.first().is_some_and(blank) {
        merged.remove(0);
    }
    while merged.last().is_some_and(blank) {
        merged.pop();
    }
    let skip = merged.len().saturating_sub(limit);
    merged.split_off(skip)
}
