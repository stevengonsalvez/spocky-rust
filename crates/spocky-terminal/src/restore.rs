//! Terminal restore policy and snapshot frames, following pinned
//! `packages/server/src/terminal/terminal-restore.ts` and the
//! `renderTerminalSnapshotToAnsi` it calls from
//! `packages/protocol/src/terminal-snapshot.ts`.

use std::fmt::Write as _;

use spocky_contracts::js_value::js_number;
use spocky_wire::{
    TerminalCell, TerminalCursor, TerminalCursorStyle, TerminalOpcode, TerminalState,
    encode_terminal_frame, encode_terminal_snapshot,
};

/// Output a stream may fall behind before the snapshot catch-up is considered.
pub const MAX_TERMINAL_OUTPUT_FRAME_BYTES: usize = 256 * 1024;

/// Client transport backlog that, with the output threshold, forces catch-up.
pub const MAX_CLIENT_BUFFERED_BYTES: usize = 4 * 1024 * 1024;

const DEFAULT_VISIBLE_RESTORE_SCROLLBACK_LINES: u32 = 200;
const MAX_VISIBLE_RESTORE_SCROLLBACK_LINES: u32 = 500;

/// `restore.mode` of a `subscribe_terminal_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    Live,
    VisibleSnapshot,
    FullSnapshot,
}

/// `restore` of a `subscribe_terminal_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreOptions {
    pub mode: RestoreMode,
    pub scrollback_lines: Option<u32>,
    /// `(rows, cols)`.
    pub size: Option<(u16, u16)>,
}

impl RestoreOptions {
    #[must_use]
    pub fn new(mode: RestoreMode) -> Self {
        Self {
            mode,
            scrollback_lines: None,
            size: None,
        }
    }
}

/// `initialSnapshot` of a terminal subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    State,
    Ready,
}

/// `TerminalStateSnapshotOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotOptions {
    pub scrollback_lines: Option<u32>,
    pub include_wrap_flags: bool,
}

/// What `resolveTerminalRestoreSnapshotOptions` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSnapshot {
    /// `null`: live restore sends no snapshot.
    None,
    /// `undefined`: a full snapshot with default options.
    Full,
    /// Options for a bounded snapshot.
    Options(SnapshotOptions),
}

/// `resolveTerminalSubscriptionSnapshotMode`.
#[must_use]
pub fn subscription_snapshot_mode(restore: Option<&RestoreOptions>) -> SnapshotMode {
    if restore.is_some() {
        SnapshotMode::Ready
    } else {
        SnapshotMode::State
    }
}

/// `resolveRestoreAfterOutputOverflow`: live restore becomes a bare
/// visible-snapshot restore; other restores are kept as they are.
#[must_use]
pub fn restore_after_output_overflow(restore: Option<RestoreOptions>) -> Option<RestoreOptions> {
    match restore {
        Some(RestoreOptions {
            mode: RestoreMode::Live,
            ..
        }) => Some(RestoreOptions::new(RestoreMode::VisibleSnapshot)),
        other => other,
    }
}

/// `resolveTerminalRestoreSnapshotOptions`.
#[must_use]
pub fn restore_snapshot_options(restore: &RestoreOptions) -> RestoreSnapshot {
    match restore.mode {
        RestoreMode::Live => RestoreSnapshot::None,
        RestoreMode::VisibleSnapshot => RestoreSnapshot::Options(SnapshotOptions {
            scrollback_lines: Some(
                restore
                    .scrollback_lines
                    .map_or(DEFAULT_VISIBLE_RESTORE_SCROLLBACK_LINES, |lines| {
                        lines.min(MAX_VISIBLE_RESTORE_SCROLLBACK_LINES)
                    }),
            ),
            include_wrap_flags: false,
        }),
        RestoreMode::FullSnapshot => RestoreSnapshot::Full,
    }
}

/// `encodeLegacyTerminalSnapshotFrame`: the JSON state in a snapshot frame.
///
/// # Errors
///
/// Returns the JSON error for a state with a non-finite number.
pub fn encode_legacy_snapshot_frame(
    slot: u8,
    state: &TerminalState,
) -> Result<Vec<u8>, serde_json::Error> {
    Ok(encode_terminal_frame(
        TerminalOpcode::Snapshot,
        slot,
        &encode_terminal_snapshot(state)?,
    ))
}

/// `encodeTerminalRestoreFrame`: the state replayed as ANSI in a restore frame.
#[must_use]
pub fn encode_restore_frame(slot: u8, state: &TerminalState) -> Vec<u8> {
    encode_terminal_frame(
        TerminalOpcode::Restore,
        slot,
        render_snapshot_to_ansi(state).as_bytes(),
    )
}

/// `TerminalStyle` of the baseline renderer, one flag per SGR attribute.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq)]
struct Style {
    fg: Option<f64>,
    bg: Option<f64>,
    fg_mode: Option<f64>,
    bg_mode: Option<f64>,
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
    inverse: bool,
    strikethrough: bool,
}

const DEFAULT_STYLE: Style = Style {
    fg: None,
    bg: None,
    fg_mode: None,
    bg_mode: None,
    bold: false,
    italic: false,
    underline: false,
    dim: false,
    inverse: false,
    strikethrough: false,
};

/// `renderTerminalSnapshotToAnsi`.
#[must_use]
pub fn render_snapshot_to_ansi(state: &TerminalState) -> String {
    let rows: Vec<&Vec<TerminalCell>> = state.scrollback.iter().chain(&state.grid).collect();
    let wrap_flags: Vec<bool> = state
        .scrollback_wrapped
        .iter()
        .flatten()
        .chain(state.grid_wrapped.iter().flatten())
        .copied()
        .collect();
    // Soft-wrapped rows replay as one unbroken logical line so the client
    // marks them wrapped and can reflow them; without flags (old daemon) rows
    // replay verbatim with autowrap off and a hard newline per row.
    let has_wrap_info = wrap_flags.len() == rows.len();
    let mut out = String::new();
    if !has_wrap_info {
        out.push_str("\u{1b}[?7l");
    }
    for (index, row) in rows.iter().enumerate() {
        let continues = has_wrap_info && wrap_flags[index];
        render_row(&mut out, row, continues.then_some(state.cols));
        if index + 1 < rows.len() && !continues {
            out.push_str("\r\n");
        }
    }
    out.push_str("\u{1b}[0m");
    if let Some(presentation) = cursor_presentation(&state.cursor) {
        out.push_str(&presentation);
    }
    let _ = write!(
        out,
        "\u{1b}[{};{}H",
        js_number(state.cursor.row + 1.0),
        js_number(state.cursor.col + 1.0)
    );
    out.push_str(if state.cursor.hidden == Some(true) {
        "\u{1b}[?25l"
    } else {
        "\u{1b}[?25h"
    });
    if !has_wrap_info {
        out.push_str("\u{1b}[?7h");
    }
    out
}

fn cursor_presentation(cursor: &TerminalCursor) -> Option<String> {
    let steady = cursor.blink == Some(false);
    let code = match cursor.style? {
        TerminalCursorStyle::Block => {
            if steady {
                2
            } else {
                1
            }
        }
        TerminalCursorStyle::Underline => {
            if steady {
                4
            } else {
                3
            }
        }
        TerminalCursorStyle::Bar => {
            if steady {
                6
            } else {
                5
            }
        }
    };
    Some(format!("\u{1b}[{code} q"))
}

fn render_row(out: &mut String, row: &[TerminalCell], pad_to_cols: Option<f64>) {
    let content = content_length(row);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let length = match pad_to_cols {
        // `Math.max(contentLength, padToCols)` loops to the ceiling of a
        // fractional column count; `cols` is a positive integer in practice.
        Some(cols) if cols > content as f64 => cols.ceil() as usize,
        _ => content,
    };
    let blank = TerminalCell::new(" ");
    let mut previous = DEFAULT_STYLE;
    for index in 0..length {
        let cell = row.get(index).unwrap_or(&blank);
        let next = style_of(cell);
        if previous != next {
            out.push_str(&style_to_ansi(&next));
            previous = next;
        }
        out.push_str(if cell.char.is_empty() {
            " "
        } else {
            &cell.char
        });
    }
    if previous != DEFAULT_STYLE {
        out.push_str("\u{1b}[0m");
    }
}

fn content_length(row: &[TerminalCell]) -> usize {
    for (index, cell) in row.iter().enumerate().rev() {
        let styled = cell.fg.is_some()
            || cell.bg.is_some()
            || cell.fg_mode.is_some()
            || cell.bg_mode.is_some()
            || cell.bold == Some(true)
            || cell.italic == Some(true)
            || cell.underline == Some(true)
            || cell.dim == Some(true)
            || cell.inverse == Some(true)
            || cell.strikethrough == Some(true);
        if cell.char != " " || styled {
            return index + 1;
        }
    }
    0
}

fn style_of(cell: &TerminalCell) -> Style {
    Style {
        fg: cell.fg,
        bg: cell.bg,
        fg_mode: cell.fg_mode,
        bg_mode: cell.bg_mode,
        bold: cell.bold == Some(true),
        italic: cell.italic == Some(true),
        underline: cell.underline == Some(true),
        dim: cell.dim == Some(true),
        inverse: cell.inverse == Some(true),
        strikethrough: cell.strikethrough == Some(true),
    }
}

fn style_to_ansi(style: &Style) -> String {
    let mut codes = vec!["0".to_owned()];
    for (on, code) in [
        (style.bold, "1"),
        (style.dim, "2"),
        (style.italic, "3"),
        (style.underline, "4"),
        (style.inverse, "7"),
        (style.strikethrough, "9"),
    ] {
        if on {
            codes.push(code.to_owned());
        }
    }
    if let (Some(value), Some(mode)) = (style.fg, style.fg_mode) {
        codes.extend(color_to_sgr(mode, value, false));
    }
    if let (Some(value), Some(mode)) = (style.bg, style.bg_mode) {
        codes.extend(color_to_sgr(mode, value, true));
    }
    format!("\u{1b}[{}m", codes.join(";"))
}

fn color_to_sgr(mode: f64, value: f64, background: bool) -> Vec<String> {
    #[allow(clippy::float_cmp)]
    if mode == 1.0 {
        let base = if value >= 8.0 {
            if background { 100.0 } else { 90.0 }
        } else if background {
            40.0
        } else {
            30.0
        };
        let offset = if value >= 8.0 { value - 8.0 } else { value };
        return vec![js_number(base + offset)];
    }
    let prefix = if background { "48" } else { "38" }.to_owned();
    #[allow(clippy::float_cmp)]
    if mode == 2.0 {
        return vec![prefix, "5".to_owned(), js_number(value)];
    }
    #[allow(clippy::float_cmp)]
    if mode == 3.0 {
        let packed = to_int32(value);
        return vec![
            prefix,
            "2".to_owned(),
            ((packed >> 16) & 0xff).to_string(),
            ((packed >> 8) & 0xff).to_string(),
            (packed & 0xff).to_string(),
        ];
    }
    Vec::new()
}

/// ECMAScript `ToInt32`, which the baseline's `>>` and `&` apply.
fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let modulo = value.trunc().rem_euclid(4_294_967_296.0);
    // `modulo` is a whole number in [0, 2^32).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let unsigned = modulo as u32;
    i32::from_ne_bytes(unsigned.to_ne_bytes())
}
