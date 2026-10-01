//! Escape sequence parameters.
//!
//! Ported from xterm.js 6.0.0 `src/common/parser/Params.ts`.
//! Copyright (c) 2019 The xterm.js authors. MIT License.

/// Largest stored (sub) parameter; larger values are clamped to it.
const MAX_VALUE: i64 = 0x7FFF_FFFF;
/// Parameters and sub parameters stored per sequence (xterm's defaults).
const MAX_LENGTH: usize = 32;

/// One entry of [`Params::to_array`]: a parameter, or the sub parameters of
/// the parameter before it (`1;2:3:4` is `[1, 2, [3, 4]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    /// A parameter value; an empty parameter is 0.
    Value(i32),
    /// Sub parameters; an empty sub parameter is -1.
    Sub(Vec<i32>),
}

/// Accumulated parameters of the sequence being parsed.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    values: [i32; MAX_LENGTH],
    length: usize,
    sub_values: [i32; MAX_LENGTH],
    sub_length: usize,
    sub_index: [u16; MAX_LENGTH],
    reject_digits: bool,
    reject_sub_digits: bool,
    digit_is_sub: bool,
}

impl Params {
    pub(crate) fn new() -> Self {
        Self {
            values: [0; MAX_LENGTH],
            length: 0,
            sub_values: [0; MAX_LENGTH],
            sub_length: 0,
            sub_index: [0; MAX_LENGTH],
            reject_digits: false,
            reject_sub_digits: false,
            digit_is_sub: false,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    /// `params.params[index]`; like the typed array, a slot past `len` keeps
    /// whatever an earlier sequence stored there.
    pub(crate) fn get(&self, index: usize) -> i32 {
        self.values[index]
    }

    pub(crate) fn to_array(&self) -> Vec<Param> {
        let mut result = Vec::with_capacity(self.length);
        for index in 0..self.length {
            result.push(Param::Value(self.values[index]));
            let sub = self.sub_params(index);
            if !sub.is_empty() {
                result.push(Param::Sub(sub.to_vec()));
            }
        }
        result
    }

    pub(crate) fn reset(&mut self) {
        self.length = 0;
        self.sub_length = 0;
        self.reject_digits = false;
        self.reject_sub_digits = false;
        self.digit_is_sub = false;
    }

    /// Adds a parameter; values never fall below -1 here.
    pub(crate) fn add_param(&mut self, value: i32) {
        self.digit_is_sub = false;
        if self.length >= MAX_LENGTH {
            self.reject_digits = true;
            return;
        }
        let offset = u16::try_from(self.sub_length).unwrap_or(u16::MAX);
        self.sub_index[self.length] = (offset << 8) | offset;
        self.values[self.length] = value;
        self.length += 1;
    }

    pub(crate) fn add_sub_param(&mut self, value: i32) {
        self.digit_is_sub = true;
        if self.length == 0 {
            return;
        }
        if self.reject_digits || self.sub_length >= MAX_LENGTH {
            self.reject_sub_digits = true;
            return;
        }
        self.sub_values[self.sub_length] = value;
        self.sub_length += 1;
        self.sub_index[self.length - 1] = self.sub_index[self.length - 1].wrapping_add(1);
    }

    pub(crate) fn has_sub_params(&self, index: usize) -> bool {
        !self.sub_params(index).is_empty()
    }

    pub(crate) fn sub_params(&self, index: usize) -> &[i32] {
        let start = usize::from(self.sub_index[index] >> 8);
        let end = usize::from(self.sub_index[index] & 0xFF);
        if end > start {
            &self.sub_values[start..end]
        } else {
            &[]
        }
    }

    pub(crate) fn add_digit(&mut self, value: i32) {
        let length = if self.digit_is_sub {
            self.sub_length
        } else {
            self.length
        };
        if self.reject_digits || length == 0 || (self.digit_is_sub && self.reject_sub_digits) {
            return;
        }
        let store = if self.digit_is_sub {
            &mut self.sub_values
        } else {
            &mut self.values
        };
        let current = store[length - 1];
        store[length - 1] = if current == -1 {
            value
        } else {
            let next = (i64::from(current) * 10 + i64::from(value)).min(MAX_VALUE);
            i32::try_from(next).unwrap_or(i32::MAX)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{Param, Params};

    fn feed(sequence: &str) -> Params {
        let mut params = Params::new();
        params.add_param(0);
        for byte in sequence.bytes() {
            match byte {
                b';' => params.add_param(0),
                b':' => params.add_sub_param(-1),
                _ => params.add_digit(i32::from(byte - b'0')),
            }
        }
        params
    }

    #[test]
    fn collects_params_and_sub_params() {
        assert_eq!(
            feed("1;2:3:4;5::6").to_array(),
            [
                Param::Value(1),
                Param::Value(2),
                Param::Sub(vec![3, 4]),
                Param::Value(5),
                Param::Sub(vec![-1, 6])
            ]
        );
        assert_eq!(feed("").to_array(), [Param::Value(0)]);
    }

    #[test]
    fn clamps_values_and_drops_params_past_the_limit() {
        assert_eq!(feed("99999999999").to_array(), [Param::Value(i32::MAX)]);
        let many = feed(&"1;".repeat(40));
        assert_eq!(many.len(), 32);
    }
}
