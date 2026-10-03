//! Input-mode tracking over PTY output, following pinned
//! `packages/protocol/src/terminal-input-mode.ts`.
//!
//! The tracker watches kitty keyboard (`CSI > u`, `CSI = u`, `CSI < u`,
//! `CSI ? u`) and private mode (`CSI ? Pm h|l` for 1, 2004, 9001) sequences,
//! answers the kitty flags query, and builds the preamble that replays the
//! modes to a client. A sequence cut at a chunk end is held for the next
//! chunk. Flags are JavaScript numbers, so a huge parameter keeps
//! `Number(...)` semantics.

use std::fmt::Write as _;

use spocky_contracts::js_value::js_number;

const ESC: char = '\u{1b}';
const APPLICATION_CURSOR_KEYS_MODE: f64 = 1.0;
const WIN32_INPUT_MODE: f64 = 9001.0;
const BRACKETED_PASTE_MODE: f64 = 2004.0;

/// `TerminalInputModeFeedResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedResult {
    pub changed: bool,
    pub responses: Vec<String>,
}

/// `TerminalInputModeState`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InputModeState {
    pub kitty_keyboard_flags: f64,
    pub win32_input_mode: bool,
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
}

/// `TerminalInputModeTracker`.
#[derive(Debug, Clone, Default)]
pub struct InputModeTracker {
    state: InputModeState,
    kitty_keyboard_stack: Vec<f64>,
    pending: String,
}

enum Sequence<'a> {
    Kitty {
        prefix: Option<char>,
        params: &'a str,
    },
    PrivateMode {
        params: &'a str,
        set: bool,
    },
}

/// Matches `ESC[` + (`[<>=?]?[0-9;]*u` | `?[0-9;]*[hl]`) at `start`, returning
/// the sequence and the index after it, as the baseline's global regex does.
fn match_at(text: &str, start: usize) -> Option<(Sequence<'_>, usize)> {
    let bytes = text.as_bytes();
    let body = start + 2;
    let digits_end = |from: usize| {
        bytes[from..]
            .iter()
            .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
            .map_or(bytes.len(), |offset| from + offset)
    };
    let prefix = bytes
        .get(body)
        .copied()
        .filter(|byte| matches!(byte, b'<' | b'>' | b'=' | b'?'))
        .map(char::from);
    let params_start = body + usize::from(prefix.is_some());
    let params_end = digits_end(params_start);
    if bytes.get(params_end) == Some(&b'u') {
        return Some((
            Sequence::Kitty {
                prefix,
                params: &text[params_start..params_end],
            },
            params_end + 1,
        ));
    }
    if prefix == Some('?')
        && let Some(final_byte @ (b'h' | b'l')) = bytes.get(params_end).copied()
    {
        return Some((
            Sequence::PrivateMode {
                params: &text[params_start..params_end],
                set: final_byte == b'h',
            },
            params_end + 1,
        ));
    }
    None
}

/// A number as a template literal writes it. A parameter longer than a double
/// holds becomes `Infinity`, which `js_number` (the `JSON.stringify` rule)
/// would turn into `null`.
fn number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        js_number(value)
    }
}

/// `/^\d+$/` then `Number(...)`.
fn whole_number(text: &str) -> Option<f64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn param(params: &str, index: usize) -> Option<f64> {
    params.split(';').nth(index).and_then(whole_number)
}

/// `INCOMPLETE_CSI_INPUT_MODE_SEQUENCE`: `ESC[`, an optional prefix, then
/// only digits and semicolons to the end.
fn is_incomplete_sequence(pending: &str) -> bool {
    let rest = &pending[2..];
    let rest = rest.strip_prefix(['<', '>', '=', '?']).unwrap_or(rest);
    rest.bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b';')
}

impl InputModeTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `feed(data)`.
    pub fn feed(&mut self, data: &str) -> FeedResult {
        if data.is_empty() {
            return FeedResult::default();
        }
        let text = format!("{}{data}", std::mem::take(&mut self.pending));
        let mut result = FeedResult::default();
        let mut consumed_until = 0;
        let mut search = 0;
        while let Some(offset) = text[search..].find("\u{1b}[") {
            let start = search + offset;
            let Some((sequence, end)) = match_at(&text, start) else {
                search = start + 1;
                continue;
            };
            consumed_until = end;
            search = end;
            match sequence {
                Sequence::PrivateMode { params, set } => {
                    result.changed = self.apply_private_mode(params, set) || result.changed;
                }
                Sequence::Kitty { prefix, params } => {
                    let changed = self.apply_kitty(prefix, params, &mut result.responses);
                    result.changed = result.changed || changed;
                }
            }
        }
        let tail = &text[consumed_until..];
        if let Some(pending_start) = tail.rfind("\u{1b}[") {
            let pending = &tail[pending_start..];
            if is_incomplete_sequence(pending) {
                pending.clone_into(&mut self.pending);
            }
        }
        result
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `getState()`.
    #[must_use]
    pub fn state(&self) -> InputModeState {
        self.state
    }

    /// `supportsModifiedEnter()`.
    #[must_use]
    pub fn supports_modified_enter(&self) -> bool {
        self.state.kitty_keyboard_flags > 0.0 || self.state.win32_input_mode
    }

    /// `getPreamble()`.
    #[must_use]
    pub fn preamble(&self) -> String {
        let mut preamble = String::new();
        if self.state.kitty_keyboard_flags > 0.0 {
            let _ = write!(
                preamble,
                "{ESC}[={};1u",
                number_text(self.state.kitty_keyboard_flags)
            );
        }
        if self.state.win32_input_mode {
            preamble.push_str("\u{1b}[?9001h");
        }
        if self.state.application_cursor_keys {
            preamble.push_str("\u{1b}[?1h");
        }
        if self.state.bracketed_paste {
            preamble.push_str("\u{1b}[?2004h");
        }
        preamble
    }

    fn apply_kitty(
        &mut self,
        prefix: Option<char>,
        params: &str,
        responses: &mut Vec<String>,
    ) -> bool {
        let previous = self.state.kitty_keyboard_flags;
        match prefix {
            Some('>') => {
                self.kitty_keyboard_stack.push(previous);
                self.state.kitty_keyboard_flags = param(params, 0).unwrap_or(1.0);
            }
            Some('=') => {
                let mode = param(params, 1).unwrap_or(1.0);
                #[allow(clippy::float_cmp)]
                let reset = mode == 0.0;
                self.state.kitty_keyboard_flags = if reset {
                    0.0
                } else {
                    param(params, 0).unwrap_or(0.0)
                };
            }
            Some('<') => {
                let mut remaining = param(params, 0).unwrap_or(1.0).max(1.0);
                while remaining > 0.0 {
                    // DEVIATION: the baseline pops `count` times, so a huge or
                    // infinite count never returns (pinned node ran past 20 s
                    // on 2000000000). Popping an empty stack yields 0 and every
                    // later pop does too, so stopping at the empty stack gives
                    // the same final state without the hang.
                    let Some(flags) = self.kitty_keyboard_stack.pop() else {
                        self.state.kitty_keyboard_flags = 0.0;
                        break;
                    };
                    self.state.kitty_keyboard_flags = flags;
                    remaining -= 1.0;
                }
            }
            Some('?') => {
                responses.push(format!(
                    "{ESC}[?{}u",
                    number_text(self.state.kitty_keyboard_flags)
                ));
                return false;
            }
            _ => return false,
        }
        #[allow(clippy::float_cmp)]
        let changed = self.state.kitty_keyboard_flags != previous;
        changed
    }

    fn apply_private_mode(&mut self, params: &str, set: bool) -> bool {
        let modes: Vec<f64> = params.split(';').filter_map(whole_number).collect();
        let has = |mode: f64| {
            modes
                .iter()
                .any(|candidate| candidate.to_bits() == mode.to_bits())
        };
        let mut changed = false;
        if has(WIN32_INPUT_MODE) {
            changed = self.state.win32_input_mode != set || changed;
            self.state.win32_input_mode = set;
        }
        if has(APPLICATION_CURSOR_KEYS_MODE) {
            changed = self.state.application_cursor_keys != set || changed;
            self.state.application_cursor_keys = set;
        }
        if has(BRACKETED_PASTE_MODE) {
            changed = self.state.bracketed_paste != set || changed;
            self.state.bracketed_paste = set;
        }
        changed
    }
}
