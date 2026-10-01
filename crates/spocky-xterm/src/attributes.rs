//! Cell attribute encoding and the cell value type.
//!
//! Ported from xterm.js 6.0.0 `src/common/buffer/Constants.ts`,
//! `src/common/buffer/AttributeData.ts` and `src/common/buffer/CellData.ts`.
//! Copyright (c) 2018 The xterm.js authors. MIT License.

pub(crate) const CODEPOINT_MASK: u32 = 0x1F_FFFF;
pub(crate) const IS_COMBINED_MASK: u32 = 0x20_0000;
pub(crate) const HAS_CONTENT_MASK: u32 = 0x3F_FFFF;
pub(crate) const WIDTH_MASK: u32 = 0xC0_0000;
pub(crate) const WIDTH_SHIFT: u32 = 22;

pub(crate) const PCOLOR_MASK: u32 = 0xFF;
pub(crate) const RGB_MASK: u32 = 0xFF_FFFF;
pub(crate) const CM_MASK: u32 = 0x300_0000;
pub(crate) const CM_P16: u32 = 0x100_0000;
pub(crate) const CM_P256: u32 = 0x200_0000;
pub(crate) const CM_RGB: u32 = 0x300_0000;

pub(crate) const FG_INVERSE: u32 = 0x400_0000;
pub(crate) const FG_BOLD: u32 = 0x800_0000;
pub(crate) const FG_UNDERLINE: u32 = 0x1000_0000;
pub(crate) const FG_BLINK: u32 = 0x2000_0000;
pub(crate) const FG_INVISIBLE: u32 = 0x4000_0000;
pub(crate) const FG_STRIKETHROUGH: u32 = 0x8000_0000;

pub(crate) const BG_ITALIC: u32 = 0x400_0000;
pub(crate) const BG_DIM: u32 = 0x800_0000;
pub(crate) const BG_HAS_EXTENDED: u32 = 0x1000_0000;
pub(crate) const BG_PROTECTED: u32 = 0x2000_0000;
pub(crate) const BG_OVERLINE: u32 = 0x4000_0000;

const EXT_UNDERLINE_STYLE: u32 = 0x1C00_0000;

pub(crate) const UNDERLINE_NONE: u32 = 0;
pub(crate) const UNDERLINE_SINGLE: u32 = 1;
pub(crate) const UNDERLINE_DOUBLE: u32 = 2;
const UNDERLINE_DASHED: u32 = 5;

/// Underline style and color plus the OSC 8 link id of a cell.
///
/// xterm shares these objects by reference, but every writer clones before
/// it mutates, so a value copy behaves the same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExtendedAttrs {
    ext: u32,
    pub(crate) url_id: u32,
}

impl ExtendedAttrs {
    pub(crate) fn underline_style(self) -> u32 {
        if self.url_id != 0 {
            return UNDERLINE_DASHED;
        }
        (self.ext & EXT_UNDERLINE_STYLE) >> 26
    }

    pub(crate) fn set_underline_style(&mut self, value: u32) {
        self.ext &= !EXT_UNDERLINE_STYLE;
        self.ext |= (value << 26) & EXT_UNDERLINE_STYLE;
    }

    pub(crate) fn underline_color(self) -> u32 {
        self.ext & (CM_MASK | RGB_MASK)
    }

    pub(crate) fn set_underline_color(&mut self, value: u32) {
        self.ext &= !(CM_MASK | RGB_MASK);
        self.ext |= value & (CM_MASK | RGB_MASK);
    }

    fn is_empty(self) -> bool {
        self.underline_style() == UNDERLINE_NONE && self.url_id == 0
    }
}

/// Foreground, background and extended attributes of a cell or the cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttributeData {
    pub(crate) fg: u32,
    pub(crate) bg: u32,
    pub(crate) extended: ExtendedAttrs,
}

impl AttributeData {
    pub(crate) fn from_color_rgb(red: i32, green: i32, blue: i32) -> u32 {
        // Each channel keeps its low byte, as `& 255` does in JavaScript.
        let channel = |value: i32| u32::from(value.to_le_bytes()[0]);
        (channel(red) << 16) | (channel(green) << 8) | channel(blue)
    }

    pub(crate) fn update_extended(&mut self) {
        if self.extended.is_empty() {
            self.bg &= !BG_HAS_EXTENDED;
        } else {
            self.bg |= BG_HAS_EXTENDED;
        }
    }
}

/// A cell loaded from a buffer line: content word, attributes, and the
/// combined string when the content marks the cell as combined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CellData {
    pub(crate) content: u32,
    pub(crate) attr: AttributeData,
    pub(crate) combined_data: String,
}

impl CellData {
    /// The null cell (`[0, '', 1, 0]`) with the given attributes.
    pub(crate) fn null_with(attr: AttributeData) -> Self {
        Self {
            content: 1 << WIDTH_SHIFT,
            attr,
            combined_data: String::new(),
        }
    }

    pub(crate) fn width(&self) -> u32 {
        self.content >> WIDTH_SHIFT
    }

    pub(crate) fn chars(&self) -> String {
        if self.content & IS_COMBINED_MASK != 0 {
            return self.combined_data.clone();
        }
        let code = self.content & CODEPOINT_MASK;
        if code != 0 {
            return string_from_code_point(code);
        }
        String::new()
    }

    pub(crate) fn is_underline(&self) -> bool {
        if self.attr.bg & BG_HAS_EXTENDED != 0
            && self.attr.extended.underline_style() != UNDERLINE_NONE
        {
            return true;
        }
        self.attr.fg & FG_UNDERLINE != 0
    }
}

/// `stringFromCodePoint`; every code point that reaches a cell is a scalar.
pub(crate) fn string_from_code_point(code: u32) -> String {
    char::from_u32(code).map_or_else(String::new, String::from)
}
