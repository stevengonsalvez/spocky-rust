//! The VT500 escape sequence state machine and the OSC sub parser.
//!
//! Ported from xterm.js 6.0.0 `src/common/parser/EscapeSequenceParser.ts`,
//! `src/common/parser/OscParser.ts` and `src/common/parser/Constants.ts`.
//! Copyright (c) 2018 The xterm.js authors. MIT License.
//!
//! The dispatch loop itself lives with the input handler in `terminal.rs`,
//! because every action calls into terminal state. Handlers are synchronous,
//! so the async pause and resume stack of the original never engages.

use std::sync::LazyLock;

use crate::params::Params;

pub(crate) const GROUND: u8 = 0;
pub(crate) const ESCAPE: u8 = 1;
const ESCAPE_INTERMEDIATE: u8 = 2;
const CSI_ENTRY: u8 = 3;
const CSI_PARAM: u8 = 4;
const CSI_INTERMEDIATE: u8 = 5;
const CSI_IGNORE: u8 = 6;
const SOS_PM_APC_STRING: u8 = 7;
const OSC_STRING: u8 = 8;
const DCS_ENTRY: u8 = 9;
const DCS_PARAM: u8 = 10;
const DCS_IGNORE: u8 = 11;
const DCS_INTERMEDIATE: u8 = 12;
const DCS_PASSTHROUGH: u8 = 13;

pub(crate) const IGNORE: u8 = 0;
pub(crate) const ERROR: u8 = 1;
pub(crate) const PRINT: u8 = 2;
pub(crate) const EXECUTE: u8 = 3;
pub(crate) const OSC_START: u8 = 4;
pub(crate) const OSC_PUT: u8 = 5;
pub(crate) const OSC_END: u8 = 6;
pub(crate) const CSI_DISPATCH: u8 = 7;
pub(crate) const PARAM: u8 = 8;
pub(crate) const COLLECT: u8 = 9;
pub(crate) const ESC_DISPATCH: u8 = 10;
pub(crate) const CLEAR: u8 = 11;
pub(crate) const DCS_HOOK: u8 = 12;
pub(crate) const DCS_PUT: u8 = 13;
pub(crate) const DCS_UNHOOK: u8 = 14;

pub(crate) const TRANSITION_ACTION_SHIFT: u8 = 4;
pub(crate) const TRANSITION_STATE_MASK: u8 = 15;

/// Placeholder index for every code point from U+00A0 up.
pub(crate) const NON_ASCII_PRINTABLE: u32 = 0xA0;

/// OSC and DCS payload limit, in UTF-16 code units.
const PAYLOAD_LIMIT: usize = 10_000_000;

/// `state << 8 | code` maps to `action << 4 | next state`.
pub(crate) static VT500_TRANSITION_TABLE: LazyLock<Vec<u8>> = LazyLock::new(build_table);

#[allow(clippy::too_many_lines)]
fn build_table() -> Vec<u8> {
    let mut table = vec![(ERROR << TRANSITION_ACTION_SHIFT) | GROUND; 4095];
    let mut add = |codes: &[u32], state: u8, action: u8, next: u8| {
        for &code in codes {
            table[(usize::from(state) << 8) | code as usize] =
                (action << TRANSITION_ACTION_SHIFT) | next;
        }
    };
    let r = |start: u32, end: u32| (start..end).collect::<Vec<u32>>();
    let printables = r(0x20, 0x7f);
    let mut executables = r(0x00, 0x18);
    executables.push(0x19);
    executables.extend(r(0x1c, 0x20));
    for state in GROUND..=DCS_PASSTHROUGH {
        add(&[0x18, 0x1a, 0x99, 0x9a], state, EXECUTE, GROUND);
        add(&r(0x80, 0x90), state, EXECUTE, GROUND);
        add(&r(0x90, 0x98), state, EXECUTE, GROUND);
        add(&[0x9c], state, IGNORE, GROUND);
        add(&[0x1b], state, CLEAR, ESCAPE);
        add(&[0x9d], state, OSC_START, OSC_STRING);
        add(&[0x98, 0x9e, 0x9f], state, IGNORE, SOS_PM_APC_STRING);
        add(&[0x9b], state, CLEAR, CSI_ENTRY);
        add(&[0x90], state, CLEAR, DCS_ENTRY);
    }
    add(&printables, GROUND, PRINT, GROUND);
    add(&executables, GROUND, EXECUTE, GROUND);
    add(&executables, ESCAPE, EXECUTE, ESCAPE);
    add(&[0x7f], ESCAPE, IGNORE, ESCAPE);
    add(&executables, OSC_STRING, IGNORE, OSC_STRING);
    add(&executables, CSI_ENTRY, EXECUTE, CSI_ENTRY);
    add(&[0x7f], CSI_ENTRY, IGNORE, CSI_ENTRY);
    add(&executables, CSI_PARAM, EXECUTE, CSI_PARAM);
    add(&[0x7f], CSI_PARAM, IGNORE, CSI_PARAM);
    add(&executables, CSI_IGNORE, EXECUTE, CSI_IGNORE);
    add(&executables, CSI_INTERMEDIATE, EXECUTE, CSI_INTERMEDIATE);
    add(&[0x7f], CSI_INTERMEDIATE, IGNORE, CSI_INTERMEDIATE);
    add(
        &executables,
        ESCAPE_INTERMEDIATE,
        EXECUTE,
        ESCAPE_INTERMEDIATE,
    );
    add(&[0x7f], ESCAPE_INTERMEDIATE, IGNORE, ESCAPE_INTERMEDIATE);
    // osc
    add(&[0x5d], ESCAPE, OSC_START, OSC_STRING);
    add(&printables, OSC_STRING, OSC_PUT, OSC_STRING);
    add(&[0x7f], OSC_STRING, OSC_PUT, OSC_STRING);
    add(&[0x9c, 0x1b, 0x18, 0x1a, 0x07], OSC_STRING, OSC_END, GROUND);
    add(&r(0x1c, 0x20), OSC_STRING, IGNORE, OSC_STRING);
    // sos/pm/apc does nothing
    add(&[0x58, 0x5e, 0x5f], ESCAPE, IGNORE, SOS_PM_APC_STRING);
    add(&printables, SOS_PM_APC_STRING, IGNORE, SOS_PM_APC_STRING);
    add(&executables, SOS_PM_APC_STRING, IGNORE, SOS_PM_APC_STRING);
    add(&[0x9c], SOS_PM_APC_STRING, IGNORE, GROUND);
    add(&[0x7f], SOS_PM_APC_STRING, IGNORE, SOS_PM_APC_STRING);
    // csi entries
    add(&[0x5b], ESCAPE, CLEAR, CSI_ENTRY);
    add(&r(0x40, 0x7f), CSI_ENTRY, CSI_DISPATCH, GROUND);
    add(&r(0x30, 0x3c), CSI_ENTRY, PARAM, CSI_PARAM);
    add(&[0x3c, 0x3d, 0x3e, 0x3f], CSI_ENTRY, COLLECT, CSI_PARAM);
    add(&r(0x30, 0x3c), CSI_PARAM, PARAM, CSI_PARAM);
    add(&r(0x40, 0x7f), CSI_PARAM, CSI_DISPATCH, GROUND);
    add(&[0x3c, 0x3d, 0x3e, 0x3f], CSI_PARAM, IGNORE, CSI_IGNORE);
    add(&r(0x20, 0x40), CSI_IGNORE, IGNORE, CSI_IGNORE);
    add(&[0x7f], CSI_IGNORE, IGNORE, CSI_IGNORE);
    add(&r(0x40, 0x7f), CSI_IGNORE, IGNORE, GROUND);
    add(&r(0x20, 0x30), CSI_ENTRY, COLLECT, CSI_INTERMEDIATE);
    add(&r(0x20, 0x30), CSI_INTERMEDIATE, COLLECT, CSI_INTERMEDIATE);
    add(&r(0x30, 0x40), CSI_INTERMEDIATE, IGNORE, CSI_IGNORE);
    add(&r(0x40, 0x7f), CSI_INTERMEDIATE, CSI_DISPATCH, GROUND);
    add(&r(0x20, 0x30), CSI_PARAM, COLLECT, CSI_INTERMEDIATE);
    // esc_intermediate
    add(&r(0x20, 0x30), ESCAPE, COLLECT, ESCAPE_INTERMEDIATE);
    add(
        &r(0x20, 0x30),
        ESCAPE_INTERMEDIATE,
        COLLECT,
        ESCAPE_INTERMEDIATE,
    );
    add(&r(0x30, 0x7f), ESCAPE_INTERMEDIATE, ESC_DISPATCH, GROUND);
    add(&r(0x30, 0x50), ESCAPE, ESC_DISPATCH, GROUND);
    add(&r(0x51, 0x58), ESCAPE, ESC_DISPATCH, GROUND);
    add(&[0x59, 0x5a, 0x5c], ESCAPE, ESC_DISPATCH, GROUND);
    add(&r(0x60, 0x7f), ESCAPE, ESC_DISPATCH, GROUND);
    // dcs entry
    add(&[0x50], ESCAPE, CLEAR, DCS_ENTRY);
    add(&executables, DCS_ENTRY, IGNORE, DCS_ENTRY);
    add(&[0x7f], DCS_ENTRY, IGNORE, DCS_ENTRY);
    add(&r(0x1c, 0x20), DCS_ENTRY, IGNORE, DCS_ENTRY);
    add(&r(0x20, 0x30), DCS_ENTRY, COLLECT, DCS_INTERMEDIATE);
    add(&r(0x30, 0x3c), DCS_ENTRY, PARAM, DCS_PARAM);
    add(&[0x3c, 0x3d, 0x3e, 0x3f], DCS_ENTRY, COLLECT, DCS_PARAM);
    add(&executables, DCS_IGNORE, IGNORE, DCS_IGNORE);
    add(&r(0x20, 0x80), DCS_IGNORE, IGNORE, DCS_IGNORE);
    add(&r(0x1c, 0x20), DCS_IGNORE, IGNORE, DCS_IGNORE);
    add(&executables, DCS_PARAM, IGNORE, DCS_PARAM);
    add(&[0x7f], DCS_PARAM, IGNORE, DCS_PARAM);
    add(&r(0x1c, 0x20), DCS_PARAM, IGNORE, DCS_PARAM);
    add(&r(0x30, 0x3c), DCS_PARAM, PARAM, DCS_PARAM);
    add(&[0x3c, 0x3d, 0x3e, 0x3f], DCS_PARAM, IGNORE, DCS_IGNORE);
    add(&r(0x20, 0x30), DCS_PARAM, COLLECT, DCS_INTERMEDIATE);
    add(&executables, DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    add(&[0x7f], DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    add(&r(0x1c, 0x20), DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    add(&r(0x20, 0x30), DCS_INTERMEDIATE, COLLECT, DCS_INTERMEDIATE);
    add(&r(0x30, 0x40), DCS_INTERMEDIATE, IGNORE, DCS_IGNORE);
    add(&r(0x40, 0x7f), DCS_INTERMEDIATE, DCS_HOOK, DCS_PASSTHROUGH);
    add(&r(0x40, 0x7f), DCS_PARAM, DCS_HOOK, DCS_PASSTHROUGH);
    add(&r(0x40, 0x7f), DCS_ENTRY, DCS_HOOK, DCS_PASSTHROUGH);
    add(&executables, DCS_PASSTHROUGH, DCS_PUT, DCS_PASSTHROUGH);
    add(&printables, DCS_PASSTHROUGH, DCS_PUT, DCS_PASSTHROUGH);
    add(&[0x7f], DCS_PASSTHROUGH, IGNORE, DCS_PASSTHROUGH);
    add(
        &[0x1b, 0x9c, 0x18, 0x1a],
        DCS_PASSTHROUGH,
        DCS_UNHOOK,
        GROUND,
    );
    // special handling of unicode chars
    add(&[NON_ASCII_PRINTABLE], GROUND, PRINT, GROUND);
    add(&[NON_ASCII_PRINTABLE], OSC_STRING, OSC_PUT, OSC_STRING);
    add(&[NON_ASCII_PRINTABLE], CSI_IGNORE, IGNORE, CSI_IGNORE);
    add(&[NON_ASCII_PRINTABLE], DCS_IGNORE, IGNORE, DCS_IGNORE);
    add(
        &[NON_ASCII_PRINTABLE],
        DCS_PASSTHROUGH,
        DCS_PUT,
        DCS_PASSTHROUGH,
    );
    table
}

/// Parser state carried across `parse` calls.
#[derive(Debug, Clone)]
pub(crate) struct ParserState {
    pub(crate) current_state: u8,
    pub(crate) preceding_join_state: u32,
    pub(crate) params: Params,
    pub(crate) collect: i32,
    pub(crate) osc: OscParser,
}

impl ParserState {
    pub(crate) fn new() -> Self {
        let mut params = Params::new();
        params.add_param(0);
        Self {
            current_state: GROUND,
            preceding_join_state: 0,
            params,
            collect: 0,
            osc: OscParser::new(),
        }
    }

    /// `EscapeSequenceParser.reset`. DCS needs no reset: no DCS handler has
    /// an observable effect (DECRQSS only answers through `onData`, which
    /// Paseo never subscribes to).
    pub(crate) fn reset(&mut self) {
        self.current_state = GROUND;
        self.osc.reset();
        self.clear();
        self.preceding_join_state = 0;
    }

    /// The `CLEAR` action: fresh ZDM params and no intermediates.
    pub(crate) fn clear(&mut self) {
        self.params.reset();
        self.params.add_param(0);
        self.collect = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Start,
    Id,
    Payload,
    Abort,
}

/// The finished OSC command handed to the handlers by [`OscParser::end`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OscCommand {
    /// The identifier, `-1` when it had no digits. A JavaScript number, so
    /// a long digit run stays a (huge) double.
    pub(crate) id: f64,
    /// The payload, or `None` when it exceeded the payload limit.
    pub(crate) data: Option<String>,
}

/// `OscParser`. Every handler of an identifier accumulates the same payload,
/// so one shared buffer stands in for each `OscHandler`'s copy.
#[derive(Debug, Clone)]
pub(crate) struct OscParser {
    state: OscState,
    id: f64,
    active: bool,
    data: String,
    data_units: usize,
    hit_limit: bool,
}

impl OscParser {
    fn new() -> Self {
        Self {
            state: OscState::Start,
            id: -1.0,
            active: false,
            data: String::new(),
            data_units: 0,
            hit_limit: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.active = false;
        self.id = -1.0;
        self.state = OscState::Start;
    }

    /// `_start`: looks up the handlers and resets their payload.
    fn begin(&mut self, has_handlers: &dyn Fn(f64) -> bool) {
        self.active = has_handlers(self.id);
        self.data.clear();
        self.data_units = 0;
        self.hit_limit = false;
    }

    pub(crate) fn start(&mut self) {
        self.reset();
        self.state = OscState::Id;
    }

    pub(crate) fn put(&mut self, data: &[u32], has_handlers: &dyn Fn(f64) -> bool) {
        if self.state == OscState::Abort {
            return;
        }
        let mut start = 0;
        if self.state == OscState::Id {
            while start < data.len() {
                let code = data[start];
                start += 1;
                if code == 0x3b {
                    self.state = OscState::Payload;
                    self.begin(has_handlers);
                    break;
                }
                if !(0x30..=0x39).contains(&code) {
                    self.state = OscState::Abort;
                    return;
                }
                if self.id < 0.0 {
                    self.id = 0.0;
                }
                self.id = self.id * 10.0 + f64::from(code) - 48.0;
            }
        }
        if self.state == OscState::Payload && start < data.len() && self.active {
            self.append(&data[start..]);
        }
    }

    fn append(&mut self, data: &[u32]) {
        if self.hit_limit {
            return;
        }
        for &code in data {
            let character = char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
            self.data.push(character);
            self.data_units += character.len_utf16();
        }
        if self.data_units > PAYLOAD_LIMIT {
            self.data = String::new();
            self.data_units = 0;
            self.hit_limit = true;
        }
    }

    /// Ends the command. Returns the command for its handlers when it
    /// completed successfully with handlers registered for its identifier.
    pub(crate) fn end(
        &mut self,
        success: bool,
        has_handlers: &dyn Fn(f64) -> bool,
    ) -> Option<OscCommand> {
        if self.state == OscState::Start {
            return None;
        }
        let mut command = None;
        if self.state != OscState::Abort {
            if self.state == OscState::Id {
                self.begin(has_handlers);
            }
            if self.active && success {
                command = Some(OscCommand {
                    id: self.id,
                    data: (!self.hit_limit).then(|| std::mem::take(&mut self.data)),
                });
            }
        }
        self.active = false;
        self.id = -1.0;
        self.state = OscState::Start;
        command
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CSI_DISPATCH, CSI_PARAM, ESCAPE, GROUND, OscParser, PRINT, TRANSITION_ACTION_SHIFT,
        VT500_TRANSITION_TABLE,
    };

    fn transition(state: u8, code: usize) -> (u8, u8) {
        let value = VT500_TRANSITION_TABLE[(usize::from(state) << 8) | code];
        (value >> TRANSITION_ACTION_SHIFT, value & 15)
    }

    #[test]
    fn table_follows_the_vt500_diagram() {
        assert_eq!(transition(GROUND, usize::from(b'a')), (PRINT, GROUND));
        assert_eq!(transition(ESCAPE, usize::from(b'[')).1, 3);
        assert_eq!(
            transition(CSI_PARAM, usize::from(b'm')),
            (CSI_DISPATCH, GROUND)
        );
    }

    #[test]
    fn osc_collects_id_and_payload() {
        let mut osc = OscParser::new();
        let any = |_: f64| true;
        osc.start();
        let text: Vec<u32> = "633;D;0".chars().map(u32::from).collect();
        osc.put(&text, &any);
        let command = osc.end(true, &any).expect("command");
        assert!((command.id - 633.0).abs() < f64::EPSILON);
        assert_eq!(command.data.as_deref(), Some("D;0"));
    }
}
