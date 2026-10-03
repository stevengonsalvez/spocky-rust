//! Exit diagnostics from recent raw output, following pinned
//! `extractLastOutputLinesFromText` and `stripAnsiSequences` in
//! `packages/server/src/terminal/terminal.ts`, plus the bounded recent-output
//! buffer `createTerminal` keeps for them.
//!
//! The baseline pattern is
//! `ESC(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\].*?(?:BEL|ESC\\))`. Its first class
//! spans `@`..`Z` and `\`..`_`, so `ESC ]` matches the first alternative and
//! only those two characters are removed; the OSC alternative never applies.
//! This port keeps that behavior.

use crate::process_title::is_js_whitespace;

/// `TERMINAL_EXIT_OUTPUT_LINE_LIMIT`.
pub const EXIT_OUTPUT_LINE_LIMIT: usize = 12;

/// `TERMINAL_EXIT_OUTPUT_CHAR_LIMIT`, in UTF-16 code units.
pub const EXIT_OUTPUT_CHAR_LIMIT: usize = 16_000;

/// `stripAnsiSequences`.
#[must_use]
pub fn strip_ansi_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('\u{1b}') {
        out.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        if let Some(length) = escape_length(after) {
            rest = &after[length..];
        } else {
            out.push('\u{1b}');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Bytes after `ESC` that the pattern consumes, if it matches here.
fn escape_length(after: &str) -> Option<usize> {
    let bytes = after.as_bytes();
    let first = *bytes.first()?;
    if (b'@'..=b'Z').contains(&first) || (b'\\'..=b'_').contains(&first) {
        return Some(1);
    }
    if first != b'[' {
        return None;
    }
    let mut index = 1;
    while bytes.get(index).is_some_and(|b| (b'0'..=b'?').contains(b)) {
        index += 1;
    }
    while bytes.get(index).is_some_and(|b| (b' '..=b'/').contains(b)) {
        index += 1;
    }
    bytes
        .get(index)
        .is_some_and(|b| (b'@'..=b'~').contains(b))
        .then_some(index + 1)
}

/// `extractLastOutputLinesFromText(text, limit)`.
#[must_use]
pub fn last_output_lines_from_text(text: &str, limit: usize) -> Vec<String> {
    let normalized = strip_ansi_sequences(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut lines: Vec<String> = normalized
        .split('\n')
        .map(|line| line.trim_end_matches(is_js_whitespace).to_owned())
        .collect();
    let blank = |line: &String| line.trim_matches(is_js_whitespace).is_empty();
    while lines.first().is_some_and(blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(blank) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(limit);
    lines.split_off(skip)
}

/// Recent PTY output kept as whole chunks whose join always holds at least
/// the last [`EXIT_OUTPUT_CHAR_LIMIT`] UTF-16 units, as `createTerminal` does.
#[derive(Debug, Default)]
pub struct RecentOutput {
    chunks: std::collections::VecDeque<(String, usize)>,
    length: usize,
}

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The last `limit` UTF-16 units of `text`, as `slice(-limit)`. A surrogate
/// pair split by the cut leaves a lone low surrogate in JavaScript; that
/// half has no UTF-8 form, so it becomes U+FFFD here.
fn utf16_tail(text: &str, limit: usize) -> String {
    let total = utf16_len(text);
    if total <= limit {
        return text.to_owned();
    }
    let mut skip = total - limit;
    let mut out = String::new();
    for c in text.chars() {
        let width = c.len_utf16();
        if skip == 0 {
            out.push(c);
        } else if skip < width {
            out.push('\u{fffd}');
            skip = 0;
        } else {
            skip -= width;
        }
    }
    out
}

impl RecentOutput {
    /// Records one PTY chunk.
    pub fn push(&mut self, data: &str) {
        let length = utf16_len(data);
        self.chunks.push_back((data.to_owned(), length));
        self.length += length;
        while self.chunks.len() > 1 {
            let front = self.chunks[0].1;
            if self.length - front < EXIT_OUTPUT_CHAR_LIMIT {
                break;
            }
            self.length -= front;
            self.chunks.pop_front();
        }
        if self.chunks.len() == 1 && self.length > EXIT_OUTPUT_CHAR_LIMIT {
            let tail = utf16_tail(&self.chunks[0].0, EXIT_OUTPUT_CHAR_LIMIT);
            let length = utf16_len(&tail);
            self.chunks[0] = (tail, length);
            self.length = length;
        }
    }

    /// `recentOutputChunks.join("").slice(-TERMINAL_EXIT_OUTPUT_CHAR_LIMIT)`.
    #[must_use]
    pub fn tail(&self) -> String {
        let joined: String = self
            .chunks
            .iter()
            .map(|(chunk, _)| chunk.as_str())
            .collect();
        utf16_tail(&joined, EXIT_OUTPUT_CHAR_LIMIT)
    }

    /// Drops everything, as `disposeResources` does.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.length = 0;
    }
}
