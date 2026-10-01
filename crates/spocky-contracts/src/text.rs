//! zod string refinements with JavaScript string semantics.
//!
//! zod measures `.min()` and `.max()` in UTF-16 code units (`String.length`)
//! and `.trim()` removes exactly the characters `String.prototype.trim`
//! removes. Both differ from Rust's `str::len` and `str::trim`.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

/// Returns `true` for the `WhiteSpace` and `LineTerminator` code points that
/// ECMAScript `String.prototype.trim` strips.
#[must_use]
pub fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
#[must_use]
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// `String.prototype.length`: UTF-16 code units.
#[must_use]
pub fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `z.string().min(1)`: a string with at least one UTF-16 code unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NonEmptyString(String);

impl NonEmptyString {
    /// Returns `None` for the empty string.
    #[must_use]
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl Serialize for NonEmptyString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?)
            .ok_or_else(|| de::Error::custom("expected a string with at least 1 character"))
    }
}

/// `z.string().trim().min(MIN).max(MAX)`: zod trims first, then checks the
/// trimmed UTF-16 length, and outputs the trimmed value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrimmedString<const MIN: usize, const MAX: usize>(String);

impl<const MIN: usize, const MAX: usize> TrimmedString<MIN, MAX> {
    /// Trims `value` and returns `None` when the trimmed length is outside
    /// `MIN..=MAX`.
    #[must_use]
    pub fn new(value: &str) -> Option<Self> {
        let trimmed = js_trim(value);
        (MIN..=MAX)
            .contains(&js_length(trimmed))
            .then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MIN: usize, const MAX: usize> Serialize for TrimmedString<MIN, MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de, const MIN: usize, const MAX: usize> Deserialize<'de> for TrimmedString<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).ok_or_else(|| {
            de::Error::custom(format_args!(
                "expected a trimmed string of {MIN}..={MAX} characters"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{NonEmptyString, TrimmedString, js_length, js_trim};

    #[test]
    fn trim_matches_ecmascript_not_unicode_white_space() {
        assert_eq!(js_trim("\u{FEFF}\u{3000} a \u{2029}"), "a");
        // U+0085 is Unicode White_Space but not ECMAScript WhiteSpace.
        assert_eq!(js_trim("\u{0085}a"), "\u{0085}a");
        assert_eq!("\u{0085}a".trim(), "a");
    }

    #[test]
    fn length_counts_utf16_code_units() {
        assert_eq!(js_length("a\u{1F600}"), 3);
        assert_eq!(js_length("\u{00E9}"), 1);
    }

    #[test]
    fn refinements_follow_zod() {
        assert!(serde_json::from_str::<NonEmptyString>(r#""""#).is_err());
        assert!(serde_json::from_str::<NonEmptyString>(r#"" ""#).is_ok());
        let parsed: TrimmedString<1, 3> = serde_json::from_str(r#""  ab  ""#).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""ab""#);
        assert!(serde_json::from_str::<TrimmedString<1, 3>>(r#""   ""#).is_err());
        assert!(serde_json::from_str::<TrimmedString<1, 3>>(r#""😀😀""#).is_err());
    }
}
