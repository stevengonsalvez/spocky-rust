//! Plain-text capture of a terminal, following pinned
//! `packages/server/src/terminal/terminal-capture.ts` and the `strip-ansi`
//! 7.1.2 package (`ansi-regex` 6.2.2) it calls.

use spocky_contracts::js_value::{js_text_from_utf16, js_text_utf16};
use spocky_wire::{TerminalCell, TerminalState};

use crate::process_title::is_js_whitespace;

const ESC: u16 = 0x1B;
const BEL: u16 = 0x07;
const CSI_C1: u16 = 0x9B;
const ST_C1: u16 = 0x9C;

/// `ansiRegex()` replaced by `""`, as `stripAnsi(string)` does:
///
/// ```text
/// osc = ESC ] [\s\S]*? (BEL | ESC \ | U+009C)
/// csi = [ESC U+009B] [[\]()#;?]* (\d{1,4}([;:]\d{0,4})*)? [\dA-PR-TZcf-nq-uy=><~]
/// ```
///
/// Matching is leftmost, `osc` before `csi`, with the regex engine's greedy
/// choices and backtracking.
#[must_use]
pub fn strip_ansi(input: &str) -> String {
    let units: Vec<u16> = js_text_utf16(input).collect();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if let Some(end) = match_ansi(&units, index) {
            index = end;
        } else {
            out.push(units[index]);
            index += 1;
        }
    }
    js_text_from_utf16(&out)
}

fn match_ansi(units: &[u16], start: usize) -> Option<usize> {
    match_osc(units, start).or_else(|| match_csi(units, start))
}

/// `ESC ] [\s\S]*? ST`: the first string terminator after the introducer.
fn match_osc(units: &[u16], start: usize) -> Option<usize> {
    if units.get(start) != Some(&ESC) || units.get(start + 1) != Some(&0x5D) {
        return None;
    }
    let mut index = start + 2;
    while index < units.len() {
        match units[index] {
            BEL | ST_C1 => return Some(index + 1),
            ESC if units.get(index + 1) == Some(&0x5C) => return Some(index + 2),
            _ => index += 1,
        }
    }
    None
}

fn is_intermediate(unit: u16) -> bool {
    matches!(unit, 0x5B | 0x5D | 0x28 | 0x29 | 0x23 | 0x3B | 0x3F)
}

fn is_digit(unit: u16) -> bool {
    (0x30..=0x39).contains(&unit)
}

/// `[\dA-PR-TZcf-nq-uy=><~]`.
fn is_final(unit: u16) -> bool {
    is_digit(unit)
        || (0x41..=0x50).contains(&unit)
        || (0x52..=0x54).contains(&unit)
        || unit == 0x5A
        || unit == 0x63
        || (0x66..=0x6E).contains(&unit)
        || (0x71..=0x75).contains(&unit)
        || unit == 0x79
        || matches!(unit, 0x3D | 0x3E | 0x3C | 0x7E)
}

fn digit_run(units: &[u16], from: usize) -> usize {
    units[from.min(units.len())..]
        .iter()
        .take_while(|unit| is_digit(**unit))
        .count()
}

fn match_csi(units: &[u16], start: usize) -> Option<usize> {
    if !matches!(units.get(start), Some(&ESC | &CSI_C1)) {
        return None;
    }
    let after = start + 1;
    let run = units[after.min(units.len())..]
        .iter()
        .take_while(|unit| is_intermediate(**unit))
        .count();
    // Greedy intermediates, backing off one at a time.
    for taken in (0..=run).rev() {
        let at = after + taken;
        // The optional parameter group, tried before skipping it.
        for digits in (1..=digit_run(units, at).min(4)).rev() {
            if let Some(end) = csi_tail(units, at + digits) {
                return Some(end);
            }
        }
        if units.get(at).is_some_and(|unit| is_final(*unit)) {
            return Some(at + 1);
        }
    }
    None
}

/// After the first digits: `([;:]\d{0,4})*` greedy, then the final byte.
fn csi_tail(units: &[u16], at: usize) -> Option<usize> {
    if matches!(units.get(at), Some(&(0x3B | 0x3A))) {
        for digits in (0..=digit_run(units, at + 1).min(4)).rev() {
            if let Some(end) = csi_tail(units, at + 1 + digits) {
                return Some(end);
            }
        }
    }
    units
        .get(at)
        .is_some_and(|unit| is_final(*unit))
        .then_some(at + 1)
}

/// `cellsToPlainText`.
fn cells_to_plain_text(cells: &[TerminalCell], strip: bool) -> String {
    let joined: String = cells.iter().map(|cell| cell.char.as_str()).collect();
    let text = joined.trim_end_matches(is_js_whitespace);
    if strip {
        strip_ansi(text)
    } else {
        text.to_owned()
    }
}

/// `resolveCaptureLineIndex` over JavaScript numbers.
fn resolve_index(line: Option<f64>, total: usize, start: bool) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let total = total as f64;
    if total == 0.0 {
        return if start { 0.0 } else { -1.0 };
    }
    let default = if start { 0.0 } else { total - 1.0 };
    let Some(line) = line else {
        return default;
    };
    let resolved = if line < 0.0 { total + line } else { line };
    if resolved < 0.0 {
        return 0.0;
    }
    if resolved >= total {
        return total - 1.0;
    }
    resolved
}

/// `Array.prototype.slice` bound: `ToIntegerOrInfinity` clamped to the length.
fn slice_bound(value: f64, length: usize) -> usize {
    #[allow(clippy::cast_precision_loss)]
    let length_f = length as f64;
    let integer = if value.is_nan() { 0.0 } else { value.trunc() };
    let relative = if integer < 0.0 {
        (length_f + integer).max(0.0)
    } else {
        integer.min(length_f)
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bound = relative as usize;
    bound
}

/// `CaptureTerminalLinesOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CaptureOptions {
    pub start: Option<f64>,
    pub end: Option<f64>,
    /// `None` is the default, which strips.
    pub strip_ansi: Option<bool>,
}

/// `CaptureTerminalLinesResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureResult {
    pub lines: Vec<String>,
    pub total_lines: usize,
}

/// `captureTerminalLines(terminal, options)` over the terminal's state.
#[must_use]
pub fn capture_lines(state: &TerminalState, options: &CaptureOptions) -> CaptureResult {
    let strip = options.strip_ansi.unwrap_or(true);
    let all: Vec<String> = state
        .scrollback
        .iter()
        .chain(&state.grid)
        .map(|cells| cells_to_plain_text(cells, strip))
        .collect();
    let total = all.len();
    let start = resolve_index(options.start, total, true);
    let end = resolve_index(options.end, total, false);
    if total == 0 || start > end {
        return CaptureResult {
            lines: Vec::new(),
            total_lines: total,
        };
    }
    let from = slice_bound(start, total);
    let to = slice_bound(end + 1.0, total);
    CaptureResult {
        lines: if from < to {
            all[from..to].to_vec()
        } else {
            Vec::new()
        },
        total_lines: total,
    }
}
