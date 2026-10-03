//! The `InputHandler` actions that change buffer, cursor, attribute, mode or
//! charset state.
//!
//! Ported from xterm.js 6.0.0 `src/common/InputHandler.ts`.
//! Copyright (c) 2014 The xterm.js authors. MIT License.
//! Copyright (c) 2012-2013, Christopher Jeffrey (MIT License).
//!
//! Each method follows its JavaScript counterpart statement by statement,
//! including where it reads a line that may be `undefined`: such a read
//! throws at the same point (`Throw`), after the same partial updates.

use spocky_contracts::text::js_trim;

use crate::attributes::{
    AttributeData, BG_DIM, BG_ITALIC, BG_OVERLINE, BG_PROTECTED, CM_MASK, CM_P16, CM_P256, CM_RGB,
    CellData, FG_BLINK, FG_BOLD, FG_INVERSE, FG_INVISIBLE, FG_STRIKETHROUGH, FG_UNDERLINE,
    PCOLOR_MASK, RGB_MASK, UNDERLINE_DOUBLE, UNDERLINE_NONE, UNDERLINE_SINGLE, WIDTH_SHIFT,
};
use crate::buffer::{Buffer, copy_cells, req};
use crate::buffer_line::LineRef;
use crate::charsets::Charset;
use crate::circular_list::Throw;
use crate::params::Params;
use crate::terminal::{CharsetService, CursorStyle, Modes, Terminal};
use crate::unicode::{char_properties, extract_should_join, extract_width};

/// `params.params[index] || 1`.
fn param_or_one(params: &Params, index: usize) -> i64 {
    match params.get(index) {
        0 => 1,
        value => i64::from(value),
    }
}

/// Whether JavaScript's `String.prototype.trim` leaves nothing.
fn is_js_blank(text: &str) -> bool {
    js_trim(text).is_empty()
}

impl Terminal {
    fn active(&self) -> &Buffer {
        self.buffers.active()
    }

    fn active_mut(&mut self) -> &mut Buffer {
        self.buffers.active_mut()
    }

    /// The active buffer's line at absolute index `ybase + y`.
    fn row_line(&self, y: i64) -> Option<LineRef> {
        let buffer = self.active();
        buffer.lines.get(buffer.ybase + y)
    }

    /// `_eraseAttrData`: the current background color with no flags.
    pub(crate) fn erase_attr_data(&mut self) -> AttributeData {
        self.erase_attr.bg &= !(CM_MASK | 0xFF_FFFF);
        self.erase_attr.bg |= self.cur_attr.bg & !0xFC00_0000;
        self.erase_attr
    }

    fn erase_null_cell(&mut self) -> CellData {
        CellData::null_with(self.erase_attr_data())
    }

    fn scroll(&mut self, is_wrapped: bool) -> Result<(), Throw> {
        let erase_attr = self.erase_attr_data();
        self.buffers.scroll(erase_attr, is_wrapped, self.cols)
    }

    /// `print`: writes `code_at(start..end)` at the cursor with wrapping,
    /// joining, insert mode and charset translation.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn print(
        &mut self,
        code_at: &dyn Fn(usize) -> u32,
        start: usize,
        end: usize,
    ) -> Result<(), Throw> {
        let charset = self.charsets.charset;
        let cols = self.cols;
        let rows = self.rows;
        let wraparound = self.modes.wraparound;
        let insert_mode = self.modes.insert_mode;
        let cur_attr = self.cur_attr;
        let mut buffer_row = self.row_line(self.active().y);
        let has_run = end > start;
        let x = self.active().x;
        if x != 0 && has_run {
            let row = req(buffer_row.clone())?;
            if row.borrow().get_width(x - 1) == 2 {
                row.borrow_mut()
                    .set_cell_from_codepoint(x - 1, 0, 1, &cur_attr);
            }
        }
        let mut preceding_join_state = self.parser.preceding_join_state;
        for pos in start..end {
            let mut code = code_at(pos);
            if code < 127
                && let Some(mapped) = charset.and_then(|charset| charset.map(code))
            {
                code = mapped;
            }
            let current_info = char_properties(code, preceding_join_state);
            let mut ch_width = i64::from(extract_width(current_info));
            let should_join = extract_should_join(current_info);
            let old_width = if should_join {
                i64::from(extract_width(preceding_join_state))
            } else {
                0
            };
            preceding_join_state = current_info;
            if self.active().x + ch_width - old_width > cols {
                if wraparound {
                    let old_row = buffer_row.clone();
                    let mut old_col = self.active().x - old_width;
                    let buffer = self.active_mut();
                    buffer.x = old_width;
                    buffer.y += 1;
                    if buffer.y == buffer.scroll_bottom + 1 {
                        buffer.y -= 1;
                        self.scroll(true)?;
                    } else {
                        if buffer.y >= rows {
                            buffer.y = rows - 1;
                        }
                        req(buffer.lines.get(buffer.ybase + buffer.y))?
                            .borrow_mut()
                            .is_wrapped = true;
                    }
                    buffer_row = self.row_line(self.active().y);
                    if old_width > 0
                        && let Some(row) = &buffer_row
                    {
                        copy_cells(row, &req(old_row.clone())?, old_col, 0, old_width, false);
                    }
                    while old_col < cols {
                        req(old_row.clone())?
                            .borrow_mut()
                            .set_cell_from_codepoint(old_col, 0, 1, &cur_attr);
                        old_col += 1;
                    }
                } else {
                    self.active_mut().x = cols - 1;
                    if ch_width == 2 {
                        continue;
                    }
                }
            }
            let x = self.active().x;
            if should_join && x != 0 {
                let row = req(buffer_row.clone())?;
                let mut row = row.borrow_mut();
                let offset = if row.get_width(x - 1) == 0 { 2 } else { 1 };
                row.add_codepoint_to_cell(x - offset, code, u32::try_from(ch_width).unwrap_or(0));
                for _ in 0..ch_width - old_width {
                    let buffer = self.buffers.active_mut();
                    row.set_cell_from_codepoint(buffer.x, 0, 0, &cur_attr);
                    buffer.x += 1;
                }
                continue;
            }
            let row = req(buffer_row.clone())?;
            let mut row = row.borrow_mut();
            if insert_mode {
                row.insert_cells(x, ch_width - old_width, &CellData::null_with(cur_attr));
                if row.get_width(cols - 1) == 2 {
                    row.set_cell_from_codepoint(cols - 1, 0, 1, &cur_attr);
                }
            }
            let buffer = self.buffers.active_mut();
            row.set_cell_from_codepoint(
                buffer.x,
                code,
                u32::try_from(ch_width).unwrap_or(0),
                &cur_attr,
            );
            buffer.x += 1;
            if ch_width > 0 {
                ch_width -= 1;
                while ch_width != 0 {
                    row.set_cell_from_codepoint(buffer.x, 0, 0, &cur_attr);
                    buffer.x += 1;
                    ch_width -= 1;
                }
            }
        }
        self.parser.preceding_join_state = preceding_join_state;
        let x = self.active().x;
        if x < cols && has_run {
            let row = req(buffer_row)?;
            let empty = {
                let row = row.borrow();
                row.get_width(x) == 0 && !row.has_content(x)
            };
            if empty {
                row.borrow_mut().set_cell_from_codepoint(x, 0, 1, &cur_attr);
            }
        }
        Ok(())
    }

    pub(crate) fn line_feed(&mut self) -> Result<(), Throw> {
        let convert_eol = self.convert_eol;
        let rows = self.rows;
        let cols = self.cols;
        let buffer = self.active_mut();
        if convert_eol {
            buffer.x = 0;
        }
        buffer.y += 1;
        if buffer.y == buffer.scroll_bottom + 1 {
            buffer.y -= 1;
            self.scroll(false)?;
        } else if buffer.y >= rows {
            buffer.y = rows - 1;
        } else {
            req(buffer.lines.get(buffer.ybase + buffer.y))?
                .borrow_mut()
                .is_wrapped = false;
        }
        let buffer = self.active_mut();
        if buffer.x >= cols {
            buffer.x -= 1;
        }
        Ok(())
    }

    pub(crate) fn backspace(&mut self) -> Result<(), Throw> {
        if !self.modes.reverse_wraparound {
            self.restrict_cursor(None);
            let buffer = self.active_mut();
            if buffer.x > 0 {
                buffer.x -= 1;
            }
            return Ok(());
        }
        self.restrict_cursor(Some(self.cols));
        let cols = self.cols;
        let buffer = self.active_mut();
        if buffer.x > 0 {
            buffer.x -= 1;
        } else if buffer.x == 0
            && buffer.y > buffer.scroll_top
            && buffer.y <= buffer.scroll_bottom
            && buffer
                .lines
                .get(buffer.ybase + buffer.y)
                .is_some_and(|line| line.borrow().is_wrapped)
        {
            req(buffer.lines.get(buffer.ybase + buffer.y))?
                .borrow_mut()
                .is_wrapped = false;
            buffer.y -= 1;
            buffer.x = cols - 1;
            let line = req(buffer.lines.get(buffer.ybase + buffer.y))?;
            let line = line.borrow();
            if line.has_width(buffer.x) && !line.has_content(buffer.x) {
                buffer.x -= 1;
            }
        }
        self.restrict_cursor(None);
        Ok(())
    }

    pub(crate) fn tab(&mut self) {
        if self.active().x >= self.cols {
            return;
        }
        let next = self.active().next_stop(None);
        self.active_mut().x = next;
    }

    fn restrict_cursor(&mut self, max_col: Option<i64>) {
        let max_col = max_col.unwrap_or(self.cols - 1);
        let origin = self.modes.origin;
        let rows = self.rows;
        let buffer = self.active_mut();
        buffer.x = max_col.min(buffer.x.max(0));
        buffer.y = if origin {
            buffer.scroll_bottom.min(buffer.scroll_top.max(buffer.y))
        } else {
            (rows - 1).min(buffer.y.max(0))
        };
    }

    fn set_cursor(&mut self, x: i64, y: i64) {
        let origin = self.modes.origin;
        let buffer = self.active_mut();
        buffer.x = x;
        buffer.y = if origin { buffer.scroll_top + y } else { y };
        self.restrict_cursor(None);
    }

    fn move_cursor(&mut self, x: i64, y: i64) {
        self.restrict_cursor(None);
        let buffer = self.active();
        self.set_cursor(buffer.x + x, buffer.y + y);
    }

    pub(crate) fn cursor_up(&mut self, params: &Params) {
        let buffer = self.active();
        let diff_to_top = buffer.y - buffer.scroll_top;
        if diff_to_top >= 0 {
            self.move_cursor(0, -diff_to_top.min(param_or_one(params, 0)));
        } else {
            self.move_cursor(0, -param_or_one(params, 0));
        }
    }

    pub(crate) fn cursor_down(&mut self, params: &Params) {
        let buffer = self.active();
        let diff_to_bottom = buffer.scroll_bottom - buffer.y;
        if diff_to_bottom >= 0 {
            self.move_cursor(0, diff_to_bottom.min(param_or_one(params, 0)));
        } else {
            self.move_cursor(0, param_or_one(params, 0));
        }
    }

    pub(crate) fn cursor_forward(&mut self, params: &Params) {
        self.move_cursor(param_or_one(params, 0), 0);
    }

    pub(crate) fn cursor_backward(&mut self, params: &Params) {
        self.move_cursor(-param_or_one(params, 0), 0);
    }

    pub(crate) fn cursor_next_line(&mut self, params: &Params) {
        self.cursor_down(params);
        self.active_mut().x = 0;
    }

    pub(crate) fn cursor_preceding_line(&mut self, params: &Params) {
        self.cursor_up(params);
        self.active_mut().x = 0;
    }

    pub(crate) fn cursor_char_absolute(&mut self, params: &Params) {
        let y = self.active().y;
        self.set_cursor(param_or_one(params, 0) - 1, y);
    }

    pub(crate) fn cursor_position(&mut self, params: &Params) {
        let x = if params.len() >= 2 {
            param_or_one(params, 1) - 1
        } else {
            0
        };
        self.set_cursor(x, param_or_one(params, 0) - 1);
    }

    pub(crate) fn line_pos_absolute(&mut self, params: &Params) {
        let x = self.active().x;
        self.set_cursor(x, param_or_one(params, 0) - 1);
    }

    pub(crate) fn v_position_relative(&mut self, params: &Params) {
        self.move_cursor(0, param_or_one(params, 0));
    }

    pub(crate) fn tab_clear(&mut self, params: &Params) {
        let buffer = self.active_mut();
        match params.get(0) {
            0 => {
                buffer.tabs.remove(&buffer.x);
            }
            3 => buffer.tabs.clear(),
            _ => {}
        }
    }

    /// `cursorForwardTab`. Once the cursor stops moving, further steps
    /// cannot move it, so the loop ends there instead of counting down.
    pub(crate) fn cursor_forward_tab(&mut self, params: &Params) {
        if self.active().x >= self.cols {
            return;
        }
        for _ in 0..param_or_one(params, 0) {
            let next = self.active().next_stop(None);
            if next == self.active().x {
                break;
            }
            self.active_mut().x = next;
        }
    }

    /// `cursorBackwardTab`, ending early at the fixed point like
    /// [`Self::cursor_forward_tab`].
    pub(crate) fn cursor_backward_tab(&mut self, params: &Params) {
        if self.active().x >= self.cols {
            return;
        }
        for _ in 0..param_or_one(params, 0) {
            let previous = self.active().prev_stop(None);
            if previous == self.active().x {
                break;
            }
            self.active_mut().x = previous;
        }
    }

    pub(crate) fn select_protected(&mut self, params: &Params) {
        match params.get(0) {
            1 => self.cur_attr.bg |= BG_PROTECTED,
            0 | 2 => self.cur_attr.bg &= !BG_PROTECTED,
            _ => {}
        }
    }

    fn erase_in_buffer_line(
        &mut self,
        y: i64,
        start: i64,
        end: i64,
        clear_wrap: bool,
        respect_protect: bool,
    ) -> Result<(), Throw> {
        let line = req(self.row_line(y))?;
        let fill = self.erase_null_cell();
        let mut line = line.borrow_mut();
        line.replace_cells(start, end, &fill, respect_protect);
        if clear_wrap {
            line.is_wrapped = false;
        }
        Ok(())
    }

    fn reset_buffer_line(&mut self, y: i64, respect_protect: bool) {
        if let Some(line) = self.row_line(y) {
            let fill = self.erase_null_cell();
            let mut line = line.borrow_mut();
            line.fill(&fill, respect_protect);
            line.is_wrapped = false;
        }
    }

    pub(crate) fn erase_in_display(
        &mut self,
        params: &Params,
        respect_protect: bool,
    ) -> Result<(), Throw> {
        self.restrict_cursor(Some(self.cols));
        let cols = self.cols;
        let rows = self.rows;
        match params.get(0) {
            0 => {
                let (x, y) = (self.active().x, self.active().y);
                self.erase_in_buffer_line(y, x, cols, x == 0, respect_protect)?;
                for j in y + 1..rows {
                    self.reset_buffer_line(j, respect_protect);
                }
            }
            1 => {
                let (x, y) = (self.active().x, self.active().y);
                self.erase_in_buffer_line(y, 0, x + 1, true, respect_protect)?;
                if x + 1 >= cols {
                    // xterm indexes this line without `ybase`.
                    req(self.active().lines.get(y + 1))?.borrow_mut().is_wrapped = false;
                }
                for j in (0..y).rev() {
                    self.reset_buffer_line(j, respect_protect);
                }
            }
            2 => {
                // `scrollOnEraseInDisplay` is off.
                for j in (0..rows).rev() {
                    self.reset_buffer_line(j, respect_protect);
                }
            }
            3 => {
                let buffer = self.active_mut();
                let scroll_back_size = buffer.lines.length() - rows;
                if scroll_back_size > 0 {
                    buffer.lines.trim_start(scroll_back_size);
                    buffer.ybase = (buffer.ybase - scroll_back_size).max(0);
                    buffer.ydisp = (buffer.ydisp - scroll_back_size).max(0);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn erase_in_line(
        &mut self,
        params: &Params,
        respect_protect: bool,
    ) -> Result<(), Throw> {
        self.restrict_cursor(Some(self.cols));
        let cols = self.cols;
        let (x, y) = (self.active().x, self.active().y);
        match params.get(0) {
            0 => self.erase_in_buffer_line(y, x, cols, x == 0, respect_protect)?,
            1 => self.erase_in_buffer_line(y, 0, x + 1, false, respect_protect)?,
            2 => self.erase_in_buffer_line(y, 0, cols, true, respect_protect)?,
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn insert_lines(&mut self, params: &Params) {
        self.restrict_cursor(None);
        let rows = self.rows;
        let buffer = self.active();
        if buffer.y > buffer.scroll_bottom || buffer.y < buffer.scroll_top {
            return;
        }
        let row = buffer.ybase + buffer.y;
        let scroll_bottom_rows_offset = rows - 1 - buffer.scroll_bottom;
        let scroll_bottom_absolute = rows - 1 + buffer.ybase - scroll_bottom_rows_offset + 1;
        for _ in 0..param_or_one(params, 0) {
            let blank = Buffer::blank_line(self.erase_attr_data(), false, self.cols);
            let lines = &mut self.active_mut().lines;
            lines.splice(scroll_bottom_absolute - 1, 1, &[]);
            lines.splice(row, 0, &[blank]);
        }
        self.active_mut().x = 0;
    }

    pub(crate) fn delete_lines(&mut self, params: &Params) {
        self.restrict_cursor(None);
        let rows = self.rows;
        let buffer = self.active();
        if buffer.y > buffer.scroll_bottom || buffer.y < buffer.scroll_top {
            return;
        }
        let row = buffer.ybase + buffer.y;
        let j = rows - 1 + buffer.ybase - (rows - 1 - buffer.scroll_bottom);
        for _ in 0..param_or_one(params, 0) {
            let blank = Buffer::blank_line(self.erase_attr_data(), false, self.cols);
            let lines = &mut self.active_mut().lines;
            lines.splice(row, 1, &[]);
            lines.splice(j, 0, &[blank]);
        }
        self.active_mut().x = 0;
    }

    pub(crate) fn insert_chars(&mut self, params: &Params) {
        self.restrict_cursor(None);
        if let Some(line) = self.row_line(self.active().y) {
            let fill = self.erase_null_cell();
            let x = self.active().x;
            line.borrow_mut()
                .insert_cells(x, param_or_one(params, 0), &fill);
        }
    }

    pub(crate) fn delete_chars(&mut self, params: &Params) {
        self.restrict_cursor(None);
        if let Some(line) = self.row_line(self.active().y) {
            let fill = self.erase_null_cell();
            let x = self.active().x;
            line.borrow_mut()
                .delete_cells(x, param_or_one(params, 0), &fill);
        }
    }

    pub(crate) fn scroll_up(&mut self, params: &Params) {
        for _ in 0..param_or_one(params, 0) {
            let blank = Buffer::blank_line(self.erase_attr_data(), false, self.cols);
            let buffer = self.active_mut();
            let (top, bottom) = (
                buffer.ybase + buffer.scroll_top,
                buffer.ybase + buffer.scroll_bottom,
            );
            buffer.lines.splice(top, 1, &[]);
            buffer.lines.splice(bottom, 0, &[blank]);
        }
    }

    pub(crate) fn scroll_down(&mut self, params: &Params) {
        for _ in 0..param_or_one(params, 0) {
            let blank = Buffer::blank_line(AttributeData::default(), false, self.cols);
            let buffer = self.active_mut();
            let (top, bottom) = (
                buffer.ybase + buffer.scroll_top,
                buffer.ybase + buffer.scroll_bottom,
            );
            buffer.lines.splice(bottom, 1, &[]);
            buffer.lines.splice(top, 0, &[blank]);
        }
    }

    /// The shared body of SL, SR, DECIC and DECDC: edits every line of the
    /// scroll region and clears its wrap flag.
    fn edit_region_columns(
        &mut self,
        edit: &dyn Fn(&mut crate::buffer_line::BufferLine, i64, &CellData),
    ) -> Result<(), Throw> {
        let buffer = self.active();
        if buffer.y > buffer.scroll_bottom || buffer.y < buffer.scroll_top {
            return Ok(());
        }
        let (top, bottom, x) = (buffer.scroll_top, buffer.scroll_bottom, buffer.x);
        for y in top..=bottom {
            let line = req(self.row_line(y))?;
            let fill = self.erase_null_cell();
            let mut line = line.borrow_mut();
            edit(&mut line, x, &fill);
            line.is_wrapped = false;
        }
        Ok(())
    }

    pub(crate) fn scroll_left(&mut self, params: &Params) -> Result<(), Throw> {
        let count = param_or_one(params, 0);
        self.edit_region_columns(&|line, _, fill| line.delete_cells(0, count, fill))
    }

    pub(crate) fn scroll_right(&mut self, params: &Params) -> Result<(), Throw> {
        let count = param_or_one(params, 0);
        self.edit_region_columns(&|line, _, fill| line.insert_cells(0, count, fill))
    }

    pub(crate) fn insert_columns(&mut self, params: &Params) -> Result<(), Throw> {
        let count = param_or_one(params, 0);
        self.edit_region_columns(&|line, x, fill| line.insert_cells(x, count, fill))
    }

    pub(crate) fn delete_columns(&mut self, params: &Params) -> Result<(), Throw> {
        let count = param_or_one(params, 0);
        self.edit_region_columns(&|line, x, fill| line.delete_cells(x, count, fill))
    }

    pub(crate) fn erase_chars(&mut self, params: &Params) {
        self.restrict_cursor(None);
        if let Some(line) = self.row_line(self.active().y) {
            let fill = self.erase_null_cell();
            let x = self.active().x;
            line.borrow_mut()
                .replace_cells(x, x + param_or_one(params, 0), &fill, false);
        }
    }

    /// `repeatPrecedingCharacter` (REP): prints the preceding cell's text
    /// again, without building the repeated array.
    pub(crate) fn repeat_preceding_character(&mut self, params: &Params) -> Result<(), Throw> {
        let join_state = self.parser.preceding_join_state;
        if join_state == 0 {
            return Ok(());
        }
        let length = usize::try_from(param_or_one(params, 0)).unwrap_or(0);
        let x = self.active().x - i64::from(extract_width(join_state));
        let line = req(self.row_line(self.active().y))?;
        let text = line.borrow().get_string(x).ok_or(Throw)?;
        let codes: Vec<u32> = text.chars().map(u32::from).collect();
        let total = codes.len() * length;
        self.print(&|index| codes[index % codes.len()], 0, total)
    }

    pub(crate) fn set_mode(&mut self, params: &Params) {
        for index in 0..params.len() {
            match params.get(index) {
                4 => self.modes.insert_mode = true,
                20 => self.convert_eol = true,
                _ => {}
            }
        }
    }

    pub(crate) fn reset_mode(&mut self, params: &Params) {
        for index in 0..params.len() {
            match params.get(index) {
                4 => self.modes.insert_mode = false,
                20 => self.convert_eol = false,
                _ => {}
            }
        }
    }

    pub(crate) fn set_mode_private(&mut self, params: &Params) {
        for index in 0..params.len() {
            match params.get(index) {
                2 => {
                    for level in 0..4 {
                        self.charsets.set_gcharset(level, None);
                    }
                }
                6 => {
                    self.modes.origin = true;
                    self.set_cursor(0, 0);
                }
                7 => self.modes.wraparound = true,
                45 => self.modes.reverse_wraparound = true,
                25 => self.cursor_hidden = false,
                1048 => self.save_cursor(),
                1049 => {
                    self.save_cursor();
                    self.activate_alt_buffer();
                }
                47 | 1047 => self.activate_alt_buffer(),
                // 3 needs windowOptions.setWinLines (off); the rest only
                // notify or set state Paseo never reads.
                _ => {}
            }
        }
    }

    fn activate_alt_buffer(&mut self) {
        let erase_attr = self.erase_attr_data();
        self.buffers.activate_alt_buffer(erase_attr, self.cols);
    }

    pub(crate) fn reset_mode_private(&mut self, params: &Params) {
        for index in 0..params.len() {
            let mode = params.get(index);
            match mode {
                6 => {
                    self.modes.origin = false;
                    self.set_cursor(0, 0);
                }
                7 => self.modes.wraparound = false,
                45 => self.modes.reverse_wraparound = false,
                25 => self.cursor_hidden = true,
                1048 => self.restore_cursor(),
                1049 | 47 | 1047 => {
                    self.buffers.activate_normal_buffer();
                    if mode == 1049 {
                        self.restore_cursor();
                    }
                }
                _ => {}
            }
        }
    }

    fn update_attr_color(color: u32, mode: i32, c1: i32, c2: i32, c3: i32) -> u32 {
        let mut color = color;
        if mode == 2 {
            color |= CM_RGB;
            color &= !RGB_MASK;
            color |= AttributeData::from_color_rgb(c1, c2, c3);
        } else if mode == 5 {
            color &= !(CM_MASK | PCOLOR_MASK);
            color |= CM_P256 | u32::from(c1.to_le_bytes()[0]);
        }
        color
    }

    /// `_extractColor` for SGR 38, 48 and 58 at `pos`; returns how many
    /// further parameters it consumed.
    fn extract_color(params: &Params, pos: usize, attr: &mut AttributeData) -> usize {
        // `accu` may grow to 7 entries, as the JavaScript array does when a
        // sub parameter list starts at its last slot.
        let mut accu = [0_i32, 0, -1, 0, 0, 0, 0];
        let mut accu_length = 6;
        let mut c_space = 0;
        let mut advance = 0;
        loop {
            accu[advance + c_space] = params.get(pos + advance);
            if params.has_sub_params(pos + advance) {
                let sub_params = params.sub_params(pos + advance);
                let mut i = 0;
                loop {
                    if accu[1] == 5 {
                        c_space = 1;
                    }
                    let slot = advance + i + 1 + c_space;
                    accu[slot] = sub_params[i];
                    accu_length = accu_length.max(slot + 1);
                    i += 1;
                    if !(i < sub_params.len() && i + advance + 1 + c_space < accu_length) {
                        break;
                    }
                }
                break;
            }
            if (accu[1] == 5 && advance + c_space >= 2) || (accu[1] == 2 && advance + c_space >= 5)
            {
                break;
            }
            if accu[1] != 0 {
                c_space = 1;
            }
            advance += 1;
            if !(advance + pos < params.len() && advance + c_space < accu_length) {
                break;
            }
        }
        for value in &mut accu[2..accu_length] {
            if *value == -1 {
                *value = 0;
            }
        }
        match accu[0] {
            38 => attr.fg = Self::update_attr_color(attr.fg, accu[1], accu[3], accu[4], accu[5]),
            48 => attr.bg = Self::update_attr_color(attr.bg, accu[1], accu[3], accu[4], accu[5]),
            58 => {
                let color = attr.extended.underline_color();
                attr.extended.set_underline_color(Self::update_attr_color(
                    color, accu[1], accu[3], accu[4], accu[5],
                ));
            }
            _ => {}
        }
        advance
    }

    fn process_underline(style: i32, attr: &mut AttributeData) {
        let style = if style == -1 || style > 5 {
            UNDERLINE_SINGLE
        } else {
            u32::try_from(style).unwrap_or(UNDERLINE_SINGLE)
        };
        attr.extended.set_underline_style(style);
        attr.fg |= FG_UNDERLINE;
        if style == UNDERLINE_NONE {
            attr.fg &= !FG_UNDERLINE;
        }
        attr.update_extended();
    }

    fn process_sgr0(attr: &mut AttributeData) {
        attr.fg = 0;
        attr.bg = 0;
        attr.extended.set_underline_style(UNDERLINE_NONE);
        let color = attr.extended.underline_color();
        attr.extended
            .set_underline_color(color & !(CM_MASK | RGB_MASK));
        attr.update_extended();
    }

    /// `charAttributes` (SGR).
    pub(crate) fn char_attributes(&mut self, params: &Params) {
        let attr = &mut self.cur_attr;
        if params.len() == 1 && params.get(0) == 0 {
            Self::process_sgr0(attr);
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let p = params.get(i);
            match p {
                30..=37 => {
                    attr.fg &= !(CM_MASK | PCOLOR_MASK);
                    attr.fg |= CM_P16 | (p.unsigned_abs() - 30);
                }
                40..=47 => {
                    attr.bg &= !(CM_MASK | PCOLOR_MASK);
                    attr.bg |= CM_P16 | (p.unsigned_abs() - 40);
                }
                90..=97 => {
                    attr.fg &= !(CM_MASK | PCOLOR_MASK);
                    attr.fg |= CM_P16 | (p.unsigned_abs() - 90) | 8;
                }
                100..=107 => {
                    attr.bg &= !(CM_MASK | PCOLOR_MASK);
                    attr.bg |= CM_P16 | (p.unsigned_abs() - 100) | 8;
                }
                0 => Self::process_sgr0(attr),
                1 => attr.fg |= FG_BOLD,
                3 => attr.bg |= BG_ITALIC,
                4 => {
                    attr.fg |= FG_UNDERLINE;
                    let style = if params.has_sub_params(i) {
                        params.sub_params(i)[0]
                    } else {
                        1
                    };
                    Self::process_underline(style, attr);
                }
                5 => attr.fg |= FG_BLINK,
                7 => attr.fg |= FG_INVERSE,
                8 => attr.fg |= FG_INVISIBLE,
                9 => attr.fg |= FG_STRIKETHROUGH,
                2 => attr.bg |= BG_DIM,
                21 => Self::process_underline(UNDERLINE_DOUBLE.cast_signed(), attr),
                22 => {
                    attr.fg &= !FG_BOLD;
                    attr.bg &= !BG_DIM;
                }
                23 => attr.bg &= !BG_ITALIC,
                24 => {
                    attr.fg &= !FG_UNDERLINE;
                    Self::process_underline(0, attr);
                }
                25 => attr.fg &= !FG_BLINK,
                27 => attr.fg &= !FG_INVERSE,
                28 => attr.fg &= !FG_INVISIBLE,
                29 => attr.fg &= !FG_STRIKETHROUGH,
                39 => attr.fg &= !(CM_MASK | RGB_MASK),
                49 => attr.bg &= !(CM_MASK | RGB_MASK),
                38 | 48 | 58 => i += Self::extract_color(params, i, attr),
                53 => attr.bg |= BG_OVERLINE,
                55 => attr.bg &= !BG_OVERLINE,
                59 => {
                    attr.extended.set_underline_color(u32::MAX);
                    attr.update_extended();
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// `softReset` (DECSTR).
    pub(crate) fn soft_reset(&mut self) {
        self.cursor_hidden = false;
        let rows = self.rows;
        self.cur_attr = AttributeData::default();
        self.modes = Modes::default();
        self.charsets = CharsetService::default();
        let charset = self.charsets.charset;
        let attr = self.cur_attr;
        let buffer = self.active_mut();
        buffer.scroll_top = 0;
        buffer.scroll_bottom = rows - 1;
        buffer.saved_x = 0;
        buffer.saved_y = buffer.ybase;
        buffer.saved_cur_attr.fg = attr.fg;
        buffer.saved_cur_attr.bg = attr.bg;
        buffer.saved_charset = charset;
        self.modes.origin = false;
    }

    /// `setCursorStyle` (DECSCUSR).
    pub(crate) fn set_cursor_style(&mut self, params: &Params) {
        let param = if params.len() == 0 { 1 } else { params.get(0) };
        if param == 0 {
            self.modes.cursor_style = None;
            self.modes.cursor_blink = None;
            return;
        }
        match param {
            1 | 2 => self.modes.cursor_style = Some(CursorStyle::Block),
            3 | 4 => self.modes.cursor_style = Some(CursorStyle::Underline),
            5 | 6 => self.modes.cursor_style = Some(CursorStyle::Bar),
            _ => {}
        }
        self.modes.cursor_blink = Some(param % 2 == 1);
    }

    pub(crate) fn set_scroll_region(&mut self, params: &Params) {
        let top = param_or_one(params, 0);
        let mut bottom = self.rows;
        if params.len() >= 2 {
            let requested = i64::from(params.get(1));
            if requested <= self.rows && requested != 0 {
                bottom = requested;
            }
        }
        if bottom > top {
            let buffer = self.active_mut();
            buffer.scroll_top = top - 1;
            buffer.scroll_bottom = bottom - 1;
            self.set_cursor(0, 0);
        }
    }

    pub(crate) fn save_cursor(&mut self) {
        let charset = self.charsets.charset;
        let attr = self.cur_attr;
        let buffer = self.active_mut();
        buffer.saved_x = buffer.x;
        buffer.saved_y = buffer.ybase + buffer.y;
        buffer.saved_cur_attr.fg = attr.fg;
        buffer.saved_cur_attr.bg = attr.bg;
        buffer.saved_charset = charset;
    }

    pub(crate) fn restore_cursor(&mut self) {
        let buffer = self.active_mut();
        buffer.x = buffer.saved_x;
        buffer.y = (buffer.saved_y - buffer.ybase).max(0);
        let (fg, bg, charset) = (
            buffer.saved_cur_attr.fg,
            buffer.saved_cur_attr.bg,
            buffer.saved_charset,
        );
        self.cur_attr.fg = fg;
        self.cur_attr.bg = bg;
        self.charsets.charset = charset;
        self.restrict_cursor(None);
    }

    pub(crate) fn set_title(&mut self, data: &str) {
        self.fire_title_change(data);
    }

    /// `setHyperlink` (OSC 8). Link ids are only ever tested for zero, so a
    /// counter stands in for `OscLinkService.registerLink`.
    pub(crate) fn set_hyperlink(&mut self, data: &str) {
        let Some(index) = data.find(';') else {
            return;
        };
        let id = &data[..index];
        let uri = &data[index + 1..];
        if !uri.is_empty() {
            if self.cur_attr.extended.url_id != 0 {
                self.finish_hyperlink();
            }
            self.cur_attr.extended.url_id = self.next_link_id;
            self.next_link_id = self.next_link_id.checked_add(1).unwrap_or(1);
            self.cur_attr.update_extended();
            return;
        }
        if !is_js_blank(id) {
            return;
        }
        self.finish_hyperlink();
    }

    fn finish_hyperlink(&mut self) {
        self.cur_attr.extended.url_id = 0;
        self.cur_attr.update_extended();
    }

    pub(crate) fn next_line(&mut self) -> Result<(), Throw> {
        self.active_mut().x = 0;
        self.index()
    }

    pub(crate) fn select_default_charset(&mut self) {
        self.charsets.set_glevel(0);
        self.charsets.set_gcharset(0, None);
    }

    /// `selectCharset` for `ESC <collect> <flag>`.
    pub(crate) fn select_charset(&mut self, collect: i32, charset: Option<Charset>) {
        let level = match u8::try_from(collect) {
            Ok(b'(') => 0,
            Ok(b')' | b'-') => 1,
            Ok(b'*' | b'.') => 2,
            Ok(b'+') => 3,
            _ => return,
        };
        self.charsets.set_gcharset(level, charset);
    }

    pub(crate) fn index(&mut self) -> Result<(), Throw> {
        self.restrict_cursor(None);
        let rows = self.rows;
        let buffer = self.active_mut();
        buffer.y += 1;
        if buffer.y == buffer.scroll_bottom + 1 {
            buffer.y -= 1;
            self.scroll(false)?;
        } else if buffer.y >= rows {
            buffer.y = rows - 1;
        }
        self.restrict_cursor(None);
        Ok(())
    }

    pub(crate) fn tab_set(&mut self) {
        let buffer = self.active_mut();
        buffer.tabs.insert(buffer.x);
    }

    pub(crate) fn reverse_index(&mut self) -> Result<(), Throw> {
        self.restrict_cursor(None);
        let buffer = self.active();
        if buffer.y == buffer.scroll_top {
            let scroll_region_height = buffer.scroll_bottom - buffer.scroll_top;
            let row = buffer.ybase + buffer.y;
            self.active_mut()
                .lines
                .shift_elements(row, scroll_region_height, 1)?;
            let blank = Buffer::blank_line(self.erase_attr_data(), false, self.cols);
            self.active_mut().lines.set(row, Some(blank));
        } else {
            self.active_mut().y -= 1;
            self.restrict_cursor(None);
        }
        Ok(())
    }

    /// `screenAlignmentPattern` (DECALN).
    pub(crate) fn screen_alignment_pattern(&mut self) {
        let cell = CellData {
            content: (1 << WIDTH_SHIFT) | u32::from(b'E'),
            attr: AttributeData {
                fg: self.cur_attr.fg,
                bg: self.cur_attr.bg,
                extended: crate::attributes::ExtendedAttrs::default(),
            },
            combined_data: String::new(),
        };
        self.set_cursor(0, 0);
        for y_offset in 0..self.rows {
            if let Some(line) = self.row_line(self.active().y + y_offset) {
                let mut line = line.borrow_mut();
                line.fill(&cell, false);
                line.is_wrapped = false;
            }
        }
        self.set_cursor(0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::is_js_blank;

    #[test]
    fn js_blank_follows_string_trim() {
        assert!(is_js_blank(" \t\u{feff}\u{3000}"));
        assert!(!is_js_blank("\u{85}"));
        assert!(!is_js_blank("id=1"));
    }
}
