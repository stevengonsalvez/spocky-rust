//! The headless terminal: construction, writes, resize, handler
//! registration, and the parser dispatch loop.
//!
//! Ported from xterm.js 6.0.0 `src/headless/Terminal.ts`,
//! `src/common/CoreTerminal.ts`, the `parse` loop of
//! `src/common/parser/EscapeSequenceParser.ts`, the handler tables of
//! `src/common/InputHandler.ts`, `src/common/input/TextDecoder.ts`
//! (`StringToUtf32`) and `src/common/services/CoreService.ts`.
//! Copyright (c) 2014-2019 The xterm.js authors. MIT License.
//!
//! The options are the ones Paseo passes: `rows`, `cols`, `scrollback:
//! 1000`, `allowProposedApi: true`. Replies xterm itself sends through
//! `onData` (device attributes, status reports, DECRQM, DECRQSS) and pure
//! notifications (bell, colors, mouse, focus, keypad, bracketed paste) are
//! left out: Paseo subscribes to neither, so they change nothing it reads.

use std::collections::HashMap;

use crate::attributes::AttributeData;
use crate::buffer::BufferSet;
use crate::charsets::Charset;
use crate::circular_list::Throw;
use crate::params::Param;
use crate::parser::{
    CLEAR, COLLECT, CSI_DISPATCH, DCS_PUT, DCS_UNHOOK, ESC_DISPATCH, ESCAPE, EXECUTE,
    NON_ASCII_PRINTABLE, OSC_END, OSC_PUT, OSC_START, PARAM, PRINT, ParserState,
    TRANSITION_ACTION_SHIFT, TRANSITION_STATE_MASK, VT500_TRANSITION_TABLE,
};

/// Minimum terminal size (`MINIMUM_COLS`, `MINIMUM_ROWS`).
const MINIMUM_COLS: i64 = 2;
const MINIMUM_ROWS: i64 = 1;
/// `InputHandler` decodes and parses a write in slices of this many UTF-16
/// code units.
const MAX_PARSEBUFFER_LENGTH: usize = 131_072;
/// OSC identifiers `InputHandler` handles itself.
const BUILTIN_OSC: [u32; 12] = [0, 1, 2, 4, 8, 10, 11, 12, 104, 110, 111, 112];

/// xterm threw a JavaScript exception while handling a call.
///
/// A write that throws leaves xterm's write queue stuck: that write and
/// every later one are never parsed and their callbacks never run. A resize
/// that throws leaves the buffers partly resized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exception;

/// `decPrivateModes.cursorStyle` as set by DECSCUSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

/// The active buffer's cursor when a custom CSI handler runs
/// (`buffer.active.cursorX` and `cursorY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerCursor {
    pub x: i64,
    pub y: i64,
}

/// A CSI identifier byte that xterm's `_identifier` rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidIdentifier;

type CsiHandler = Box<dyn FnMut(&[Param], HandlerCursor) -> bool>;
type OscHandler = Box<dyn FnMut(&str) -> bool>;
type TitleListener = Box<dyn FnMut(&str)>;

/// `CoreService` modes plus the cursor fields Paseo reads.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Modes {
    pub(crate) insert_mode: bool,
    pub(crate) origin: bool,
    pub(crate) reverse_wraparound: bool,
    pub(crate) wraparound: bool,
    pub(crate) cursor_style: Option<CursorStyle>,
    pub(crate) cursor_blink: Option<bool>,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            insert_mode: false,
            origin: false,
            reverse_wraparound: false,
            wraparound: true,
            cursor_style: None,
            cursor_blink: None,
        }
    }
}

/// `CharsetService`.
#[derive(Debug, Clone, Default)]
pub(crate) struct CharsetService {
    pub(crate) charset: Option<Charset>,
    glevel: i64,
    charsets: [Option<Charset>; 4],
}

impl CharsetService {
    pub(crate) fn set_glevel(&mut self, level: i64) {
        self.glevel = level;
        self.charset = usize::try_from(level)
            .ok()
            .and_then(|level| self.charsets.get(level).copied().flatten());
    }

    pub(crate) fn set_gcharset(&mut self, level: i64, charset: Option<Charset>) {
        if let Some(slot) = usize::try_from(level)
            .ok()
            .and_then(|level| self.charsets.get_mut(level))
        {
            *slot = charset;
        }
        if self.glevel == level {
            self.charset = charset;
        }
    }
}

/// A headless terminal emulator, byte for byte the `@xterm/headless`
/// 6.0.0 `Terminal` that Paseo constructs.
pub struct Terminal {
    pub(crate) cols: i64,
    pub(crate) rows: i64,
    pub(crate) buffers: BufferSet,
    pub(crate) modes: Modes,
    pub(crate) cursor_hidden: bool,
    pub(crate) convert_eol: bool,
    pub(crate) charsets: CharsetService,
    pub(crate) cur_attr: AttributeData,
    pub(crate) erase_attr: AttributeData,
    pub(crate) parser: ParserState,
    pub(crate) next_link_id: u32,
    interim: u16,
    wedged: bool,
    csi_handlers: HashMap<i32, Vec<CsiHandler>>,
    osc_handlers: HashMap<u32, Vec<OscHandler>>,
    title_listeners: Vec<TitleListener>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Terminal")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("wedged", &self.wedged)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// `new Terminal({ rows, cols, scrollback: 1000, allowProposedApi: true })`.
    #[must_use]
    pub fn new(cols: u32, rows: u32) -> Self {
        let cols = i64::from(cols).max(MINIMUM_COLS);
        let rows = i64::from(rows).max(MINIMUM_ROWS);
        Self {
            cols,
            rows,
            buffers: BufferSet::new(cols, rows),
            modes: Modes::default(),
            cursor_hidden: false,
            convert_eol: false,
            charsets: CharsetService::default(),
            cur_attr: AttributeData::default(),
            erase_attr: AttributeData::default(),
            parser: ParserState::new(),
            next_link_id: 1,
            interim: 0,
            wedged: false,
            csi_handlers: HashMap::new(),
            osc_handlers: HashMap::new(),
            title_listeners: Vec::new(),
        }
    }

    #[must_use]
    pub fn cols(&self) -> i64 {
        self.cols
    }

    #[must_use]
    pub fn rows(&self) -> i64 {
        self.rows
    }

    /// `coreService.decPrivateModes.cursorStyle`.
    #[must_use]
    pub fn cursor_style(&self) -> Option<CursorStyle> {
        self.modes.cursor_style
    }

    /// `coreService.decPrivateModes.cursorBlink`.
    #[must_use]
    pub fn cursor_blink(&self) -> Option<bool> {
        self.modes.cursor_blink
    }

    /// `coreService.isCursorHidden`.
    #[must_use]
    pub fn is_cursor_hidden(&self) -> bool {
        self.cursor_hidden
    }

    /// Whether an earlier write threw, so no write is parsed any more.
    #[must_use]
    pub fn is_wedged(&self) -> bool {
        self.wedged
    }

    /// `terminal.write(data)` up to its callback: parses `data` now. Paseo
    /// awaits each write before the next state read, so a synchronous parse
    /// gives the same states.
    ///
    /// # Errors
    ///
    /// [`Exception`] when parsing threw, now or in an earlier write; the
    /// write callback would never run.
    pub fn write(&mut self, data: &str) -> Result<(), Exception> {
        if self.wedged {
            return Err(Exception);
        }
        let units: Vec<u16> = data.encode_utf16().collect();
        for chunk in units.chunks(MAX_PARSEBUFFER_LENGTH) {
            let decoded = self.decode_utf16(chunk);
            if self.parse(&decoded).is_err() {
                self.wedged = true;
                return Err(Exception);
            }
        }
        Ok(())
    }

    /// `terminal.resize(cols, rows)`.
    ///
    /// # Errors
    ///
    /// [`Exception`] when the reflow threw part way; the buffers keep the
    /// partial resize, as xterm's do.
    pub fn resize(&mut self, cols: u32, rows: u32) -> Result<(), Exception> {
        let cols = i64::from(cols);
        let rows = i64::from(rows);
        if cols == self.cols && rows == self.rows {
            return Ok(());
        }
        let cols = cols.max(MINIMUM_COLS);
        let rows = rows.max(MINIMUM_ROWS);
        self.cols = cols;
        self.rows = rows;
        self.buffers.resize(cols, rows).map_err(|Throw| Exception)
    }

    /// `terminal.parser.registerCsiHandler({ prefix, final }, handler)`. The
    /// handler gets `params.toArray()`; returning true stops the handlers
    /// registered before it, xterm's own included.
    ///
    /// # Errors
    ///
    /// [`InvalidIdentifier`] for a prefix outside `<`..`?` or a final
    /// outside `@`..`~`.
    pub fn register_csi_handler(
        &mut self,
        prefix: Option<u8>,
        final_byte: u8,
        handler: impl FnMut(&[Param], HandlerCursor) -> bool + 'static,
    ) -> Result<(), InvalidIdentifier> {
        if prefix.is_some_and(|prefix| !(0x3c..=0x3f).contains(&prefix))
            || !(0x40..=0x7e).contains(&final_byte)
        {
            return Err(InvalidIdentifier);
        }
        let ident = (i32::from(prefix.unwrap_or(0)) << 8) | i32::from(final_byte);
        // A plain `t` handler is gated on `windowOptions`, all off by default,
        // so it always reports handled without running the callback.
        let handler: CsiHandler = if ident == i32::from(b't') {
            drop(handler);
            Box::new(|_: &[Param], _: HandlerCursor| true)
        } else {
            Box::new(handler)
        };
        self.csi_handlers.entry(ident).or_default().push(handler);
        Ok(())
    }

    /// `terminal.parser.registerOscHandler(ident, handler)`.
    pub fn register_osc_handler(
        &mut self,
        ident: u32,
        handler: impl FnMut(&str) -> bool + 'static,
    ) {
        self.osc_handlers
            .entry(ident)
            .or_default()
            .push(Box::new(handler));
    }

    /// `terminal.onTitleChange(listener)`.
    pub fn on_title_change(&mut self, listener: impl FnMut(&str) + 'static) {
        self.title_listeners.push(Box::new(listener));
    }

    pub(crate) fn fire_title_change(&mut self, title: &str) {
        for listener in &mut self.title_listeners {
            listener(title);
        }
    }

    /// `StringToUtf32.decode`: joins surrogate pairs split across slices and
    /// drops U+FEFF.
    fn decode_utf16(&mut self, input: &[u16]) -> Vec<u32> {
        let mut target = Vec::with_capacity(input.len());
        if input.is_empty() {
            return target;
        }
        let mut start = 0;
        if self.interim != 0 {
            let second = input[0];
            start = 1;
            if (0xDC00..=0xDFFF).contains(&second) {
                target.push(combine_surrogates(self.interim, second));
            } else {
                target.push(u32::from(self.interim));
                target.push(u32::from(second));
            }
            self.interim = 0;
        }
        let mut index = start;
        while index < input.len() {
            let code = input[index];
            if (0xD800..=0xDBFF).contains(&code) {
                index += 1;
                if index >= input.len() {
                    self.interim = code;
                    return target;
                }
                let second = input[index];
                if (0xDC00..=0xDFFF).contains(&second) {
                    target.push(combine_surrogates(code, second));
                } else {
                    target.push(u32::from(code));
                    target.push(u32::from(second));
                }
            } else if code != 0xFEFF {
                target.push(u32::from(code));
            }
            index += 1;
        }
        target
    }

    /// `EscapeSequenceParser.parse` over one decoded slice.
    #[allow(clippy::too_many_lines)]
    fn parse(&mut self, data: &[u32]) -> Result<(), Throw> {
        let table = &*VT500_TRANSITION_TABLE;
        let length = data.len();
        let mut i = 0;
        while i < length {
            let mut code = data[i];
            let column = if code < 0xa0 {
                code
            } else {
                NON_ASCII_PRINTABLE
            };
            let mut transition =
                table[(usize::from(self.parser.current_state) << 8) | column as usize];
            match transition >> TRANSITION_ACTION_SHIFT {
                PRINT => {
                    let mut j = i + 1;
                    while j < length {
                        code = data[j];
                        if code < 0x20 || (code > 0x7e && code < NON_ASCII_PRINTABLE) {
                            break;
                        }
                        j += 1;
                    }
                    self.print(&|index| data[index], i, j)?;
                    i = j - 1;
                }
                EXECUTE => {
                    self.execute(code)?;
                    self.parser.preceding_join_state = 0;
                }
                CSI_DISPATCH => {
                    self.csi_dispatch(code)?;
                    self.parser.preceding_join_state = 0;
                }
                PARAM => {
                    loop {
                        match code {
                            0x3b => self.parser.params.add_param(0),
                            0x3a => self.parser.params.add_sub_param(-1),
                            _ => self.parser.params.add_digit(digit_value(code)),
                        }
                        i += 1;
                        if i >= length {
                            break;
                        }
                        code = data[i];
                        if !(code > 0x2f && code < 0x3c) {
                            break;
                        }
                    }
                    i -= 1;
                }
                COLLECT => {
                    self.parser.collect = self.parser.collect.wrapping_shl(8) | code_as_i32(code);
                }
                ESC_DISPATCH => {
                    self.esc_dispatch(code)?;
                    self.parser.preceding_join_state = 0;
                }
                CLEAR => self.parser.clear(),
                DCS_PUT => {
                    let mut j = i + 1;
                    while j < length {
                        code = data[j];
                        if code == 0x18
                            || code == 0x1a
                            || code == 0x1b
                            || (code > 0x7f && code < NON_ASCII_PRINTABLE)
                        {
                            break;
                        }
                        j += 1;
                    }
                    i = j - 1;
                }
                DCS_UNHOOK => {
                    if code == 0x1b {
                        transition |= ESCAPE;
                    }
                    self.parser.clear();
                    self.parser.preceding_join_state = 0;
                }
                OSC_START => self.parser.osc.start(),
                OSC_PUT => {
                    let mut j = i + 1;
                    while j < length {
                        code = data[j];
                        if code < 0x20 || (code > 0x7f && code < NON_ASCII_PRINTABLE) {
                            break;
                        }
                        j += 1;
                    }
                    let handlers = &self.osc_handlers;
                    self.parser
                        .osc
                        .put(&data[i..j], &|id| has_osc_handler(handlers, id));
                    i = j - 1;
                }
                OSC_END => {
                    self.osc_end(code != 0x18 && code != 0x1a);
                    if code == 0x1b {
                        transition |= ESCAPE;
                    }
                    self.parser.clear();
                    self.parser.preceding_join_state = 0;
                }
                // IGNORE; ERROR, whose handler only logs and never aborts;
                // DCS_HOOK, as DCS payloads reach no handler with an
                // observable effect.
                _ => {}
            }
            self.parser.current_state = transition & TRANSITION_STATE_MASK;
            i += 1;
        }
        Ok(())
    }

    fn execute(&mut self, code: u32) -> Result<(), Throw> {
        match code {
            0x0a..=0x0c => self.line_feed(),
            0x0d => {
                self.buffers.active_mut().x = 0;
                Ok(())
            }
            0x08 => self.backspace(),
            0x09 => {
                self.tab();
                Ok(())
            }
            0x0e => {
                self.charsets.set_glevel(1);
                Ok(())
            }
            0x0f => {
                self.charsets.set_glevel(0);
                Ok(())
            }
            0x84 => self.index(),
            0x85 => self.next_line(),
            0x88 => {
                self.tab_set();
                Ok(())
            }
            // BEL only notifies; other codes fall back to a debug log.
            _ => Ok(()),
        }
    }

    fn csi_dispatch(&mut self, final_code: u32) -> Result<(), Throw> {
        let ident = self.parser.collect.wrapping_shl(8) | code_as_i32(final_code);
        if let Some(handlers) = self.csi_handlers.get_mut(&ident) {
            let params = self.parser.params.to_array();
            let buffer = self.buffers.active();
            let cursor = HandlerCursor {
                x: buffer.x,
                y: buffer.y,
            };
            for handler in handlers.iter_mut().rev() {
                if handler(&params, cursor) {
                    return Ok(());
                }
            }
        }
        self.csi_builtin(ident)
    }

    /// The `InputHandler` CSI table. Unregistered identifiers fall back to a
    /// debug log.
    fn csi_builtin(&mut self, ident: i32) -> Result<(), Throw> {
        let collect = ident >> 8;
        let Ok(final_byte) = u8::try_from(ident & 0xFF) else {
            return Ok(());
        };
        let params = self.parser.params.clone();
        match (collect, final_byte) {
            (0, b'@') => self.insert_chars(&params),
            (0x20, b'@') => self.scroll_left(&params)?,
            (0, b'A') => self.cursor_up(&params),
            (0x20, b'A') => self.scroll_right(&params)?,
            (0, b'B') => self.cursor_down(&params),
            (0, b'C' | b'a') => self.cursor_forward(&params),
            (0, b'D') => self.cursor_backward(&params),
            (0, b'E') => self.cursor_next_line(&params),
            (0, b'F') => self.cursor_preceding_line(&params),
            (0, b'G' | b'`') => self.cursor_char_absolute(&params),
            (0, b'H' | b'f') => self.cursor_position(&params),
            (0, b'I') => self.cursor_forward_tab(&params),
            (0, b'J') => self.erase_in_display(&params, false)?,
            (0x3f, b'J') => self.erase_in_display(&params, true)?,
            (0, b'K') => self.erase_in_line(&params, false)?,
            (0x3f, b'K') => self.erase_in_line(&params, true)?,
            (0, b'L') => self.insert_lines(&params),
            (0, b'M') => self.delete_lines(&params),
            (0, b'P') => self.delete_chars(&params),
            (0, b'S') => self.scroll_up(&params),
            (0, b'T') => self.scroll_down(&params),
            (0, b'X') => self.erase_chars(&params),
            (0, b'Z') => self.cursor_backward_tab(&params),
            (0, b'b') => self.repeat_preceding_character(&params)?,
            (0, b'd') => self.line_pos_absolute(&params),
            (0, b'e') => self.v_position_relative(&params),
            (0, b'g') => self.tab_clear(&params),
            (0, b'h') => self.set_mode(&params),
            (0x3f, b'h') => self.set_mode_private(&params),
            (0, b'l') => self.reset_mode(&params),
            (0x3f, b'l') => self.reset_mode_private(&params),
            (0, b'm') => self.char_attributes(&params),
            (0x21, b'p') => self.soft_reset(),
            (0x20, b'q') => self.set_cursor_style(&params),
            (0, b'r') => self.set_scroll_region(&params),
            (0, b's') => self.save_cursor(),
            (0, b'u') => self.restore_cursor(),
            (0x27, b'}') => self.insert_columns(&params)?,
            (0x27, b'~') => self.delete_columns(&params)?,
            (0x22, b'q') => self.select_protected(&params),
            // DA1, DA2, DSR, DECDSR, DECRQM and the window ops only answer
            // through onData or are disabled by windowOptions.
            _ => {}
        }
        Ok(())
    }

    fn esc_dispatch(&mut self, final_code: u32) -> Result<(), Throw> {
        let collect = self.parser.collect;
        let Ok(final_byte) = u8::try_from(final_code) else {
            return Ok(());
        };
        match (collect, final_byte) {
            (0, b'7') => self.save_cursor(),
            (0, b'8') => self.restore_cursor(),
            (0, b'D') => self.index()?,
            (0, b'E') => self.next_line()?,
            (0, b'H') => self.tab_set(),
            (0, b'M') => self.reverse_index()?,
            (0, b'c') => self.full_reset(),
            (0, b'n' | b'}') => self.charsets.set_glevel(2),
            (0, b'o' | b'|') => self.charsets.set_glevel(3),
            (0, b'~') => self.charsets.set_glevel(1),
            (0x25, b'@' | b'G') => self.select_default_charset(),
            (0x23, b'8') => self.screen_alignment_pattern(),
            (0x28 | 0x29 | 0x2a | 0x2b | 0x2d | 0x2e | 0x2f, flag) if Charset::is_flag(flag) => {
                self.select_charset(collect, Charset::from_flag(flag));
            }
            // ESC \ (ST) is swallowed; DECKPAM and DECKPNM only notify; the
            // rest falls back to a debug log.
            _ => {}
        }
        Ok(())
    }

    fn osc_end(&mut self, success: bool) {
        let handlers = &self.osc_handlers;
        let command = self
            .parser
            .osc
            .end(success, &|id| has_osc_handler(handlers, id));
        let Some(command) = command else {
            return;
        };
        let Some(data) = command.data else {
            return;
        };
        let Some(id) = osc_ident(command.id) else {
            return;
        };
        if let Some(list) = self.osc_handlers.get_mut(&id) {
            for handler in list.iter_mut().rev() {
                if handler(&data) {
                    return;
                }
            }
        }
        match id {
            0 | 2 => self.set_title(&data),
            8 => self.set_hyperlink(&data),
            // Icon name, colors and color resets only notify.
            _ => {}
        }
    }

    /// `fullReset` (RIS): the parser reset, then `Terminal.reset`.
    fn full_reset(&mut self) {
        self.parser.reset();
        self.cur_attr = AttributeData::default();
        self.erase_attr = AttributeData::default();
        self.buffers = BufferSet::new(self.cols, self.rows);
        self.charsets = CharsetService::default();
        self.modes = Modes::default();
    }
}

fn combine_surrogates(high: u16, low: u16) -> u32 {
    (u32::from(high) - 0xD800) * 0x400 + u32::from(low) - 0xDC00 + 0x10000
}

/// The value of an ASCII digit byte in a parameter run.
fn digit_value(code: u32) -> i32 {
    code_as_i32(code) - 48
}

/// Parser codes are below U+110000, so they fit an `i32`.
fn code_as_i32(code: u32) -> i32 {
    i32::try_from(code).unwrap_or(i32::MAX)
}

/// The handler table key for an OSC identifier, which JavaScript computes
/// as a double: only exact non-negative integers can match a registration.
fn osc_ident(id: f64) -> Option<u32> {
    if id >= 0.0 && id <= f64::from(u32::MAX) && id.fract() == 0.0 {
        format!("{id:.0}").parse().ok()
    } else {
        None
    }
}

fn has_osc_handler(handlers: &HashMap<u32, Vec<OscHandler>>, id: f64) -> bool {
    osc_ident(id).is_some_and(|id| {
        BUILTIN_OSC.contains(&id) || handlers.get(&id).is_some_and(|list| !list.is_empty())
    })
}
