//! The normal and alternate screen buffers: cursor, scroll region, tab
//! stops, scrolling, resize and reflow.
//!
//! Ported from xterm.js 6.0.0 `src/common/buffer/Buffer.ts`,
//! `src/common/buffer/BufferReflow.ts`, `src/common/buffer/BufferSet.ts` and
//! the `scroll` of `src/common/services/BufferService.ts`.
//! Copyright (c) 2017 The xterm.js authors. MIT License.
//!
//! Options are the ones Paseo sets: `scrollback` 1000, and the defaults for
//! the rest (`tabStopWidth` 8, no Windows mode, no `reflowCursorLine`).
//! Markers are left out: only OSC 8 link bookkeeping uses them, and nothing
//! reads that back.

use std::collections::HashSet;

use crate::attributes::{AttributeData, CellData};
use crate::buffer_line::{BufferLine, LineRef};
use crate::charsets::Charset;
use crate::circular_list::{CircularList, Throw};

/// Scrollback lines kept by the normal buffer (`scrollback: 1000`).
pub(crate) const SCROLLBACK: i64 = 1000;
const TAB_STOP_WIDTH: i64 = 8;

/// Turns a JavaScript `undefined` line into the `TypeError` its use throws.
pub(crate) fn req(line: Option<LineRef>) -> Result<LineRef, Throw> {
    line.ok_or(Throw)
}

fn null_cell() -> CellData {
    CellData::null_with(AttributeData::default())
}

#[derive(Debug)]
pub(crate) struct Buffer {
    pub(crate) lines: CircularList,
    pub(crate) ydisp: i64,
    pub(crate) ybase: i64,
    pub(crate) y: i64,
    pub(crate) x: i64,
    pub(crate) scroll_bottom: i64,
    pub(crate) scroll_top: i64,
    pub(crate) tabs: HashSet<i64>,
    pub(crate) saved_y: i64,
    pub(crate) saved_x: i64,
    pub(crate) saved_cur_attr: AttributeData,
    pub(crate) saved_charset: Option<Charset>,
    cols: i64,
    rows: i64,
    has_scrollback: bool,
}

impl Buffer {
    fn new(has_scrollback: bool, cols: i64, rows: i64) -> Self {
        let mut buffer = Self {
            lines: CircularList::new(Self::correct_buffer_length(has_scrollback, rows)),
            ydisp: 0,
            ybase: 0,
            y: 0,
            x: 0,
            scroll_bottom: rows - 1,
            scroll_top: 0,
            tabs: HashSet::new(),
            saved_y: 0,
            saved_x: 0,
            saved_cur_attr: AttributeData::default(),
            saved_charset: None,
            cols,
            rows,
            has_scrollback,
        };
        buffer.setup_tab_stops(None);
        buffer
    }

    fn correct_buffer_length(has_scrollback: bool, rows: i64) -> i64 {
        if has_scrollback {
            rows + SCROLLBACK
        } else {
            rows
        }
    }

    /// `getBlankLine`: a line as wide as the buffer service's `cols`.
    pub(crate) fn blank_line(attr: AttributeData, is_wrapped: bool, service_cols: i64) -> LineRef {
        BufferLine::new(service_cols, &CellData::null_with(attr), is_wrapped).into_ref()
    }

    fn fill_viewport_rows(&mut self, fill_attr: Option<AttributeData>, service_cols: i64) {
        if self.lines.length() == 0 {
            let attr = fill_attr.unwrap_or_default();
            for _ in 0..self.rows {
                self.lines.push(Self::blank_line(attr, false, service_cols));
            }
        }
    }

    fn clear(&mut self) {
        self.ydisp = 0;
        self.ybase = 0;
        self.y = 0;
        self.x = 0;
        self.lines = CircularList::new(Self::correct_buffer_length(self.has_scrollback, self.rows));
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.setup_tab_stops(None);
    }

    pub(crate) fn line(&self, index: i64) -> Result<LineRef, Throw> {
        req(self.lines.get(index))
    }

    fn resize(&mut self, new_cols: i64, new_rows: i64, service_cols: i64) -> Result<(), Throw> {
        let null = null_cell();
        let new_max_length = Self::correct_buffer_length(self.has_scrollback, new_rows);
        if new_max_length > self.lines.max_length() {
            self.lines.set_max_length(new_max_length);
        }
        if self.lines.length() > 0 {
            if self.cols < new_cols {
                for index in 0..self.lines.length() {
                    self.line(index)?.borrow_mut().resize(new_cols, &null);
                }
            }
            let mut add_to_y = 0;
            if self.rows < new_rows {
                for _ in self.rows..new_rows {
                    if self.lines.length() < new_rows + self.ybase {
                        if self.ybase > 0
                            && self.lines.length() <= self.ybase + self.y + add_to_y + 1
                        {
                            self.ybase -= 1;
                            add_to_y += 1;
                            if self.ydisp > 0 {
                                self.ydisp -= 1;
                            }
                        } else {
                            self.lines
                                .push(BufferLine::new(new_cols, &null, false).into_ref());
                        }
                    }
                }
            } else {
                for _ in new_rows..self.rows {
                    if self.lines.length() > new_rows + self.ybase {
                        if self.lines.length() > self.ybase + self.y + 1 {
                            self.lines.pop();
                        } else {
                            self.ybase += 1;
                            self.ydisp += 1;
                        }
                    }
                }
            }
            if new_max_length < self.lines.max_length() {
                let amount_to_trim = self.lines.length() - new_max_length;
                if amount_to_trim > 0 {
                    self.lines.trim_start(amount_to_trim);
                    self.ybase = (self.ybase - amount_to_trim).max(0);
                    self.ydisp = (self.ydisp - amount_to_trim).max(0);
                    self.saved_y = (self.saved_y - amount_to_trim).max(0);
                }
                self.lines.set_max_length(new_max_length);
            }
            self.x = self.x.min(new_cols - 1);
            self.y = self.y.min(new_rows - 1);
            if add_to_y != 0 {
                self.y += add_to_y;
            }
            self.saved_x = self.saved_x.min(new_cols - 1);
            self.scroll_top = 0;
        }
        self.scroll_bottom = new_rows - 1;
        // Reflow runs for the buffer with scrollback only.
        if self.has_scrollback {
            self.reflow(new_cols, new_rows, service_cols)?;
            if self.cols > new_cols {
                for index in 0..self.lines.length() {
                    self.line(index)?.borrow_mut().resize(new_cols, &null);
                }
            }
        }
        self.cols = new_cols;
        self.rows = new_rows;
        Ok(())
    }

    fn reflow(&mut self, new_cols: i64, new_rows: i64, service_cols: i64) -> Result<(), Throw> {
        if self.cols == new_cols {
            return Ok(());
        }
        if new_cols > self.cols {
            self.reflow_larger(new_cols, new_rows)
        } else {
            self.reflow_smaller(new_cols, new_rows, service_cols)
        }
    }

    fn reflow_larger(&mut self, new_cols: i64, new_rows: i64) -> Result<(), Throw> {
        let to_remove = reflow_larger_get_lines_to_remove(
            &self.lines,
            self.cols,
            new_cols,
            self.ybase + self.y,
            &null_cell(),
        )?;
        if !to_remove.is_empty() {
            let (layout, count_removed) = reflow_larger_create_new_layout(&self.lines, &to_remove);
            reflow_larger_apply_new_layout(&mut self.lines, &layout);
            self.reflow_larger_adjust_viewport(new_cols, new_rows, count_removed);
        }
        Ok(())
    }

    fn reflow_larger_adjust_viewport(&mut self, new_cols: i64, new_rows: i64, count_removed: i64) {
        let null = null_cell();
        for _ in 0..count_removed {
            if self.ybase == 0 {
                if self.y > 0 {
                    self.y -= 1;
                }
                if self.lines.length() < new_rows {
                    self.lines
                        .push(BufferLine::new(new_cols, &null, false).into_ref());
                }
            } else {
                if self.ydisp == self.ybase {
                    self.ydisp -= 1;
                }
                self.ybase -= 1;
            }
        }
        self.saved_y = (self.saved_y - count_removed).max(0);
    }

    #[allow(clippy::too_many_lines)]
    fn reflow_smaller(
        &mut self,
        new_cols: i64,
        new_rows: i64,
        service_cols: i64,
    ) -> Result<(), Throw> {
        let null = null_cell();
        let mut to_insert: Vec<(i64, Vec<LineRef>)> = Vec::new();
        let mut count_to_insert: i64 = 0;
        let mut y = self.lines.length() - 1;
        while y >= 0 {
            let mut next_line = self.lines.get(y);
            let skip = match &next_line {
                None => true,
                Some(line) => {
                    let line = line.borrow();
                    !line.is_wrapped && line.get_trimmed_length() <= new_cols
                }
            };
            if skip {
                y -= 1;
                continue;
            }
            let mut wrapped_lines: Vec<Option<LineRef>> = vec![next_line.clone()];
            while req(next_line.clone())?.borrow().is_wrapped && y > 0 {
                y -= 1;
                next_line = self.lines.get(y);
                wrapped_lines.insert(0, next_line.clone());
            }
            // `reflowCursorLine` is off: the cursor's wrapped line keeps its layout.
            let absolute_y = self.ybase + self.y;
            let group_length = i64::try_from(wrapped_lines.len()).unwrap_or(0);
            if absolute_y >= y && absolute_y < y + group_length {
                y -= 1;
                continue;
            }
            let last_line_length = req(wrapped_lines[wrapped_lines.len() - 1].clone())?
                .borrow()
                .get_trimmed_length();
            let dest_line_lengths =
                reflow_smaller_get_new_line_lengths(&wrapped_lines, self.cols, new_cols)?;
            let lines_to_add = i64::try_from(dest_line_lengths.len()).unwrap_or(0) - group_length;
            let trimmed_lines = if self.ybase == 0 && self.y != self.lines.length() - 1 {
                (self.y - self.lines.max_length() + lines_to_add).max(0)
            } else {
                (self.lines.length() - self.lines.max_length() + lines_to_add).max(0)
            };
            let new_lines: Vec<LineRef> = (0..lines_to_add)
                .map(|_| Buffer::blank_line(AttributeData::default(), true, service_cols))
                .collect();
            if !new_lines.is_empty() {
                to_insert.push((y + group_length + count_to_insert, new_lines.clone()));
                count_to_insert += i64::try_from(new_lines.len()).unwrap_or(0);
            }
            wrapped_lines.extend(new_lines.into_iter().map(Some));
            let line_length = |index: i64| -> Option<i64> {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| dest_line_lengths.get(index).copied())
            };
            let mut dest_line_index = i64::try_from(dest_line_lengths.len()).unwrap_or(0) - 1;
            let mut dest_col = line_length(dest_line_index);
            if dest_col == Some(0) {
                dest_line_index -= 1;
                dest_col = line_length(dest_line_index);
            }
            let wrapped_at = |index: i64| -> Option<LineRef> {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| wrapped_lines.get(index).cloned().flatten())
            };
            let mut src_line_index =
                i64::try_from(wrapped_lines.len()).unwrap_or(0) - lines_to_add - 1;
            let mut src_col = last_line_length;
            while src_line_index >= 0 {
                let Some(dest_line) = wrapped_at(dest_line_index) else {
                    break;
                };
                // `dest_col` is defined whenever its line is.
                let current_dest = dest_col.unwrap_or(0);
                let cells_to_copy = src_col.min(current_dest);
                let src_line = req(wrapped_at(src_line_index))?;
                copy_cells(
                    &dest_line,
                    &src_line,
                    src_col - cells_to_copy,
                    current_dest - cells_to_copy,
                    cells_to_copy,
                    true,
                );
                let next_dest = current_dest - cells_to_copy;
                dest_col = Some(next_dest);
                if next_dest == 0 {
                    dest_line_index -= 1;
                    dest_col = line_length(dest_line_index);
                }
                src_col -= cells_to_copy;
                if src_col == 0 {
                    src_line_index -= 1;
                    let wrapped_lines_index = src_line_index.max(0);
                    src_col = get_wrapped_line_trimmed_length(
                        &wrapped_lines,
                        wrapped_lines_index,
                        self.cols,
                    )?;
                }
            }
            for (index, line) in wrapped_lines.iter().enumerate() {
                if let Some(&length) = dest_line_lengths.get(index)
                    && length < new_cols
                {
                    req(line.clone())?.borrow_mut().set_cell(length, &null);
                }
            }
            let mut viewport_adjustments = lines_to_add - trimmed_lines;
            while viewport_adjustments > 0 {
                viewport_adjustments -= 1;
                if self.ybase == 0 {
                    if self.y < new_rows - 1 {
                        self.y += 1;
                        self.lines.pop();
                    } else {
                        self.ybase += 1;
                        self.ydisp += 1;
                    }
                } else if self.ybase
                    < self
                        .lines
                        .max_length()
                        .min(self.lines.length() + count_to_insert)
                        - new_rows
                {
                    if self.ybase == self.ydisp {
                        self.ydisp += 1;
                    }
                    self.ybase += 1;
                }
            }
            self.saved_y = (self.saved_y + lines_to_add).min(self.ybase + new_rows - 1);
            y -= 1;
        }
        if !to_insert.is_empty() {
            self.apply_inserts(&to_insert, count_to_insert);
        }
        Ok(())
    }

    /// Splices the reflow's new lines in, bottom up, in one pass.
    fn apply_inserts(&mut self, to_insert: &[(i64, Vec<LineRef>)], count_to_insert: i64) {
        let original_lines: Vec<Option<LineRef>> = (0..self.lines.length())
            .map(|index| self.lines.get(index))
            .collect();
        let original_lines_length = self.lines.length();
        let mut original_line_index = original_lines_length - 1;
        let mut next_to_insert_index = 0;
        let mut next_to_insert = to_insert.first();
        self.lines.set_length(
            self.lines
                .max_length()
                .min(self.lines.length() + count_to_insert),
        );
        let mut count_inserted_so_far = 0;
        let mut index =
            (self.lines.max_length() - 1).min(original_lines_length + count_to_insert - 1);
        while index >= 0 {
            match next_to_insert {
                Some((start, new_lines))
                    if *start > original_line_index + count_inserted_so_far =>
                {
                    for line in new_lines.iter().rev() {
                        self.lines.set(index, Some(line.clone()));
                        index -= 1;
                    }
                    index += 1;
                    count_inserted_so_far += i64::try_from(new_lines.len()).unwrap_or(0);
                    next_to_insert_index += 1;
                    next_to_insert = to_insert.get(next_to_insert_index);
                }
                _ => {
                    let line = usize::try_from(original_line_index)
                        .ok()
                        .and_then(|position| original_lines.get(position).cloned().flatten());
                    self.lines.set(index, line);
                    original_line_index -= 1;
                }
            }
            index -= 1;
        }
    }

    pub(crate) fn setup_tab_stops(&mut self, index: Option<i64>) {
        let mut index = if let Some(index) = index {
            if self.tabs.contains(&index) {
                index
            } else {
                self.prev_stop(Some(index))
            }
        } else {
            self.tabs.clear();
            0
        };
        while index < self.cols {
            self.tabs.insert(index);
            index += TAB_STOP_WIDTH;
        }
    }

    pub(crate) fn prev_stop(&self, x: Option<i64>) -> i64 {
        let mut x = x.unwrap_or(self.x);
        loop {
            x -= 1;
            if self.tabs.contains(&x) || x <= 0 {
                break;
            }
        }
        if x >= self.cols {
            self.cols - 1
        } else {
            x.max(0)
        }
    }

    pub(crate) fn next_stop(&self, x: Option<i64>) -> i64 {
        let mut x = x.unwrap_or(self.x);
        loop {
            x += 1;
            if self.tabs.contains(&x) || x >= self.cols {
                break;
            }
        }
        if x >= self.cols {
            self.cols - 1
        } else {
            x.max(0)
        }
    }
}

/// `copyCellsFrom` between two lines that may be the same line.
pub(crate) fn copy_cells(
    dest: &LineRef,
    src: &LineRef,
    src_col: i64,
    dest_col: i64,
    length: i64,
    apply_in_reverse: bool,
) {
    if std::rc::Rc::ptr_eq(dest, src) {
        dest.borrow_mut()
            .copy_cells_from(None, src_col, dest_col, length, apply_in_reverse);
    } else {
        let source = src.borrow();
        dest.borrow_mut().copy_cells_from(
            Some(&source),
            src_col,
            dest_col,
            length,
            apply_in_reverse,
        );
    }
}

fn wrapped_line(lines: &[Option<LineRef>], index: i64) -> Result<LineRef, Throw> {
    req(usize::try_from(index)
        .ok()
        .and_then(|index| lines.get(index).cloned().flatten()))
}

fn get_wrapped_line_trimmed_length(
    lines: &[Option<LineRef>],
    index: i64,
    cols: i64,
) -> Result<i64, Throw> {
    let last = i64::try_from(lines.len()).unwrap_or(0) - 1;
    if index == last {
        return Ok(wrapped_line(lines, index)?.borrow().get_trimmed_length());
    }
    let line = wrapped_line(lines, index)?;
    let line = line.borrow();
    let ends_in_null = !line.has_content(cols - 1) && line.get_width(cols - 1) == 1;
    let following_line_starts_with_wide =
        wrapped_line(lines, index + 1)?.borrow().get_width(0) == 2;
    if ends_in_null && following_line_starts_with_wide {
        return Ok(cols - 1);
    }
    Ok(cols)
}

fn reflow_larger_get_lines_to_remove(
    lines: &CircularList,
    old_cols: i64,
    new_cols: i64,
    buffer_absolute_y: i64,
    null: &CellData,
) -> Result<Vec<i64>, Throw> {
    let mut to_remove = Vec::new();
    let mut y = 0;
    while y < lines.length() - 1 {
        let mut i = y + 1;
        let mut next_line = lines.get(i);
        if !req(next_line.clone())?.borrow().is_wrapped {
            y += 1;
            continue;
        }
        let mut wrapped_lines: Vec<Option<LineRef>> = vec![lines.get(y)];
        while i < lines.length() && req(next_line.clone())?.borrow().is_wrapped {
            wrapped_lines.push(next_line.clone());
            i += 1;
            next_line = lines.get(i);
        }
        let group_length = i64::try_from(wrapped_lines.len()).unwrap_or(0);
        // `reflowCursorLine` is off: the cursor's wrapped line keeps its layout.
        if buffer_absolute_y >= y && buffer_absolute_y < i {
            y += group_length;
            continue;
        }
        let mut dest_line_index = 0;
        let mut dest_col =
            get_wrapped_line_trimmed_length(&wrapped_lines, dest_line_index, old_cols)?;
        let mut src_line_index = 1;
        let mut src_col = 0;
        while src_line_index < group_length {
            let src_trimmed_line_length =
                get_wrapped_line_trimmed_length(&wrapped_lines, src_line_index, old_cols)?;
            let src_remaining_cells = src_trimmed_line_length - src_col;
            let dest_remaining_cells = new_cols - dest_col;
            let cells_to_copy = src_remaining_cells.min(dest_remaining_cells);
            copy_cells(
                &wrapped_line(&wrapped_lines, dest_line_index)?,
                &wrapped_line(&wrapped_lines, src_line_index)?,
                src_col,
                dest_col,
                cells_to_copy,
                false,
            );
            dest_col += cells_to_copy;
            if dest_col == new_cols {
                dest_line_index += 1;
                dest_col = 0;
            }
            src_col += cells_to_copy;
            if src_col == src_trimmed_line_length {
                src_line_index += 1;
                src_col = 0;
            }
            if dest_col == 0 && dest_line_index != 0 {
                let previous = wrapped_line(&wrapped_lines, dest_line_index - 1)?;
                if previous.borrow().get_width(new_cols - 1) == 2 {
                    copy_cells(
                        &wrapped_line(&wrapped_lines, dest_line_index)?,
                        &previous,
                        new_cols - 1,
                        dest_col,
                        1,
                        false,
                    );
                    dest_col += 1;
                    previous.borrow_mut().set_cell(new_cols - 1, null);
                }
            }
        }
        wrapped_line(&wrapped_lines, dest_line_index)?
            .borrow_mut()
            .replace_cells(dest_col, new_cols, null, false);
        let mut count_to_remove = 0;
        let mut index = group_length - 1;
        while index > 0 {
            if index > dest_line_index
                || wrapped_line(&wrapped_lines, index)?
                    .borrow()
                    .get_trimmed_length()
                    == 0
            {
                count_to_remove += 1;
            } else {
                break;
            }
            index -= 1;
        }
        if count_to_remove > 0 {
            to_remove.push(y + group_length - count_to_remove);
            to_remove.push(count_to_remove);
        }
        y += group_length;
    }
    Ok(to_remove)
}

fn reflow_larger_create_new_layout(lines: &CircularList, to_remove: &[i64]) -> (Vec<i64>, i64) {
    let mut layout = Vec::new();
    let mut next_to_remove_index = 0;
    let mut next_to_remove_start = to_remove.first().copied();
    let mut count_removed_so_far = 0;
    let mut index = 0;
    while index < lines.length() {
        if next_to_remove_start == Some(index) {
            next_to_remove_index += 1;
            let count_to_remove = to_remove.get(next_to_remove_index).copied().unwrap_or(0);
            index += count_to_remove - 1;
            count_removed_so_far += count_to_remove;
            next_to_remove_index += 1;
            next_to_remove_start = to_remove.get(next_to_remove_index).copied();
        } else {
            layout.push(index);
        }
        index += 1;
    }
    (layout, count_removed_so_far)
}

fn reflow_larger_apply_new_layout(lines: &mut CircularList, new_layout: &[i64]) {
    let new_layout_lines: Vec<Option<LineRef>> =
        new_layout.iter().map(|&index| lines.get(index)).collect();
    for (index, line) in (0..).zip(new_layout_lines) {
        lines.set(index, line);
    }
    lines.set_length(i64::try_from(new_layout.len()).unwrap_or(0));
}

fn reflow_smaller_get_new_line_lengths(
    wrapped_lines: &[Option<LineRef>],
    old_cols: i64,
    new_cols: i64,
) -> Result<Vec<i64>, Throw> {
    let mut new_line_lengths = Vec::new();
    let mut cells_needed = 0;
    for index in 0..i64::try_from(wrapped_lines.len()).unwrap_or(0) {
        cells_needed += get_wrapped_line_trimmed_length(wrapped_lines, index, old_cols)?;
    }
    let mut src_col = 0;
    let mut src_line = 0;
    let mut cells_available = 0;
    while cells_available < cells_needed {
        if cells_needed - cells_available < new_cols {
            new_line_lengths.push(cells_needed - cells_available);
            break;
        }
        src_col += new_cols;
        let old_trimmed_length =
            get_wrapped_line_trimmed_length(wrapped_lines, src_line, old_cols)?;
        if src_col > old_trimmed_length {
            src_col -= old_trimmed_length;
            src_line += 1;
        }
        let ends_with_wide = wrapped_line(wrapped_lines, src_line)?
            .borrow()
            .get_width(src_col - 1)
            == 2;
        if ends_with_wide {
            src_col -= 1;
        }
        let line_length = if ends_with_wide {
            new_cols - 1
        } else {
            new_cols
        };
        new_line_lengths.push(line_length);
        cells_available += line_length;
    }
    Ok(new_line_lengths)
}

/// `BufferSet`: the normal buffer with scrollback and the alternate screen.
#[derive(Debug)]
pub(crate) struct BufferSet {
    pub(crate) normal: Buffer,
    pub(crate) alt: Buffer,
    pub(crate) alt_active: bool,
}

impl BufferSet {
    pub(crate) fn new(cols: i64, rows: i64) -> Self {
        let mut normal = Buffer::new(true, cols, rows);
        normal.fill_viewport_rows(None, cols);
        let mut set = Self {
            normal,
            alt: Buffer::new(false, cols, rows),
            alt_active: false,
        };
        set.setup_tab_stops(None);
        set
    }

    pub(crate) fn active(&self) -> &Buffer {
        if self.alt_active {
            &self.alt
        } else {
            &self.normal
        }
    }

    pub(crate) fn active_mut(&mut self) -> &mut Buffer {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.normal
        }
    }

    pub(crate) fn activate_normal_buffer(&mut self) {
        if !self.alt_active {
            return;
        }
        self.normal.x = self.alt.x;
        self.normal.y = self.alt.y;
        self.alt.clear();
        self.alt_active = false;
    }

    pub(crate) fn activate_alt_buffer(&mut self, fill_attr: AttributeData, service_cols: i64) {
        if self.alt_active {
            return;
        }
        self.alt.fill_viewport_rows(Some(fill_attr), service_cols);
        self.alt.x = self.normal.x;
        self.alt.y = self.normal.y;
        self.alt_active = true;
    }

    pub(crate) fn resize(&mut self, new_cols: i64, new_rows: i64) -> Result<(), Throw> {
        self.normal.resize(new_cols, new_rows, new_cols)?;
        self.alt.resize(new_cols, new_rows, new_cols)?;
        self.setup_tab_stops(Some(new_cols));
        Ok(())
    }

    fn setup_tab_stops(&mut self, index: Option<i64>) {
        self.normal.setup_tab_stops(index);
        self.alt.setup_tab_stops(index);
    }

    /// `BufferService.scroll` on the active buffer. The user never scrolls
    /// the viewport in Paseo, so `isUserScrolling` stays false.
    pub(crate) fn scroll(
        &mut self,
        erase_attr: AttributeData,
        is_wrapped: bool,
        service_cols: i64,
    ) -> Result<(), Throw> {
        let buffer = self.active_mut();
        let new_line = Buffer::blank_line(erase_attr, is_wrapped, service_cols);
        let top_row = buffer.ybase + buffer.scroll_top;
        let bottom_row = buffer.ybase + buffer.scroll_bottom;
        if buffer.scroll_top == 0 {
            let will_buffer_be_trimmed = buffer.lines.is_full();
            if bottom_row == buffer.lines.length() - 1 {
                if will_buffer_be_trimmed {
                    let recycled = req(buffer.lines.recycle())?;
                    recycled.borrow_mut().copy_from(&new_line.borrow());
                } else {
                    buffer.lines.push(new_line);
                }
            } else {
                buffer.lines.splice(bottom_row + 1, 0, &[new_line]);
            }
            if !will_buffer_be_trimmed {
                buffer.ybase += 1;
            }
        } else {
            let scroll_region_height = bottom_row - top_row + 1;
            buffer
                .lines
                .shift_elements(top_row + 1, scroll_region_height - 1, -1)?;
            buffer.lines.set(bottom_row, Some(new_line));
        }
        buffer.ydisp = buffer.ybase;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::BufferSet;
    use crate::attributes::AttributeData;

    #[test]
    fn scrolling_past_the_screen_grows_scrollback() {
        let mut buffers = BufferSet::new(4, 2);
        for _ in 0..3 {
            buffers
                .scroll(AttributeData::default(), false, 4)
                .expect("scroll");
        }
        let normal = buffers.active();
        assert_eq!(normal.ybase, 3);
        assert_eq!(normal.lines.length(), 5);
    }

    #[test]
    fn default_tab_stops_every_eight_columns() {
        let buffers = BufferSet::new(20, 2);
        let normal = buffers.active();
        assert_eq!(normal.next_stop(Some(0)), 8);
        assert_eq!(normal.next_stop(Some(16)), 19);
        assert_eq!(normal.prev_stop(Some(9)), 8);
    }
}
