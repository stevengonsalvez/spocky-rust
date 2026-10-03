//! The IDNA step of ada 2.9.2's `ada::idna::to_ascii` (`src/ada_idna.cpp`),
//! the host-to-ASCII conversion node v22.20.0 runs for every special URL.
//!
//! It is `UTS 46` processing with `CheckHyphens` off, `CheckBidi` and
//! `CheckJoiners` on, non-transitional mapping, and no length verification, over the Unicode
//! 15.0 tables of [`crate::url_tables`]. The port keeps ada's own behavior
//! where it departs from UTS 46: ASCII labels are copied unchecked, a label
//! that already holds a ZWNJ or ZWJ rule result skips the Bidi rule, the
//! punycode routines wrap `int32` and `uint32` arithmetic as C++ does, and a
//! failure is an empty result.

// The punycode, composition, and Bidi routines keep the single-letter names of
// ada's code (n, i, k, t, w, ...), so the port reads against the original.
//
// The casts between `u8`, `u16`, `i32`, `u32`, and `usize` are ada's own C++
// conversions, whose truncation and wrapping the port reproduces.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names
)]

use crate::url_tables::{
    COMBINING_CLASS_BLOCK, COMBINING_CLASS_INDEX, COMBINING_MARKS, COMPOSITION_BLOCK,
    COMPOSITION_DATA, COMPOSITION_INDEX, DECOMPOSITION_BLOCK, DECOMPOSITION_DATA,
    DECOMPOSITION_INDEX, DIRECTIONS, JOINING_D, JOINING_L, JOINING_R, MAPPINGS, RANGES, VIRAMA,
};

const HANGUL_SBASE: u32 = 0xAC00;
const HANGUL_TBASE: u32 = 0x11A7;
const HANGUL_VBASE: u32 = 0x1161;
const HANGUL_LBASE: u32 = 0x1100;
const HANGUL_LCOUNT: u32 = 19;
const HANGUL_VCOUNT: u32 = 21;
const HANGUL_TCOUNT: u32 = 28;
const HANGUL_NCOUNT: u32 = HANGUL_VCOUNT * HANGUL_TCOUNT;
const HANGUL_SCOUNT: u32 = HANGUL_LCOUNT * HANGUL_VCOUNT * HANGUL_TCOUNT;

/// `to_ascii(utf8)`: the ASCII form of a domain, or an empty vector where ada
/// returns the empty string (an error, or nothing left after mapping).
#[must_use]
pub fn to_ascii(input: &[u8]) -> Vec<u8> {
    if input.is_ascii() {
        return from_ascii_to_ascii(input);
    }
    // `utf8_to_utf32` fails on invalid UTF-8 and on an empty result.
    let Ok(text) = std::str::from_utf8(input) else {
        return Vec::new();
    };
    let mut utf32: Vec<u32> = text.chars().map(u32::from).collect();
    if utf32.is_empty() {
        return Vec::new();
    }
    utf32 = map(&utf32);
    normalize(&mut utf32);
    let mut out: Vec<u8> = Vec::new();
    let mut label_start = 0;
    while label_start != utf32.len() {
        let location = utf32[label_start..]
            .iter()
            .position(|unit| *unit == u32::from(b'.'));
        let is_last = location.is_none();
        let label_size = location.unwrap_or(utf32.len() - label_start);
        let label = &utf32[label_start..label_start + label_size];
        label_start += if is_last { label_size } else { label_size + 1 };
        if label_size == 0 {
            // An empty label: nothing to do.
        } else if label.starts_with(&[0x78, 0x6e, 0x2d, 0x2d]) {
            for unit in label {
                if *unit >= 0x80 {
                    return Vec::new();
                }
                out.push(*unit as u8);
            }
            let segment = out[out.len() - label.len() + 4..].to_vec();
            if !verify_xn_label(&segment) {
                return Vec::new();
            }
        } else if label.iter().all(|unit| *unit < 0x80) {
            out.extend(label.iter().map(|unit| *unit as u8));
        } else {
            if !is_label_valid(label) {
                return Vec::new();
            }
            out.extend_from_slice(b"xn--");
            if !utf32_to_punycode(label, &mut out) {
                return Vec::new();
            }
        }
        if !is_last {
            out.push(b'.');
        }
    }
    out
}

/// The ASCII path: lowercase, and check every `xn--` label.
fn from_ascii_to_ascii(input: &[u8]) -> Vec<u8> {
    let mapped = input.to_ascii_lowercase();
    let mut out: Vec<u8> = Vec::new();
    let mut label_start = 0;
    while label_start != mapped.len() {
        let location = mapped[label_start..].iter().position(|byte| *byte == b'.');
        let is_last = location.is_none();
        let label_size = location.unwrap_or(mapped.len() - label_start);
        let label = &mapped[label_start..label_start + label_size];
        label_start += if is_last { label_size } else { label_size + 1 };
        if label_size == 0 {
            // An empty label: nothing to do.
        } else if label.starts_with(b"xn--") {
            out.extend_from_slice(label);
            if !verify_xn_label(&label[4..]) {
                return Vec::new();
            }
        } else {
            out.extend_from_slice(label);
        }
        if !is_last {
            out.push(b'.');
        }
    }
    out
}

/// The checks both paths run on the punycode part of an `xn--` label.
fn verify_xn_label(segment: &[u8]) -> bool {
    let mut decoded: Vec<u32> = Vec::new();
    if !punycode_to_utf32(segment, &mut decoded) {
        return false;
    }
    let post_map = map(&decoded);
    if decoded != post_map {
        return false;
    }
    let mut normalized = post_map.clone();
    normalize(&mut normalized);
    if normalized != post_map {
        return false;
    }
    if normalized.is_empty() {
        return false;
    }
    is_label_valid(&normalized)
}

/// `find_range_index`: the range holding `key`.
fn find_range_index(key: u32) -> usize {
    let length = RANGES.len() / 2;
    let mut low: u32 = 0;
    let mut high: u32 = (length - 1) as u32;
    while low <= high {
        let middle = (low + high) >> 1;
        let middle_value = RANGES[middle as usize * 2];
        match middle_value.cmp(&key) {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle.wrapping_sub(1),
            std::cmp::Ordering::Equal => return middle as usize,
        }
    }
    if low == 0 { 0 } else { low as usize - 1 }
}

/// `map`: the IDNA mapping step; an empty result for a disallowed code point.
fn map(input: &[u32]) -> Vec<u32> {
    let mut answer = Vec::with_capacity(input.len());
    for &unit in input {
        let index = find_range_index(unit);
        let descriptor = RANGES[index * 2 + 1];
        match descriptor as u8 {
            0 => {}
            1 => answer.push(unit),
            2 => return Vec::new(),
            _ => {
                let count = (descriptor >> 24) as usize;
                let start = usize::from((descriptor >> 8) as u16);
                answer.extend_from_slice(&MAPPINGS[start..start + count]);
            }
        }
    }
    answer
}

fn decomposition_at(unit: u32) -> (u16, u16) {
    let block = usize::from(DECOMPOSITION_INDEX[(unit >> 8) as usize]);
    let at = block * 257 + (unit % 256) as usize;
    (DECOMPOSITION_BLOCK[at], DECOMPOSITION_BLOCK[at + 1])
}

fn decomposition_length(unit: u32) -> usize {
    if (HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT).contains(&unit) {
        return if (unit - HANGUL_SBASE).is_multiple_of(HANGUL_TCOUNT) {
            2
        } else {
            3
        };
    }
    if unit < 0x11_0000 {
        let (first, second) = decomposition_at(unit);
        let length = usize::from(second >> 2) - usize::from(first >> 2);
        if length > 0 && first & 1 == 1 {
            return 0;
        }
        return length;
    }
    0
}

fn combining_class(unit: u32) -> u8 {
    if unit < 0x11_0000 {
        let block = usize::from(COMBINING_CLASS_INDEX[(unit >> 8) as usize]);
        COMBINING_CLASS_BLOCK[block * 256 + (unit % 256) as usize]
    } else {
        0
    }
}

/// `decompose` (canonical, non-recursive table lookups, Hangul by formula).
fn decompose(input: &mut Vec<u32>) {
    let mut out: Vec<u32> = Vec::with_capacity(input.len());
    for &unit in input.iter().rev() {
        if (HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT).contains(&unit) {
            let index = unit - HANGUL_SBASE;
            if !index.is_multiple_of(HANGUL_TCOUNT) {
                out.push(HANGUL_TBASE + index % HANGUL_TCOUNT);
            }
            out.push(HANGUL_VBASE + (index % HANGUL_NCOUNT) / HANGUL_TCOUNT);
            out.push(HANGUL_LBASE + index / HANGUL_NCOUNT);
        } else if unit < 0x11_0000 {
            let length = decomposition_length(unit);
            if length > 0 {
                let (first, _) = decomposition_at(unit);
                let start = usize::from(first >> 2);
                for offset in (0..length).rev() {
                    out.push(DECOMPOSITION_DATA[start + offset]);
                }
            } else {
                out.push(unit);
            }
        } else {
            out.push(unit);
        }
    }
    out.reverse();
    *input = out;
}

/// `sort_marks`: a stable insertion sort of combining marks by class.
fn sort_marks(input: &mut [u32]) {
    for index in 1..input.len() {
        let class = combining_class(input[index]);
        if class == 0 {
            continue;
        }
        let current = input[index];
        let mut back = index;
        while back != 0 && combining_class(input[back - 1]) > class {
            input[back] = input[back - 1];
            back -= 1;
        }
        input[back] = current;
    }
}

fn composition_row(unit: u32) -> usize {
    usize::from(COMPOSITION_INDEX[(unit >> 8) as usize]) * 257 + (unit % 256) as usize
}

/// `compose`: canonical composition, including ada's Hangul rules.
fn compose(input: &mut Vec<u32>) {
    let mut input_count = 0;
    let mut composition_count = 0;
    while input_count < input.len() {
        input[composition_count] = input[input_count];
        let unit = input[input_count];
        if (HANGUL_LBASE..HANGUL_LBASE + HANGUL_LCOUNT).contains(&unit) {
            if input_count + 1 < input.len()
                && (HANGUL_VBASE..HANGUL_VBASE + HANGUL_VCOUNT).contains(&input[input_count + 1])
            {
                input[composition_count] = HANGUL_SBASE
                    + ((unit - HANGUL_LBASE) * HANGUL_VCOUNT + input[input_count + 1]
                        - HANGUL_VBASE)
                        * HANGUL_TCOUNT;
                input_count += 1;
                if input_count + 1 < input.len()
                    && input[input_count + 1] > HANGUL_TBASE
                    && input[input_count + 1] < HANGUL_TBASE + HANGUL_TCOUNT
                {
                    input_count += 1;
                    input[composition_count] += input[input_count] - HANGUL_TBASE;
                }
            }
        } else if (HANGUL_SBASE..HANGUL_SBASE + HANGUL_SCOUNT).contains(&unit) {
            if !(unit - HANGUL_SBASE).is_multiple_of(HANGUL_TCOUNT)
                && input_count + 1 < input.len()
                && input[input_count + 1] > HANGUL_TBASE
                && input[input_count + 1] < HANGUL_TBASE + HANGUL_TCOUNT
            {
                input_count += 1;
                input[composition_count] += input[input_count] - HANGUL_TBASE;
            }
        } else if unit < 0x11_0000 {
            let mut composition = composition_row(unit);
            let initial_composition_count = composition_count;
            let mut previous_class: i32 = -1;
            while input_count + 1 < input.len() {
                let next = input[input_count + 1];
                let class = combining_class(next);
                let first = COMPOSITION_BLOCK[composition];
                let second = COMPOSITION_BLOCK[composition + 1];
                if second != first && previous_class < i32::from(class) {
                    // Try finding a composition.
                    let mut left = u32::from(first);
                    let mut right = u32::from(second);
                    while left + 2 < right {
                        let middle = left + (((right - left) >> 1) & !1);
                        if COMPOSITION_DATA[middle as usize] <= next {
                            left = middle;
                        }
                        if COMPOSITION_DATA[middle as usize] >= next {
                            right = middle;
                        }
                    }
                    if COMPOSITION_DATA[left as usize] == next {
                        let composite = COMPOSITION_DATA[left as usize + 1];
                        input[initial_composition_count] = composite;
                        composition = composition_row(composite);
                        input_count += 1;
                        continue;
                    }
                }
                if class == 0 {
                    break;
                }
                previous_class = i32::from(class);
                composition_count += 1;
                input[composition_count] = next;
                input_count += 1;
            }
        }
        input_count += 1;
        composition_count += 1;
    }
    if composition_count < input_count {
        input.truncate(composition_count);
    }
}

/// `normalize`: NFC.
fn normalize(input: &mut Vec<u32>) {
    if (0..input.len()).any(|index| decomposition_length(input[index]) != 0) {
        decompose(input);
    }
    sort_marks(input);
    compose(input);
}

const BASE: i32 = 36;
const TMIN: i32 = 1;
const TMAX: i32 = 26;
const SKEW: i32 = 38;
const DAMP: i32 = 700;
const INITIAL_BIAS: i32 = 72;
const INITIAL_N: u32 = 128;

fn digit_value(byte: u8) -> i32 {
    match byte {
        b'a'..=b'z' => i32::from(byte - b'a'),
        b'0'..=b'9' => i32::from(byte - b'0') + 26,
        _ => -1,
    }
}

fn digit_to_char(digit: i32) -> u8 {
    if digit < 26 {
        (digit + 97) as u8
    } else {
        (digit + 22) as u8
    }
}

fn adapt(mut delta: i32, count: i32, first_time: bool) -> i32 {
    delta = if first_time { delta / DAMP } else { delta / 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

/// `punycode_to_utf32`; `false` on any decoding error.
fn punycode_to_utf32(input: &[u8], out: &mut Vec<u32>) -> bool {
    let mut written_out: i32 = 0;
    let mut n: u32 = INITIAL_N;
    let mut i: i32 = 0;
    let mut bias: i32 = INITIAL_BIAS;
    let mut rest = input;
    if let Some(end_of_ascii) = rest.iter().rposition(|byte| *byte == b'-') {
        for &byte in &rest[..end_of_ascii] {
            if byte >= 0x80 {
                return false;
            }
            out.push(u32::from(byte));
            written_out += 1;
        }
        rest = &rest[end_of_ascii + 1..];
    }
    while !rest.is_empty() {
        let oldi = i;
        let mut w: i32 = 1;
        let mut k = BASE;
        loop {
            let Some((&code_point, tail)) = rest.split_first() else {
                return false;
            };
            rest = tail;
            let digit = digit_value(code_point);
            if digit < 0 {
                return false;
            }
            if digit > (0x7fff_ffff - i) / w {
                return false;
            }
            i += digit * w;
            let t = if k <= bias {
                TMIN
            } else if k >= bias + TMAX {
                TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            if w > 0x7fff_ffff / (BASE - t) {
                return false;
            }
            w *= BASE - t;
            k += BASE;
        }
        bias = adapt(i - oldi, written_out + 1, oldi == 0);
        // `i / (written_out + 1) > int32_t(0x7fffffff - n)`, `n` unsigned.
        if i / (written_out + 1) > 0x7fff_ffff_u32.wrapping_sub(n) as i32 {
            return false;
        }
        n = n.wrapping_add((i / (written_out + 1)) as u32);
        i %= written_out + 1;
        if n < 0x80 {
            return false;
        }
        out.insert(i as usize, n);
        written_out += 1;
        i += 1;
    }
    true
}

/// `utf32_to_punycode`; `false` on any encoding error.
fn utf32_to_punycode(input: &[u32], out: &mut Vec<u8>) -> bool {
    let mut n: u32 = INITIAL_N;
    let mut d: i32 = 0;
    let mut bias: i32 = INITIAL_BIAS;
    let mut h: usize = 0;
    for &c in input {
        if c < 0x80 {
            h += 1;
            out.push(c as u8);
        }
        if c > 0x10_ffff || (0xd880..0xe000).contains(&c) {
            return false;
        }
    }
    let b = h;
    if b > 0 {
        out.push(b'-');
    }
    while h < input.len() {
        let mut m: u32 = 0x10_FFFF;
        for &code_point in input {
            if code_point >= n && code_point < m {
                m = code_point;
            }
        }
        if u64::from(m.wrapping_sub(n)) > (0x7fff_ffff_u64 - d as u64) / (h as u64 + 1) {
            return false;
        }
        d = d.wrapping_add((u64::from(m.wrapping_sub(n)).wrapping_mul(h as u64 + 1)) as i32);
        n = m;
        for &c in input {
            if c < n {
                if d == 0x7fff_ffff {
                    return false;
                }
                d += 1;
            }
            if c == n {
                let mut q = d;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit_to_char(t + ((q - t) % (BASE - t))));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit_to_char(q));
                bias = adapt(d, (h + 1) as i32, h == b);
                d = 0;
                h += 1;
            }
        }
        d += 1;
        n += 1;
    }
    true
}

/// Bidi classes in the order of ada's `direction` enum.
mod class {
    pub const BN: u32 = 1;
    pub const CS: u32 = 2;
    pub const ES: u32 = 3;
    pub const ON: u32 = 4;
    pub const EN: u32 = 5;
    pub const L: u32 = 6;
    pub const R: u32 = 7;
    pub const NSM: u32 = 8;
    pub const AL: u32 = 9;
    pub const AN: u32 = 10;
    pub const ET: u32 = 11;
}

/// `find_direction`; class 0 (`NONE`) for a code point outside the table.
fn find_direction(unit: u32) -> u32 {
    let count = DIRECTIONS.len() / 3;
    let index = (0..count)
        .collect::<Vec<_>>()
        .partition_point(|row| DIRECTIONS[row * 3 + 1] < unit);
    if index == count {
        return 0;
    }
    if unit >= DIRECTIONS[index * 3] {
        return DIRECTIONS[index * 3 + 2];
    }
    0
}

fn find_last_not_of_nsm(label: &[u32]) -> Option<usize> {
    (0..label.len())
        .rev()
        .find(|index| find_direction(label[*index]) != class::NSM)
}

fn is_rtl_label(label: &[u32]) -> bool {
    let mask: u32 = (1 << class::R) | (1 << class::AL) | (1 << class::AN);
    let mut directions: u32 = 0;
    for &unit in label {
        directions |= 1 << find_direction(unit);
    }
    directions & mask != 0
}

fn contains(table: &[u32], unit: u32) -> bool {
    table.binary_search(&unit).is_ok()
}

/// `is_label_valid`: the label checks ada runs (`ContextJ` and Bidi on).
fn is_label_valid(label: &[u32]) -> bool {
    if label.is_empty() {
        return true;
    }
    // The label must not begin with a combining mark.
    if contains(&COMBINING_MARKS, label[0]) {
        return false;
    }
    for (index, &unit) in label.iter().enumerate() {
        if unit == 0x200c {
            if index > 0 && contains(&VIRAMA, label[index - 1]) {
                return true;
            }
            if index == 0 || index + 1 >= label.len() {
                return false;
            }
            let is_l_or_d = |code: &u32| contains(&JOINING_L, *code) || contains(&JOINING_D, *code);
            let is_r_or_d = |code: &u32| contains(&JOINING_R, *code) || contains(&JOINING_D, *code);
            return label[..index].iter().any(is_l_or_d)
                && label[index + 1..].iter().any(is_r_or_d);
        } else if unit == 0x200d {
            return index > 0 && contains(&VIRAMA, label[index - 1]);
        }
    }
    let Some(last_non_nsm) = find_last_not_of_nsm(label) else {
        return false;
    };
    if is_rtl_label(label) {
        if find_direction(label[0]) == class::L {
            // Evaluated as LTR; the loop stops short of the last non-NSM
            // character, as in ada.
            for &unit in &label[..last_non_nsm] {
                let direction = find_direction(unit);
                let allowed = [
                    class::L,
                    class::EN,
                    class::ES,
                    class::CS,
                    class::ET,
                    class::ON,
                    class::BN,
                    class::NSM,
                ];
                if !allowed.contains(&direction) {
                    return false;
                }
            }
            return true;
        }
        let mut has_an = false;
        let mut has_en = false;
        for (index, &unit) in label.iter().enumerate().take(last_non_nsm + 1) {
            let direction = find_direction(unit);
            if direction == class::EN {
                has_en = true;
                if has_an {
                    return false;
                }
            }
            if direction == class::AN {
                has_an = true;
                if has_en {
                    return false;
                }
            }
            let allowed = [
                class::R,
                class::AL,
                class::AN,
                class::EN,
                class::ES,
                class::CS,
                class::ET,
                class::ON,
                class::BN,
                class::NSM,
            ];
            if !allowed.contains(&direction) {
                return false;
            }
            if index == last_non_nsm
                && ![class::R, class::AL, class::AN, class::EN].contains(&direction)
            {
                return false;
            }
        }
        return true;
    }
    true
}
