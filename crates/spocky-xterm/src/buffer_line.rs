//! One buffer row: packed cells plus sparse combined strings and extended
//! attributes.
//!
//! Ported from xterm.js 6.0.0 `src/common/buffer/BufferLine.ts`.
//! Copyright (c) 2018 The xterm.js authors. MIT License.
//!
//! Positions are `i64` and cell words follow typed-array rules: reading
//! outside the row gives 0 and writing outside it is ignored, while the
//! sparse maps still record entries there, exactly like the JavaScript
//! objects. The maps are never pruned except where xterm prunes them, so a
//! stale combined string can resurface through `copy_cells_from` as it does
//! in xterm.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::attributes::{
    AttributeData, BG_HAS_EXTENDED, BG_PROTECTED, CODEPOINT_MASK, CellData, ExtendedAttrs,
    HAS_CONTENT_MASK, IS_COMBINED_MASK, WIDTH_MASK, WIDTH_SHIFT, string_from_code_point,
};

const CELL_SIZE: i64 = 3;

/// Shared reference to a line; the circular list can hold one line in two
/// slots, as JavaScript arrays of objects do.
pub(crate) type LineRef = Rc<RefCell<BufferLine>>;

#[derive(Debug, Clone, Default)]
pub(crate) struct BufferLine {
    data: Vec<u32>,
    combined: BTreeMap<i64, String>,
    extended: BTreeMap<i64, ExtendedAttrs>,
    length: i64,
    pub(crate) is_wrapped: bool,
}

impl BufferLine {
    pub(crate) fn new(cols: i64, fill: &CellData, is_wrapped: bool) -> Self {
        let mut line = Self {
            data: vec![0; usize::try_from(cols * CELL_SIZE).unwrap_or(0)],
            combined: BTreeMap::new(),
            extended: BTreeMap::new(),
            length: cols,
            is_wrapped,
        };
        for index in 0..cols {
            line.set_cell(index, fill);
        }
        line
    }

    pub(crate) fn into_ref(self) -> LineRef {
        Rc::new(RefCell::new(self))
    }

    pub(crate) fn length(&self) -> i64 {
        self.length
    }

    fn read(&self, word: i64) -> u32 {
        usize::try_from(word)
            .ok()
            .and_then(|word| self.data.get(word))
            .copied()
            .unwrap_or(0)
    }

    fn write(&mut self, word: i64, value: u32) {
        if let Some(slot) = usize::try_from(word)
            .ok()
            .and_then(|word| self.data.get_mut(word))
        {
            *slot = value;
        }
    }

    fn content(&self, index: i64) -> u32 {
        self.read(index * CELL_SIZE)
    }

    pub(crate) fn get_width(&self, index: i64) -> u32 {
        self.content(index) >> WIDTH_SHIFT
    }

    pub(crate) fn has_width(&self, index: i64) -> bool {
        self.content(index) & WIDTH_MASK != 0
    }

    pub(crate) fn get_bg(&self, index: i64) -> u32 {
        self.read(index * CELL_SIZE + 2)
    }

    pub(crate) fn has_content(&self, index: i64) -> bool {
        self.content(index) & HAS_CONTENT_MASK != 0
    }

    /// `getString`: `None` stands for JavaScript `undefined`, a combined
    /// cell whose string entry is missing.
    pub(crate) fn get_string(&self, index: i64) -> Option<String> {
        let content = self.content(index);
        if content & IS_COMBINED_MASK != 0 {
            return self.combined.get(&index).cloned();
        }
        let code = content & CODEPOINT_MASK;
        if code != 0 {
            return Some(string_from_code_point(code));
        }
        Some(String::new())
    }

    fn is_protected(&self, index: i64) -> bool {
        self.get_bg(index) & BG_PROTECTED != 0
    }

    pub(crate) fn load_cell(&self, index: i64, cell: &mut CellData) {
        let start = index * CELL_SIZE;
        cell.content = self.read(start);
        cell.attr.fg = self.read(start + 1);
        cell.attr.bg = self.read(start + 2);
        if cell.content & IS_COMBINED_MASK != 0 {
            cell.combined_data = self.combined.get(&index).cloned().unwrap_or_default();
        }
        if cell.attr.bg & BG_HAS_EXTENDED != 0 {
            cell.attr.extended = self.extended.get(&index).copied().unwrap_or_default();
        }
    }

    pub(crate) fn set_cell(&mut self, index: i64, cell: &CellData) {
        if cell.content & IS_COMBINED_MASK != 0 {
            self.combined.insert(index, cell.combined_data.clone());
        }
        if cell.attr.bg & BG_HAS_EXTENDED != 0 {
            self.extended.insert(index, cell.attr.extended);
        }
        let start = index * CELL_SIZE;
        self.write(start, cell.content);
        self.write(start + 1, cell.attr.fg);
        self.write(start + 2, cell.attr.bg);
    }

    pub(crate) fn set_cell_from_codepoint(
        &mut self,
        index: i64,
        code_point: u32,
        width: u32,
        attrs: &AttributeData,
    ) {
        if attrs.bg & BG_HAS_EXTENDED != 0 {
            self.extended.insert(index, attrs.extended);
        }
        let start = index * CELL_SIZE;
        self.write(start, code_point | (width << WIDTH_SHIFT));
        self.write(start + 1, attrs.fg);
        self.write(start + 2, attrs.bg);
    }

    pub(crate) fn add_codepoint_to_cell(&mut self, index: i64, code_point: u32, width: u32) {
        let mut content = self.content(index);
        if content & IS_COMBINED_MASK != 0 {
            self.combined
                .entry(index)
                .or_default()
                .push_str(&string_from_code_point(code_point));
        } else if content & CODEPOINT_MASK != 0 {
            let mut joined = string_from_code_point(content & CODEPOINT_MASK);
            joined.push_str(&string_from_code_point(code_point));
            self.combined.insert(index, joined);
            content &= !CODEPOINT_MASK;
            content |= IS_COMBINED_MASK;
        } else {
            content = code_point | (1 << WIDTH_SHIFT);
        }
        if width != 0 {
            content &= !WIDTH_MASK;
            content |= width << WIDTH_SHIFT;
        }
        self.write(index * CELL_SIZE, content);
    }

    pub(crate) fn insert_cells(&mut self, pos: i64, count: i64, fill: &CellData) {
        let pos = pos % self.length;
        if pos != 0 && self.get_width(pos - 1) == 2 {
            self.set_cell_from_codepoint(pos - 1, 0, 1, &fill.attr);
        }
        if count < self.length - pos {
            let mut cell = CellData::default();
            let mut index = self.length - pos - count - 1;
            while index >= 0 {
                self.load_cell(pos + index, &mut cell);
                self.set_cell(pos + count + index, &cell);
                index -= 1;
            }
            for index in 0..count {
                self.set_cell(pos + index, fill);
            }
        } else {
            for index in pos..self.length {
                self.set_cell(index, fill);
            }
        }
        if self.get_width(self.length - 1) == 2 {
            self.set_cell_from_codepoint(self.length - 1, 0, 1, &fill.attr);
        }
    }

    pub(crate) fn delete_cells(&mut self, pos: i64, count: i64, fill: &CellData) {
        let pos = pos % self.length;
        if count < self.length - pos {
            let mut cell = CellData::default();
            for index in 0..self.length - pos - count {
                self.load_cell(pos + count + index, &mut cell);
                self.set_cell(pos + index, &cell);
            }
            for index in self.length - count..self.length {
                self.set_cell(index, fill);
            }
        } else {
            for index in pos..self.length {
                self.set_cell(index, fill);
            }
        }
        if pos != 0 && self.get_width(pos - 1) == 2 {
            self.set_cell_from_codepoint(pos - 1, 0, 1, &fill.attr);
        }
        if self.get_width(pos) == 0 && !self.has_content(pos) {
            self.set_cell_from_codepoint(pos, 0, 1, &fill.attr);
        }
    }

    pub(crate) fn replace_cells(
        &mut self,
        mut start: i64,
        end: i64,
        fill: &CellData,
        respect_protect: bool,
    ) {
        if respect_protect {
            if start != 0 && self.get_width(start - 1) == 2 && !self.is_protected(start - 1) {
                self.set_cell_from_codepoint(start - 1, 0, 1, &fill.attr);
            }
            if end < self.length && self.get_width(end - 1) == 2 && !self.is_protected(end) {
                self.set_cell_from_codepoint(end, 0, 1, &fill.attr);
            }
            while start < end && start < self.length {
                if !self.is_protected(start) {
                    self.set_cell(start, fill);
                }
                start += 1;
            }
            return;
        }
        if start != 0 && self.get_width(start - 1) == 2 {
            self.set_cell_from_codepoint(start - 1, 0, 1, &fill.attr);
        }
        if end < self.length && self.get_width(end - 1) == 2 {
            self.set_cell_from_codepoint(end, 0, 1, &fill.attr);
        }
        while start < end && start < self.length {
            self.set_cell(start, fill);
            start += 1;
        }
    }

    pub(crate) fn resize(&mut self, cols: i64, fill: &CellData) {
        if cols == self.length {
            return;
        }
        let words = usize::try_from(cols * CELL_SIZE).unwrap_or(0);
        if cols > self.length {
            self.data.resize(words, 0);
            for index in self.length..cols {
                self.set_cell(index, fill);
            }
        } else {
            self.data.truncate(words);
            self.combined.retain(|&key, _| key < cols);
            self.extended.retain(|&key, _| key < cols);
        }
        self.length = cols;
    }

    pub(crate) fn fill(&mut self, fill: &CellData, respect_protect: bool) {
        if respect_protect {
            for index in 0..self.length {
                if !self.is_protected(index) {
                    self.set_cell(index, fill);
                }
            }
            return;
        }
        self.combined.clear();
        self.extended.clear();
        for index in 0..self.length {
            self.set_cell(index, fill);
        }
    }

    pub(crate) fn copy_from(&mut self, line: &Self) {
        self.clone_from(line);
    }

    pub(crate) fn get_trimmed_length(&self) -> i64 {
        let mut index = self.length - 1;
        while index >= 0 {
            let content = self.content(index);
            if content & HAS_CONTENT_MASK != 0 {
                return index + i64::from(content >> WIDTH_SHIFT);
            }
            index -= 1;
        }
        0
    }

    /// `copyCellsFrom`. `src` is `None` when the source is this same line;
    /// the copy then reads cells it may already have overwritten, as the
    /// in-place JavaScript loop does.
    pub(crate) fn copy_cells_from(
        &mut self,
        src: Option<&Self>,
        src_col: i64,
        dest_col: i64,
        length: i64,
        apply_in_reverse: bool,
    ) {
        let cells: Vec<i64> = if apply_in_reverse {
            (0..length).rev().collect()
        } else {
            (0..length).collect()
        };
        for cell in cells {
            for word in 0..CELL_SIZE {
                let value = src
                    .unwrap_or(self)
                    .read((src_col + cell) * CELL_SIZE + word);
                self.write((dest_col + cell) * CELL_SIZE + word, value);
            }
            let source = src.unwrap_or(self);
            if source.get_bg(src_col + cell) & BG_HAS_EXTENDED != 0 {
                match source.extended.get(&(src_col + cell)).copied() {
                    Some(value) => self.extended.insert(dest_col + cell, value),
                    None => self.extended.remove(&(dest_col + cell)),
                };
            }
        }
        let keys: Vec<i64> = src.unwrap_or(self).combined.keys().copied().collect();
        for key in keys {
            if key >= src_col {
                let value = src.unwrap_or(self).combined.get(&key).cloned();
                if let Some(value) = value {
                    self.combined.insert(key - src_col + dest_col, value);
                }
            }
        }
    }

    /// `translateToString`; a combined cell without a string entry reads
    /// as JavaScript's `"undefined"`.
    pub(crate) fn translate_to_string(
        &self,
        trim_right: bool,
        start_col: Option<i64>,
        end_col: Option<i64>,
    ) -> String {
        let mut start = start_col.unwrap_or(0);
        let mut end = end_col.unwrap_or(self.length);
        if trim_right {
            end = end.min(self.get_trimmed_length());
        }
        let mut result = String::new();
        while start < end {
            let content = self.content(start);
            let code = content & CODEPOINT_MASK;
            if content & IS_COMBINED_MASK != 0 {
                match self.combined.get(&start) {
                    Some(text) => result.push_str(text),
                    None => result.push_str("undefined"),
                }
            } else if code != 0 {
                result.push_str(&string_from_code_point(code));
            } else {
                result.push(' ');
            }
            let width = i64::from(content >> WIDTH_SHIFT);
            start += if width == 0 { 1 } else { width };
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::BufferLine;
    use crate::attributes::{AttributeData, CellData};

    fn line_with(text: &str, cols: i64) -> BufferLine {
        let mut line = BufferLine::new(cols, &CellData::null_with(AttributeData::default()), false);
        for (index, character) in (0..).zip(text.chars()) {
            line.set_cell_from_codepoint(index, u32::from(character), 1, &AttributeData::default());
        }
        line
    }

    #[test]
    fn inserts_and_deletes_cells() {
        let null = CellData::null_with(AttributeData::default());
        let mut line = line_with("abcdef", 8);
        line.insert_cells(1, 2, &null);
        assert_eq!(line.translate_to_string(true, None, None), "a  bcdef");
        line.delete_cells(0, 3, &null);
        assert_eq!(line.translate_to_string(true, None, None), "bcdef");
    }

    #[test]
    fn combines_and_trims() {
        let mut line = line_with("e", 4);
        line.add_codepoint_to_cell(0, 0x0301, 0);
        assert_eq!(line.get_string(0).as_deref(), Some("e\u{301}"));
        assert_eq!(line.get_trimmed_length(), 1);
        assert_eq!(line.translate_to_string(false, None, None), "e\u{301}   ");
    }

    #[test]
    fn copies_stale_combined_strings_like_xterm() {
        let mut source = line_with("ab", 4);
        source.add_codepoint_to_cell(1, 0x0301, 0);
        source.set_cell_from_codepoint(1, u32::from('c'), 1, &AttributeData::default());
        let mut dest = line_with("xy", 4);
        dest.add_codepoint_to_cell(1, 0x0300, 0);
        dest.copy_cells_from(Some(&source), 0, 0, 1, false);
        assert_eq!(dest.get_string(1).as_deref(), Some("b\u{301}"));
    }
}
