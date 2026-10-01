//! The fixed-capacity ring of buffer lines behind scrollback.
//!
//! Ported from xterm.js 6.0.0 `src/common/CircularList.ts`.
//! Copyright (c) 2016 The xterm.js authors. MIT License.
//!
//! Slots keep whatever they last held: `get` past `length` returns a stale
//! line (or nothing) exactly as the JavaScript array does, and the `length`
//! setter clears raw indexes rather than ring positions, as xterm does.

use crate::buffer_line::LineRef;

/// A JavaScript exception (a `TypeError` from reading a property of
/// `undefined`, or an `Error` from `shiftElements`) that aborts the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Throw;

#[derive(Debug)]
pub(crate) struct CircularList {
    array: Vec<Option<LineRef>>,
    start_index: i64,
    length: i64,
    max_length: i64,
}

impl CircularList {
    pub(crate) fn new(max_length: i64) -> Self {
        Self {
            array: vec![None; usize::try_from(max_length).unwrap_or(0)],
            start_index: 0,
            length: 0,
            max_length,
        }
    }

    pub(crate) fn max_length(&self) -> i64 {
        self.max_length
    }

    pub(crate) fn set_max_length(&mut self, new_max_length: i64) {
        if self.max_length == new_max_length {
            return;
        }
        let mut new_array = vec![None; usize::try_from(new_max_length).unwrap_or(0)];
        for index in 0..new_max_length.min(self.length) {
            new_array[usize::try_from(index).unwrap_or(0)] = self.get(index);
        }
        self.array = new_array;
        self.max_length = new_max_length;
        self.start_index = 0;
    }

    pub(crate) fn length(&self) -> i64 {
        self.length
    }

    pub(crate) fn set_length(&mut self, new_length: i64) {
        if new_length > self.length {
            for index in self.length..new_length {
                if let Some(slot) = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.array.get_mut(index))
                {
                    *slot = None;
                }
            }
        }
        self.length = new_length;
    }

    fn cyclic_index(&self, index: i64) -> Option<usize> {
        usize::try_from((self.start_index + index) % self.max_length).ok()
    }

    pub(crate) fn get(&self, index: i64) -> Option<LineRef> {
        self.cyclic_index(index)
            .and_then(|slot| self.array.get(slot))
            .and_then(Clone::clone)
    }

    /// Sets a ring position. Callers never pass a position before the start.
    pub(crate) fn set(&mut self, index: i64, value: Option<LineRef>) {
        if let Some(slot) = self.cyclic_index(index) {
            self.array[slot] = value;
        }
    }

    pub(crate) fn push(&mut self, value: LineRef) {
        self.set(self.length, Some(value));
        if self.length == self.max_length {
            self.start_index = (self.start_index + 1) % self.max_length;
        } else {
            self.length += 1;
        }
    }

    /// Advances the start over a full ring and returns the line now last.
    pub(crate) fn recycle(&mut self) -> Option<LineRef> {
        self.start_index = (self.start_index + 1) % self.max_length;
        self.get(self.length - 1)
    }

    pub(crate) fn is_full(&self) -> bool {
        self.length == self.max_length
    }

    pub(crate) fn pop(&mut self) {
        self.length -= 1;
    }

    pub(crate) fn splice(&mut self, start: i64, delete_count: i64, items: &[LineRef]) {
        if delete_count != 0 {
            for index in start..self.length - delete_count {
                let moved = self.get(index + delete_count);
                self.set(index, moved);
            }
            self.length -= delete_count;
        }
        let count = i64::try_from(items.len()).unwrap_or(0);
        let mut index = self.length - 1;
        while index >= start {
            let moved = self.get(index);
            self.set(index + count, moved);
            index -= 1;
        }
        for (offset, item) in (0..).zip(items) {
            self.set(start + offset, Some(item.clone()));
        }
        if self.length + count > self.max_length {
            let count_to_trim = self.length + count - self.max_length;
            self.start_index += count_to_trim;
            self.length = self.max_length;
        } else {
            self.length += count;
        }
    }

    pub(crate) fn trim_start(&mut self, count: i64) {
        let count = count.min(self.length);
        self.start_index += count;
        self.length -= count;
    }

    pub(crate) fn shift_elements(
        &mut self,
        start: i64,
        count: i64,
        offset: i64,
    ) -> Result<(), Throw> {
        if count <= 0 {
            return Ok(());
        }
        if start < 0 || start >= self.length || start + offset < 0 {
            return Err(Throw);
        }
        if offset > 0 {
            let mut index = count - 1;
            while index >= 0 {
                let moved = self.get(start + index);
                self.set(start + index + offset, moved);
                index -= 1;
            }
            let expand_list_by = start + count + offset - self.length;
            if expand_list_by > 0 {
                self.length += expand_list_by;
                while self.length > self.max_length {
                    self.length -= 1;
                    self.start_index += 1;
                }
            }
        } else {
            for index in 0..count {
                let moved = self.get(start + index);
                self.set(start + index + offset, moved);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CircularList;
    use crate::attributes::{AttributeData, CellData};
    use crate::buffer_line::{BufferLine, LineRef};
    use std::rc::Rc;

    fn line() -> LineRef {
        BufferLine::new(2, &CellData::null_with(AttributeData::default()), false).into_ref()
    }

    #[test]
    fn push_trims_the_oldest_line_when_full() {
        let mut list = CircularList::new(2);
        let (first, second, third) = (line(), line(), line());
        list.push(first.clone());
        list.push(second.clone());
        list.push(third.clone());
        assert_eq!(list.length(), 2);
        assert!(Rc::ptr_eq(&list.get(0).expect("line"), &second));
        assert!(Rc::ptr_eq(&list.get(1).expect("line"), &third));
    }

    #[test]
    fn splice_leaves_stale_slots_past_the_length() {
        let mut list = CircularList::new(4);
        let lines: Vec<LineRef> = (0..3).map(|_| line()).collect();
        for item in &lines {
            list.push(item.clone());
        }
        list.splice(0, 1, &[]);
        assert_eq!(list.length(), 2);
        assert!(Rc::ptr_eq(&list.get(2).expect("stale"), &lines[2]));
        assert!(list.get(3).is_none());
    }
}
