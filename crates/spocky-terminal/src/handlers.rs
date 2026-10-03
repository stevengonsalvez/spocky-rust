//! The parser handlers `createTerminal` registers on the headless terminal in
//! pinned `packages/server/src/terminal/terminal.ts`: device attribute and
//! status replies, OSC color query replies, and OSC 633 command completion.
//!
//! Replies the baseline writes straight to the PTY are queued in
//! [`ParserEvents`] in parse order; the session writes them after each
//! `Terminal::write`, which keeps their order relative to input-mode replies
//! (those are written before the parse, as in the baseline).

use std::cell::RefCell;
use std::rc::Rc;

use spocky_xterm::{Param, Terminal};

use crate::process_title::parse_command_finished_osc;

/// `TERMINAL_OSC_COLOR_QUERY_RESPONSES`.
const OSC_COLOR_QUERY_RESPONSES: [(u32, &str); 3] = [
    (10, "rgb:e6e6/e6e6/e6e6"),
    (11, "rgb:0b0b/0b0b/0b0b"),
    (12, "rgb:e6e6/e6e6/e6e6"),
];

/// What the handlers produced since the last [`ParserEvents::take`].
#[derive(Debug, Default)]
pub struct ParserEventQueue {
    /// Bytes to write to the PTY, in parse order.
    pub replies: Vec<String>,
    /// OSC 633 `D` payloads: the exit code, or `None` for `{ exitCode: null }`.
    pub command_finished: Vec<Option<f64>>,
}

/// Shared handle to the queue the handlers fill.
#[derive(Debug, Clone, Default)]
pub struct ParserEvents(Rc<RefCell<ParserEventQueue>>);

impl ParserEvents {
    /// Everything queued so far, emptying the queue.
    #[must_use]
    pub fn take(&self) -> ParserEventQueue {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

fn reply(events: &ParserEvents, text: String) {
    events.0.borrow_mut().replies.push(text);
}

fn only_value(params: &[Param]) -> Option<i32> {
    match params {
        [Param::Value(value)] => Some(*value),
        _ => None,
    }
}

/// Registers the baseline handlers; replies and OSC 633 events land in the
/// returned queue.
#[must_use]
pub fn register_handlers(terminal: &mut Terminal) -> ParserEvents {
    let events = ParserEvents::default();

    // DA1 (`CSI c` or `CSI 0 c`): apps like nvim query terminal capabilities.
    let sink = events.clone();
    let _ = terminal.register_csi_handler(None, b'c', move |params, _| {
        if params.is_empty() || only_value(params) == Some(0) {
            reply(&sink, "\u{1b}[?62;4;22c".to_owned());
            return true;
        }
        false
    });

    let sink = events.clone();
    let _ =
        terminal.register_csi_handler(None, b'n', move |params, cursor| match only_value(params) {
            Some(5) => {
                reply(&sink, "\u{1b}[0n".to_owned());
                true
            }
            Some(6) => {
                reply(&sink, format!("\u{1b}[{};{}R", cursor.y + 1, cursor.x + 1));
                true
            }
            _ => false,
        });

    let sink = events.clone();
    let _ = terminal.register_csi_handler(Some(b'?'), b'n', move |params, cursor| {
        if only_value(params) != Some(6) {
            return false;
        }
        reply(&sink, format!("\u{1b}[?{};{}R", cursor.y + 1, cursor.x + 1));
        true
    });

    for (code, response) in OSC_COLOR_QUERY_RESPONSES {
        let sink = events.clone();
        terminal.register_osc_handler(code, move |data| {
            if data.trim_matches(crate::process_title::is_js_whitespace) != "?" {
                return false;
            }
            reply(&sink, format!("\u{1b}]{code};{response}\u{1b}\\"));
            true
        });
    }

    // OSC 633 is terminal control traffic, but a foreground command can still
    // print arbitrary control bytes: only the exact command-finished shape
    // counts, and the sequence is always consumed.
    let sink = events.clone();
    terminal.register_osc_handler(633, move |data| {
        if let Some(exit_code) = parse_command_finished_osc(data) {
            sink.0.borrow_mut().command_finished.push(exit_code);
        }
        true
    });

    events
}
