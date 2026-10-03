//! `Array.prototype.sort(compareFn)` as V8 runs it (`TimSort`, the Torque
//! `array-sort.tq` shipped in node v22.20.0).
//!
//! The order of equal elements is stable, but a comparator that is not a
//! consistent ordering (`NaN` differences that fall through to a tiebreak,
//! for example) gives a result that depends on the algorithm's exact
//! comparison sequence. Callers that read `sort(...)[0]` need this port, not
//! a minimum scan.
//!
//! The elements are plain values: the baseline's handling of `undefined` and
//! holes (moved to the end before sorting) does not apply.

/// `kMinGallopWins` in `array-sort.tq`.
const MIN_GALLOP_WINS: usize = 7;

/// Index arithmetic of the galloping search runs below zero before it is
/// clamped, as in the baseline's Smi arithmetic.
type Index = isize;

fn index(value: Index) -> usize {
    usize::try_from(value).expect("sort index is not negative")
}

fn signed(value: usize) -> Index {
    Index::try_from(value).expect("sort length fits an index")
}

/// Sorts `items` in place like `items.sort(compare)`; `compare` returns what
/// the baseline's comparator returns (`NaN` counts as `0`, as `ToNumber`
/// then `NaN -> +0` does in `SortCompareUserFn`).
pub fn js_sort_by<T: Clone>(items: &mut [T], compare: impl FnMut(&T, &T) -> f64) {
    let length = items.len();
    if length < 2 {
        return;
    }
    let mut sorter = Sorter {
        work: items.to_vec(),
        compare,
        min_gallop: MIN_GALLOP_WINS,
        runs: Vec::new(),
    };
    sorter.run();
    items.clone_from_slice(&sorter.work);
}

struct Sorter<T, F> {
    work: Vec<T>,
    compare: F,
    min_gallop: usize,
    /// `(base, length)` of each pending run.
    runs: Vec<(Index, Index)>,
}

fn order<T>(compare: &mut impl FnMut(&T, &T) -> f64, left: &T, right: &T) -> f64 {
    let result = compare(left, right);
    if result.is_nan() { 0.0 } else { result }
}

/// `ComputeMinRunLength`.
fn min_run_length(mut remaining: Index) -> Index {
    let mut carry = 0;
    while remaining >= 64 {
        carry |= remaining & 1;
        remaining >>= 1;
    }
    remaining + carry
}

/// `GallopLeft`: where `key` goes among `array[base..base + length]`, to the
/// left of equal elements.
fn gallop_left<T>(
    compare: &mut impl FnMut(&T, &T) -> f64,
    array: &[T],
    key: &T,
    base: Index,
    length: Index,
    hint: Index,
) -> Index {
    let mut last_offset: Index = 0;
    let mut offset: Index = 1;
    let at = |position: Index| &array[index(base + position)];
    if order(compare, at(hint), key) < 0.0 {
        // a[hint] < key: gallop right until a[hint + last] < key <= a[hint + offset].
        let max_offset = length - hint;
        while offset < max_offset {
            if order(compare, at(hint + offset), key) >= 0.0 {
                break;
            }
            last_offset = offset;
            offset = (offset << 1) + 1;
        }
        offset = offset.min(max_offset);
        last_offset += hint;
        offset += hint;
    } else {
        // key <= a[hint]: gallop left until a[hint - offset] < key <= a[hint - last].
        let max_offset = hint + 1;
        while offset < max_offset {
            if order(compare, at(hint - offset), key) < 0.0 {
                break;
            }
            last_offset = offset;
            offset = (offset << 1) + 1;
        }
        offset = offset.min(max_offset);
        let previous = last_offset;
        last_offset = hint - offset;
        offset = hint - previous;
    }
    // a[last] < key <= a[offset]: binary search the gap.
    last_offset += 1;
    while last_offset < offset {
        let middle = last_offset + ((offset - last_offset) >> 1);
        if order(compare, at(middle), key) < 0.0 {
            last_offset = middle + 1;
        } else {
            offset = middle;
        }
    }
    offset
}

/// `GallopRight`: like [`gallop_left`], to the right of equal elements.
fn gallop_right<T>(
    compare: &mut impl FnMut(&T, &T) -> f64,
    array: &[T],
    key: &T,
    base: Index,
    length: Index,
    hint: Index,
) -> Index {
    let mut last_offset: Index = 0;
    let mut offset: Index = 1;
    let at = |position: Index| &array[index(base + position)];
    if order(compare, key, at(hint)) < 0.0 {
        // key < a[hint]: gallop left until a[hint - offset] <= key < a[hint - last].
        let max_offset = hint + 1;
        while offset < max_offset {
            if order(compare, key, at(hint - offset)) >= 0.0 {
                break;
            }
            last_offset = offset;
            offset = (offset << 1) + 1;
        }
        offset = offset.min(max_offset);
        let previous = last_offset;
        last_offset = hint - offset;
        offset = hint - previous;
    } else {
        // a[hint] <= key: gallop right until a[hint + last] <= key < a[hint + offset].
        let max_offset = length - hint;
        while offset < max_offset {
            if order(compare, key, at(hint + offset)) < 0.0 {
                break;
            }
            last_offset = offset;
            offset = (offset << 1) + 1;
        }
        offset = offset.min(max_offset);
        last_offset += hint;
        offset += hint;
    }
    last_offset += 1;
    while last_offset < offset {
        let middle = last_offset + ((offset - last_offset) >> 1);
        if order(compare, key, at(middle)) < 0.0 {
            offset = middle;
        } else {
            last_offset = middle + 1;
        }
    }
    offset
}

impl<T: Clone, F: FnMut(&T, &T) -> f64> Sorter<T, F> {
    fn run(&mut self) {
        let mut low: Index = 0;
        let mut remaining = signed(self.work.len());
        let minimum = min_run_length(remaining);
        while remaining != 0 {
            let mut run_length = self.count_and_make_run(low, low + remaining);
            if run_length < minimum {
                let forced = minimum.min(remaining);
                self.binary_insertion_sort(low, low + run_length, low + forced);
                run_length = forced;
            }
            self.runs.push((low, run_length));
            self.merge_collapse();
            low += run_length;
            remaining -= run_length;
        }
        self.merge_force_collapse();
    }

    fn get(&self, position: Index) -> &T {
        &self.work[index(position)]
    }

    /// `CountAndMakeRun`: the length of the run at `low`, reversed in place
    /// when it is strictly descending.
    fn count_and_make_run(&mut self, low_argument: Index, high: Index) -> Index {
        let low = low_argument + 1;
        if low == high {
            return 1;
        }
        let mut run_length: Index = 2;
        let element_low = self.get(low).clone();
        let element_high = self.get(low - 1).clone();
        let mut result = order(&mut self.compare, &element_low, &element_high);
        let descending = result < 0.0;
        let mut previous = element_low;
        let mut position = low + 1;
        while position < high {
            let current = self.get(position).clone();
            result = order(&mut self.compare, &current, &previous);
            if descending {
                if result >= 0.0 {
                    break;
                }
            } else if result < 0.0 {
                break;
            }
            previous = current;
            run_length += 1;
            position += 1;
        }
        if descending {
            self.work[index(low_argument)..index(low_argument + run_length)].reverse();
        }
        run_length
    }

    /// `BinaryInsertionSort`: extends the sorted `[low, start)` to `high`.
    fn binary_insertion_sort(&mut self, low: Index, start_argument: Index, high: Index) {
        let mut start = if low == start_argument {
            start_argument + 1
        } else {
            start_argument
        };
        while start < high {
            let mut left = low;
            let mut right = start;
            let pivot = self.get(right).clone();
            while left < right {
                let middle = left + ((right - left) >> 1);
                if order(&mut self.compare, &pivot, &self.work[index(middle)]) < 0.0 {
                    right = middle;
                } else {
                    left = middle + 1;
                }
            }
            // Shift [left, start) up one place; the pivot lands at `left`.
            self.work[index(left)..=index(start)].rotate_right(1);
            start += 1;
        }
    }

    fn run_invariant_established(&self, position: usize) -> bool {
        position < 2
            || self.runs[position - 2].1 > self.runs[position - 1].1 + self.runs[position].1
    }

    /// `MergeCollapse`.
    fn merge_collapse(&mut self) {
        while self.runs.len() > 1 {
            let mut position = self.runs.len() - 2;
            if !self.run_invariant_established(position + 1)
                || !self.run_invariant_established(position)
            {
                if self.runs[position - 1].1 < self.runs[position + 1].1 {
                    position -= 1;
                }
                self.merge_at(position);
            } else if self.runs[position].1 <= self.runs[position + 1].1 {
                self.merge_at(position);
            } else {
                break;
            }
        }
    }

    /// `MergeForceCollapse`.
    fn merge_force_collapse(&mut self) {
        while self.runs.len() > 1 {
            let mut position = self.runs.len() - 2;
            if position > 0 && self.runs[position - 1].1 < self.runs[position + 1].1 {
                position -= 1;
            }
            self.merge_at(position);
        }
    }

    /// `MergeAt`: merges the runs at `position` and `position + 1`.
    fn merge_at(&mut self, position: usize) {
        let (mut base_a, mut length_a) = self.runs[position];
        let (base_b, mut length_b) = self.runs[position + 1];
        self.runs[position].1 = length_a + length_b;
        self.runs.remove(position + 1);
        // Where does b[0] go in a? Elements before it are already in place.
        let key_right = self.get(base_b).clone();
        let skipped = gallop_right(
            &mut self.compare,
            &self.work,
            &key_right,
            base_a,
            length_a,
            0,
        );
        base_a += skipped;
        length_a -= skipped;
        if length_a == 0 {
            return;
        }
        // Where does a's last element go in b? Elements after it are in place.
        let key_left = self.get(base_a + length_a - 1).clone();
        length_b = gallop_left(
            &mut self.compare,
            &self.work,
            &key_left,
            base_b,
            length_b,
            length_b - 1,
        );
        if length_b == 0 {
            return;
        }
        if length_a <= length_b {
            self.merge_low(base_a, length_a, base_b, length_b);
        } else {
            self.merge_high(base_a, length_a, base_b, length_b);
        }
    }

    fn copy(&mut self, from: &[T], from_start: Index, to_start: Index, count: Index) {
        let (from_start, to_start, count) = (index(from_start), index(to_start), index(count));
        self.work[to_start..to_start + count]
            .clone_from_slice(&from[from_start..from_start + count]);
    }

    /// `Copy` within the work array, in the direction that survives overlap.
    fn copy_within(&mut self, from_start: Index, to_start: Index, count: Index) {
        let (from_start, to_start, count) = (index(from_start), index(to_start), index(count));
        if from_start < to_start {
            for offset in (0..count).rev() {
                self.work[to_start + offset] = self.work[from_start + offset].clone();
            }
        } else {
            for offset in 0..count {
                self.work[to_start + offset] = self.work[from_start + offset].clone();
            }
        }
    }

    /// `MergeLow`: merges run A (shorter, copied out) with run B, forwards.
    // A straight port of one Torque macro; splitting it would hide the
    // comparison sequence it must keep.
    #[allow(clippy::too_many_lines)]
    fn merge_low(
        &mut self,
        base_a: Index,
        mut length_a: Index,
        base_b: Index,
        mut length_b: Index,
    ) {
        let temp: Vec<T> = self.work[index(base_a)..index(base_a + length_a)].to_vec();
        let mut destination = base_a;
        let mut cursor_temp: Index = 0;
        let mut cursor_b = base_b;
        self.work[index(destination)] = self.get(cursor_b).clone();
        destination += 1;
        cursor_b += 1;
        length_b -= 1;
        // `finish`: 0 = Succeed, 1 = CopyB.
        let finish = 'merge: {
            if length_b == 0 {
                break 'merge 0;
            }
            if length_a == 1 {
                break 'merge 1;
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut wins_a: usize = 0;
                let mut wins_b: usize = 0;
                // The straightforward merge until one run wins consistently.
                loop {
                    let result = order(
                        &mut self.compare,
                        &self.work[index(cursor_b)],
                        &temp[index(cursor_temp)],
                    );
                    if result < 0.0 {
                        self.work[index(destination)] = self.get(cursor_b).clone();
                        destination += 1;
                        cursor_b += 1;
                        wins_b += 1;
                        length_b -= 1;
                        wins_a = 0;
                        if length_b == 0 {
                            break 'merge 0;
                        }
                        if wins_b >= min_gallop {
                            break;
                        }
                    } else {
                        self.work[index(destination)] = temp[index(cursor_temp)].clone();
                        destination += 1;
                        cursor_temp += 1;
                        wins_a += 1;
                        length_a -= 1;
                        wins_b = 0;
                        if length_a == 1 {
                            break 'merge 1;
                        }
                        if wins_a >= min_gallop {
                            break;
                        }
                    }
                }
                // One run wins so consistently that galloping may pay off.
                min_gallop += 1;
                let mut first_iteration = true;
                while wins_a >= MIN_GALLOP_WINS || wins_b >= MIN_GALLOP_WINS || first_iteration {
                    first_iteration = false;
                    min_gallop = min_gallop.saturating_sub(1).max(1);
                    self.min_gallop = min_gallop;
                    let key = self.get(cursor_b).clone();
                    let taken_a =
                        gallop_right(&mut self.compare, &temp, &key, cursor_temp, length_a, 0);
                    wins_a = index(taken_a);
                    if taken_a > 0 {
                        self.copy(&temp, cursor_temp, destination, taken_a);
                        destination += taken_a;
                        cursor_temp += taken_a;
                        length_a -= taken_a;
                        if length_a == 1 {
                            break 'merge 1;
                        }
                        // Impossible for a consistent comparator, not assumed.
                        if length_a == 0 {
                            break 'merge 0;
                        }
                    }
                    self.work[index(destination)] = self.get(cursor_b).clone();
                    destination += 1;
                    cursor_b += 1;
                    length_b -= 1;
                    if length_b == 0 {
                        break 'merge 0;
                    }
                    let key = temp[index(cursor_temp)].clone();
                    let taken_b =
                        gallop_left(&mut self.compare, &self.work, &key, cursor_b, length_b, 0);
                    wins_b = index(taken_b);
                    if taken_b > 0 {
                        self.copy_within(cursor_b, destination, taken_b);
                        destination += taken_b;
                        cursor_b += taken_b;
                        length_b -= taken_b;
                        if length_b == 0 {
                            break 'merge 0;
                        }
                    }
                    self.work[index(destination)] = temp[index(cursor_temp)].clone();
                    destination += 1;
                    cursor_temp += 1;
                    length_a -= 1;
                    if length_a == 1 {
                        break 'merge 1;
                    }
                }
                // Penalize leaving galloping mode.
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        };
        if finish == 0 {
            if length_a > 0 {
                self.copy(&temp, cursor_temp, destination, length_a);
            }
        } else {
            // The last element of run A belongs at the end of the merge.
            self.copy_within(cursor_b, destination, length_b);
            self.work[index(destination + length_b)] = temp[index(cursor_temp)].clone();
        }
    }

    /// `MergeHigh`: merges run A with run B (shorter, copied out), backwards.
    // A straight port of one Torque macro; splitting it would hide the
    // comparison sequence it must keep.
    #[allow(clippy::too_many_lines)]
    fn merge_high(
        &mut self,
        base_a: Index,
        mut length_a: Index,
        base_b: Index,
        mut length_b: Index,
    ) {
        let temp: Vec<T> = self.work[index(base_b)..index(base_b + length_b)].to_vec();
        let mut destination = base_b + length_b - 1;
        let mut cursor_temp = length_b - 1;
        let mut cursor_a = base_a + length_a - 1;
        self.work[index(destination)] = self.get(cursor_a).clone();
        destination -= 1;
        cursor_a -= 1;
        length_a -= 1;
        // `finish`: 0 = Succeed, 1 = CopyA.
        let finish = 'merge: {
            if length_a == 0 {
                break 'merge 0;
            }
            if length_b == 1 {
                break 'merge 1;
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut wins_a: usize = 0;
                let mut wins_b: usize = 0;
                loop {
                    let result = order(
                        &mut self.compare,
                        &temp[index(cursor_temp)],
                        &self.work[index(cursor_a)],
                    );
                    if result < 0.0 {
                        self.work[index(destination)] = self.get(cursor_a).clone();
                        destination -= 1;
                        cursor_a -= 1;
                        wins_a += 1;
                        length_a -= 1;
                        wins_b = 0;
                        if length_a == 0 {
                            break 'merge 0;
                        }
                        if wins_a >= min_gallop {
                            break;
                        }
                    } else {
                        self.work[index(destination)] = temp[index(cursor_temp)].clone();
                        destination -= 1;
                        cursor_temp -= 1;
                        wins_b += 1;
                        length_b -= 1;
                        wins_a = 0;
                        if length_b == 1 {
                            break 'merge 1;
                        }
                        if wins_b >= min_gallop {
                            break;
                        }
                    }
                }
                min_gallop += 1;
                let mut first_iteration = true;
                while wins_a >= MIN_GALLOP_WINS || wins_b >= MIN_GALLOP_WINS || first_iteration {
                    first_iteration = false;
                    min_gallop = min_gallop.saturating_sub(1).max(1);
                    self.min_gallop = min_gallop;
                    let key = temp[index(cursor_temp)].clone();
                    let k = gallop_right(
                        &mut self.compare,
                        &self.work,
                        &key,
                        base_a,
                        length_a,
                        length_a - 1,
                    );
                    let taken_a = length_a - k;
                    wins_a = index(taken_a);
                    if taken_a > 0 {
                        destination -= taken_a;
                        cursor_a -= taken_a;
                        self.copy_within(cursor_a + 1, destination + 1, taken_a);
                        length_a -= taken_a;
                        if length_a == 0 {
                            break 'merge 0;
                        }
                    }
                    self.work[index(destination)] = temp[index(cursor_temp)].clone();
                    destination -= 1;
                    cursor_temp -= 1;
                    length_b -= 1;
                    if length_b == 1 {
                        break 'merge 1;
                    }
                    let key = self.get(cursor_a).clone();
                    let k = gallop_left(&mut self.compare, &temp, &key, 0, length_b, length_b - 1);
                    let taken_b = length_b - k;
                    wins_b = index(taken_b);
                    if taken_b > 0 {
                        destination -= taken_b;
                        cursor_temp -= taken_b;
                        self.copy(&temp, cursor_temp + 1, destination + 1, taken_b);
                        length_b -= taken_b;
                        if length_b == 1 {
                            break 'merge 1;
                        }
                        // Impossible for a consistent comparator, not assumed.
                        if length_b == 0 {
                            break 'merge 0;
                        }
                    }
                    self.work[index(destination)] = self.get(cursor_a).clone();
                    destination -= 1;
                    cursor_a -= 1;
                    length_a -= 1;
                    if length_a == 0 {
                        break 'merge 0;
                    }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        };
        if finish == 0 {
            if length_b > 0 {
                self.copy(&temp, 0, destination - (length_b - 1), length_b);
            }
        } else {
            // The first element of run B belongs at the front of the merge.
            destination -= length_a;
            cursor_a -= length_a;
            self.copy_within(cursor_a + 1, destination + 1, length_a);
            self.work[index(destination)] = temp[index(cursor_temp)].clone();
        }
    }
}
