//! Read views of the active buffer, shaped after the `buffer.active`,
//! `getLine` and `getCell` API that Paseo's `terminal.ts` walks.
//!
//! Ported from xterm.js 6.0.0 `src/common/public/BufferApiView.ts`,
//! `src/common/public/BufferLineApiView.ts` and the getters of
//! `src/common/buffer/AttributeData.ts`.
//! Copyright (c) 2021 The xterm.js authors. MIT License.

use crate::attributes::{
    BG_DIM, BG_ITALIC, CM_MASK, CM_P16, CM_P256, CM_RGB, CellData, FG_BOLD, FG_INVERSE,
    FG_STRIKETHROUGH, PCOLOR_MASK, RGB_MASK,
};
use crate::buffer::Buffer;
use crate::buffer_line::LineRef;
use crate::terminal::Terminal;

impl Terminal {
    /// `terminal.buffer.active`.
    #[must_use]
    pub fn buffer(&self) -> BufferView<'_> {
        BufferView {
            buffer: self.buffers.active(),
        }
    }
}

/// `IBuffer` of the active buffer.
#[derive(Debug, Clone, Copy)]
pub struct BufferView<'a> {
    buffer: &'a Buffer,
}

impl BufferView<'_> {
    #[must_use]
    pub fn cursor_x(&self) -> i64 {
        self.buffer.x
    }

    #[must_use]
    pub fn cursor_y(&self) -> i64 {
        self.buffer.y
    }

    #[must_use]
    pub fn viewport_y(&self) -> i64 {
        self.buffer.ydisp
    }

    #[must_use]
    pub fn base_y(&self) -> i64 {
        self.buffer.ybase
    }

    /// `lines.length`, scrollback included.
    #[must_use]
    pub fn length(&self) -> i64 {
        self.buffer.lines.length()
    }

    /// `getLine(y)`: like xterm, a `y` past `length` can still return the
    /// line a ring slot last held.
    #[must_use]
    pub fn get_line(&self, y: i64) -> Option<LineView> {
        self.buffer.lines.get(y).map(|line| LineView { line })
    }
}

/// `IBufferLine`; it sees later changes to the line, as xterm's view does.
#[derive(Debug, Clone)]
pub struct LineView {
    line: LineRef,
}

impl LineView {
    /// Whether this line continues the line before it.
    #[must_use]
    pub fn is_wrapped(&self) -> bool {
        self.line.borrow().is_wrapped
    }

    #[must_use]
    pub fn length(&self) -> i64 {
        self.line.borrow().length()
    }

    /// `getCell(x)`: `None` outside the line.
    #[must_use]
    pub fn get_cell(&self, x: i64) -> Option<CellView> {
        let line = self.line.borrow();
        if x < 0 || x >= line.length() {
            return None;
        }
        let mut cell = CellData::default();
        line.load_cell(x, &mut cell);
        Some(CellView { cell })
    }

    /// `translateToString(trimRight)` over the whole line.
    #[must_use]
    pub fn translate_to_string(&self, trim_right: bool) -> String {
        self.line
            .borrow()
            .translate_to_string(trim_right, None, None)
    }
}

/// `IBufferCell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellView {
    cell: CellData,
}

impl CellView {
    /// `getChars()`: empty for a null cell or the right half of a wide one.
    #[must_use]
    pub fn chars(&self) -> String {
        self.cell.chars()
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.cell.width()
    }

    /// `getFgColorMode()`: 0, `0x1000000` (16 colors), `0x2000000` (256)
    /// or `0x3000000` (RGB).
    #[must_use]
    pub fn fg_color_mode(&self) -> u32 {
        self.cell.attr.fg & CM_MASK
    }

    #[must_use]
    pub fn bg_color_mode(&self) -> u32 {
        self.cell.attr.bg & CM_MASK
    }

    /// `getFgColor()`: the palette index or RGB value, -1 for default.
    #[must_use]
    pub fn fg_color(&self) -> i64 {
        color(self.cell.attr.fg)
    }

    #[must_use]
    pub fn bg_color(&self) -> i64 {
        color(self.cell.attr.bg)
    }

    #[must_use]
    pub fn is_bold(&self) -> bool {
        self.cell.attr.fg & FG_BOLD != 0
    }

    #[must_use]
    pub fn is_italic(&self) -> bool {
        self.cell.attr.bg & BG_ITALIC != 0
    }

    #[must_use]
    pub fn is_underline(&self) -> bool {
        self.cell.is_underline()
    }

    #[must_use]
    pub fn is_dim(&self) -> bool {
        self.cell.attr.bg & BG_DIM != 0
    }

    #[must_use]
    pub fn is_inverse(&self) -> bool {
        self.cell.attr.fg & FG_INVERSE != 0
    }

    #[must_use]
    pub fn is_strikethrough(&self) -> bool {
        self.cell.attr.fg & FG_STRIKETHROUGH != 0
    }
}

fn color(value: u32) -> i64 {
    match value & CM_MASK {
        CM_P16 | CM_P256 => i64::from(value & PCOLOR_MASK),
        CM_RGB => i64::from(value & RGB_MASK),
        _ => -1,
    }
}
