//! zod string refinements with JavaScript string semantics.
//!
//! zod measures `.min()` and `.max()` in UTF-16 code units (`String.length`)
//! and `.trim()` removes exactly the characters `String.prototype.trim`
//! removes. Both differ from Rust's `str::len` and `str::trim`.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

use crate::js_case_tables::{CASE_IGNORABLE, CASED, LOWER_MULTI, LOWER_SINGLE};
use crate::js_value::{JsTextUnit, js_text, js_text_canonical_cow, js_text_units};

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

/// `String.prototype.trimStart`.
#[must_use]
pub fn js_trim_start(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

/// `String.prototype.trimEnd`.
#[must_use]
pub fn js_trim_end(value: &str) -> &str {
    value.trim_end_matches(is_js_whitespace)
}

/// `String.prototype.toLowerCase` as node v22.20.0 runs it: the mappings and
/// the `Cased` and `Case_Ignorable` properties of its Unicode version (see
/// [`crate::js_case_tables`]), with `Final_Sigma` for `U+03A3`. The Rust
/// toolchain's own `str::to_lowercase` follows a newer Unicode. A lone
/// surrogate in the [`crate::js_value`] encoding is not cased, so it passes
/// through.
#[must_use]
pub fn js_to_lowercase(value: &str) -> String {
    let mut lowered = String::with_capacity(value.len());
    for (index, character) in value.char_indices() {
        if character == '\u{3a3}' {
            let before = cased_after_ignorables(value[..index].chars().rev());
            let after = cased_after_ignorables(value[index + character.len_utf8()..].chars());
            lowered.push(if before && !after {
                '\u{3c2}'
            } else {
                '\u{3c3}'
            });
            continue;
        }
        let code = u32::from(character);
        if let Ok(position) = LOWER_SINGLE.binary_search_by_key(&code, |entry| entry.0) {
            lowered.extend(char::from_u32(LOWER_SINGLE[position].1));
        } else if let Ok(position) = LOWER_MULTI.binary_search_by_key(&code, |entry| entry.0) {
            lowered.extend(
                LOWER_MULTI[position]
                    .1
                    .iter()
                    .filter_map(|unit| char::from_u32(*unit)),
            );
        } else {
            lowered.push(character);
        }
    }
    lowered
}

fn in_ranges(ranges: &[(u32, u32)], character: char) -> bool {
    let code = u32::from(character);
    ranges
        .binary_search_by(|&(low, high)| {
            if high < code {
                std::cmp::Ordering::Less
            } else if low > code {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Skips `Case_Ignorable` characters, then asks whether the next is `Cased`.
fn cased_after_ignorables(mut characters: impl Iterator<Item = char>) -> bool {
    characters
        .find(|character| !in_ranges(&CASE_IGNORABLE, *character))
        .is_some_and(|character| in_ranges(&CASED, character))
}

/// `String.prototype.length` of JavaScript text: UTF-16 code units, with a
/// lone surrogate counted once (see [`crate::js_value`]).
#[must_use]
pub fn js_length(value: &str) -> usize {
    js_text_units(value)
        .map(|unit| match unit {
            JsTextUnit::Char(character) => character.len_utf16(),
            JsTextUnit::LoneSurrogate(_) => 1,
        })
        .sum()
}

/// A wire string as JavaScript text in the [`crate::js_value`] encoding,
/// where a lone surrogate is held escaped behind `U+10FFFF`.
///
/// Strings parsed from frames are already JavaScript text. Rust text must
/// enter through [`JsText::new`], which escapes a literal `U+10FFFF` so it
/// cannot read as a lone surrogate. Contract values are written only by
/// [`crate::frame::frame_text`], which turns the encoding back into
/// `JSON.stringify` escapes; serializing them any other way writes the
/// internal encoding.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsText(String);

impl JsText {
    /// JavaScript text for Rust text.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(js_text(text))
    }

    /// Wraps text that is already JavaScript text, such as a parsed string
    /// or the result of a concatenation. It is stored in canonical form, so
    /// two texts with the same UTF-16 code units are equal and hash alike.
    #[must_use]
    pub fn from_js(text: String) -> Self {
        match js_text_canonical_cow(&text) {
            std::borrow::Cow::Borrowed(_) => Self(text),
            std::borrow::Cow::Owned(canonical) => Self(canonical),
        }
    }

    /// The JavaScript text, in the `js_value` encoding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<&str> for JsText {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl PartialEq<str> for JsText {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl Serialize for JsText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for JsText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from_js)
    }
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

/// `z.string().min(MIN).max(MAX)` without trimming, measured in UTF-16
/// code units.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedString<const MIN: usize, const MAX: usize>(String);

impl<const MIN: usize, const MAX: usize> BoundedString<MIN, MAX> {
    /// Returns `None` when the UTF-16 length is outside `MIN..=MAX`.
    #[must_use]
    pub fn new(value: String) -> Option<Self> {
        (MIN..=MAX)
            .contains(&js_length(&value))
            .then_some(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<const MIN: usize, const MAX: usize> Serialize for BoundedString<MIN, MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de, const MIN: usize, const MAX: usize> Deserialize<'de> for BoundedString<MIN, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).ok_or_else(|| {
            de::Error::custom(format_args!(
                "expected a string of {MIN}..={MAX} characters"
            ))
        })
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
    use super::{BoundedString, JsText, NonEmptyString, TrimmedString, js_length, js_trim};

    #[test]
    fn trim_matches_ecmascript_not_unicode_white_space() {
        assert_eq!(js_trim("\u{FEFF}\u{3000} a \u{2029}"), "a");
        // U+0085 is Unicode White_Space but not ECMAScript WhiteSpace.
        assert_eq!(js_trim("\u{0085}a"), "\u{0085}a");
        assert_eq!("\u{0085}a".trim(), "a");
    }

    #[test]
    fn rust_text_escapes_the_encoding_prefix() {
        let text = JsText::new("a\u{10FFFF}\u{F0000}");
        assert_eq!(js_length(text.as_str()), 5);
        let written = crate::frame::frame_text(&text).unwrap();
        assert_eq!(written, "\"a\u{10FFFF}\u{F0000}\"");
    }

    #[test]
    fn length_counts_utf16_code_units() {
        assert_eq!(js_length("a\u{1F600}"), 3);
        assert_eq!(js_length("\u{00E9}"), 1);
        // A lone surrogate and U+10FFFF as JSON.parse reads them.
        let lone = crate::js_value::parse(r#""\ud800""#).unwrap();
        assert_eq!(js_length(lone.as_str().unwrap()), 1);
        let max = crate::js_value::parse(r#""\udbff\udfff""#).unwrap();
        assert_eq!(js_length(max.as_str().unwrap()), 2);
    }

    #[test]
    fn refinements_follow_zod() {
        assert!(serde_json::from_str::<NonEmptyString>(r#""""#).is_err());
        assert!(serde_json::from_str::<NonEmptyString>(r#"" ""#).is_ok());
        assert!(serde_json::from_str::<BoundedString<1, 2>>(r#""\ud83d\ude00""#).is_ok());
        assert!(serde_json::from_str::<BoundedString<1, 2>>(r#""\ud83d\ude00a""#).is_err());
        assert!(serde_json::from_str::<BoundedString<1, 2>>(r#""""#).is_err());
        let parsed: TrimmedString<1, 3> = serde_json::from_str(r#""  ab  ""#).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""ab""#);
        assert!(serde_json::from_str::<TrimmedString<1, 3>>(r#""   ""#).is_err());
        assert!(serde_json::from_str::<TrimmedString<1, 3>>(r#""😀😀""#).is_err());
    }
}
