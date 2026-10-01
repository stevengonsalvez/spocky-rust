//! JSON numbers with JavaScript `Number` semantics.
//!
//! Paseo parses frames with `JSON.parse`, validates them with zod 4, and
//! writes them with `JSON.stringify`. Every JSON number is therefore an IEEE
//! 754 double, `z.number().int()` accepts only safe integers, and output uses
//! `Number.prototype.toString` formatting (`1e+21`, `0.000001`, `-0` as `0`).

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::ser::{Error as _, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// `Number.MAX_SAFE_INTEGER`, the bound zod 4 applies to `.int()`.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Formats a finite double exactly as JavaScript `String(value)` does.
#[must_use]
pub fn format_js_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    ryu_js::Buffer::new().format_finite(value).to_owned()
}

fn serialize_js_number<S: Serializer>(value: f64, serializer: S) -> Result<S::Ok, S::Error> {
    if !value.is_finite() {
        return Err(S::Error::custom("JSON numbers must be finite"));
    }
    let raw = RawValue::from_string(format_js_number(value)).map_err(S::Error::custom)?;
    raw.serialize(serializer)
}

struct F64Visitor;

impl Visitor<'_> for F64Visitor {
    type Value = f64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a number")
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<f64, E> {
        // Round to nearest double, as JSON.parse does for the same literal.
        Ok(value as f64)
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<f64, E> {
        Ok(value as f64)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<f64, E> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(E::custom("expected a finite number"))
        }
    }
}

fn deserialize_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    deserializer.deserialize_f64(F64Visitor)
}

/// A finite `z.number()` value, written with JavaScript formatting.
///
/// Exact formatting holds for `serde_json::to_string` and `to_writer`, which
/// emit the raw number text unchanged.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct JsNumber(f64);

impl JsNumber {
    /// Returns `None` for NaN and infinities, which JSON cannot carry.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self(value))
    }

    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for JsNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_js_number(self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for JsNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_f64(deserializer).map(Self)
    }
}

/// A `z.number().int()` value refined to `MIN..=MAX`.
///
/// `.int()` alone is the safe-integer range, `.positive()` raises the floor
/// to 1, `.nonnegative()` to 0, and `.max(n)` lowers the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedInt<const MIN: i64, const MAX: i64>(i64);

/// `z.number().int()`.
pub type Int = BoundedInt<{ -MAX_SAFE_INTEGER }, MAX_SAFE_INTEGER>;
/// `z.number().int().positive()`.
pub type PositiveInt = BoundedInt<1, MAX_SAFE_INTEGER>;
/// `z.number().int().nonnegative()`.
pub type NonNegativeInt = BoundedInt<0, MAX_SAFE_INTEGER>;
/// `z.number().int().positive().max(200)`, the directory page limit.
pub type PageLimit = BoundedInt<1, 200>;

impl<const MIN: i64, const MAX: i64> BoundedInt<MIN, MAX> {
    /// Returns `None` when `value` is outside `MIN..=MAX`.
    #[must_use]
    pub fn new(value: i64) -> Option<Self> {
        (MIN..=MAX).contains(&value).then_some(Self(value))
    }

    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

impl<const MIN: i64, const MAX: i64> Serialize for BoundedInt<MIN, MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.0)
    }
}

impl<'de, const MIN: i64, const MAX: i64> Deserialize<'de> for BoundedInt<MIN, MAX> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = deserialize_f64(deserializer)?;
        // Every bound is a safe integer, so the double comparison is exact.
        if value.fract() != 0.0 || value < MIN as f64 || value > MAX as f64 {
            return Err(de::Error::custom(format_args!(
                "expected an integer in {MIN}..={MAX}, received {}",
                format_js_number(value)
            )));
        }
        Ok(Self(value as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::{JsNumber, NonNegativeInt, PageLimit, PositiveInt, format_js_number};

    #[test]
    fn formats_like_number_to_string() {
        let cases = [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (1.5, "1.5"),
            (0.000_001, "0.000001"),
            (0.000_000_1, "1e-7"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (-1.25e300, "-1.25e+300"),
            (9_007_199_254_740_993.0, "9007199254740992"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_js_number(value), expected, "{value:?}");
        }
    }

    #[test]
    fn number_roundtrips_through_js_text() {
        for (input, output) in [
            ("1.0", "1"),
            ("-0", "0"),
            ("1e3", "1000"),
            ("2.5E-7", "2.5e-7"),
        ] {
            let parsed: JsNumber = serde_json::from_str(input).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), output, "{input}");
        }
        assert!(serde_json::from_str::<JsNumber>("1e400").is_err());
        assert!(serde_json::from_str::<JsNumber>("\"1\"").is_err());
    }

    #[test]
    fn bounded_ints_match_zod_refinements() {
        let positive =
            |text: &str| serde_json::from_str::<PositiveInt>(text).map(super::BoundedInt::get);
        assert_eq!(positive("1.0").unwrap(), 1);
        assert_eq!(positive("9007199254740991").unwrap(), 9_007_199_254_740_991);
        assert!(positive("9007199254740992").is_err());
        assert!(positive("0").is_err());
        assert!(positive("-0").is_err());
        assert!(positive("1.5").is_err());
        assert_eq!(
            serde_json::from_str::<NonNegativeInt>("-0").unwrap().get(),
            0
        );
        assert!(serde_json::from_str::<PageLimit>("200").is_ok());
        assert!(serde_json::from_str::<PageLimit>("201").is_err());
    }
}
