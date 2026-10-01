//! Terminal emulator core: a faithful port of the `@xterm/headless` 6.0.0
//! parser, input handler subset, buffer and buffer lines with scrollback,
//! reflow, Unicode 6 widths, charsets, DEC modes, and cursor state, matched
//! byte for byte against the pinned xterm. xterm.js is MIT licensed; the port
//! keeps its copyright and license notice with the ported sources and in
//! `LICENSE-xterm.js`.
//!
//! The API covers what Paseo's `terminal.ts` uses: [`Terminal::new`] with
//! 1000 lines of scrollback, [`Terminal::write`] and [`Terminal::resize`],
//! custom CSI and OSC handlers, title change listeners, the active buffer's
//! lines and cells, and the core service cursor fields. [`Utf8Decoder`]
//! turns PTY bytes into the text node-pty would hand to `onData`.

mod attributes;
mod buffer;
mod buffer_line;
mod charsets;
mod circular_list;
mod decoder;
mod input_handler;
mod params;
mod parser;
mod terminal;
mod unicode;
mod view;

pub use decoder::Utf8Decoder;
pub use params::Param;
pub use terminal::{CursorStyle, Exception, HandlerCursor, InvalidIdentifier, Terminal};
pub use view::{BufferView, CellView, LineView};
