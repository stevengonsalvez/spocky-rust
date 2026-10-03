//! `String.prototype.toLocaleLowerCase` as node v22.20.0 runs it (V8 over
//! ICU 77.1, Unicode 16.0), and the process's default locale.
//!
//! The root mapping is [`crate::text::js_to_lowercase`]. The locale tailoring
//! ICU applies when lowercasing is `SpecialCasing.txt`'s: Turkish and
//! Azerbaijani map `U+0130` to `i`, `I` to dotless `U+0131` (to `i`, and drop
//! the following `U+0307`, when a dot above follows), and Lithuanian keeps the
//! dot on `i`, `j`, and `U+012E` under accents. Any other language is the
//! root mapping.
//!
//! A locale argument is validated as V8 does: a `unicode_language_id` prefix
//! (language, script, region, variants) that fails is "Incorrect locale
//! information provided"; a bad extension, private-use part, or a duplicate
//! variant is "Invalid language tag: <tag>". The three-letter codes ICU
//! aliases to `tr`, `az`, and `lt` (`tur`, `aze`, `azj`, `lit`) select the
//! same rules.

use std::fmt;

use crate::js_locale_tables::COMBINING_CLASS_RANGES;
use crate::js_value::js_text_canonical_cow;
use crate::text::lowercase_default;

/// The `RangeError` V8 throws for an unusable locale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleError {
    /// `error.message`.
    pub message: String,
}

impl fmt::Display for LocaleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RangeError: {}", self.message)
    }
}

impl std::error::Error for LocaleError {}

fn bad_locale() -> LocaleError {
    LocaleError {
        message: "Incorrect locale information provided".to_owned(),
    }
}

fn invalid_tag(tag: &str) -> LocaleError {
    LocaleError {
        message: format!("Invalid language tag: {}", tag.to_ascii_lowercase()),
    }
}

/// The lowercase tailoring a language selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tailoring {
    Root,
    Turkic,
    Lithuanian,
}

fn tailoring_of(language: &str) -> Tailoring {
    match language.to_ascii_lowercase().as_str() {
        "tr" | "tur" | "az" | "aze" | "azj" => Tailoring::Turkic,
        "lt" | "lit" => Tailoring::Lithuanian,
        _ => Tailoring::Root,
    }
}

fn is_alpha(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn is_alnum(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn is_language(part: &str) -> bool {
    is_alpha(part) && matches!(part.len(), 2 | 3 | 5..=8)
}

fn is_script(part: &str) -> bool {
    part.len() == 4 && is_alpha(part)
}

fn is_region(part: &str) -> bool {
    (part.len() == 2 && is_alpha(part))
        || (part.len() == 3 && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_variant(part: &str) -> bool {
    is_alnum(part)
        && ((5..=8).contains(&part.len())
            || (part.len() == 4 && part.as_bytes()[0].is_ascii_digit()))
}

/// The extension and private-use part of a tag (everything from the first
/// singleton), per UTS 35's `unicode_locale_id` as ICU's strict parser reads it.
fn extensions_valid(parts: &[&str]) -> bool {
    let mut seen: Vec<char> = Vec::new();
    let mut index = 0;
    while index < parts.len() {
        let singleton = parts[index];
        if singleton.len() != 1 || !is_alnum(singleton) {
            return false;
        }
        let letter = singleton.as_bytes()[0].to_ascii_lowercase() as char;
        if seen.contains(&letter) {
            return false;
        }
        seen.push(letter);
        index += 1;
        let start = index;
        if letter == 'x' {
            let rest = &parts[start..];
            return !rest.is_empty() && rest.iter().all(|part| is_alnum(part) && part.len() <= 8);
        }
        while index < parts.len() && parts[index].len() != 1 {
            index += 1;
        }
        let subtags = &parts[start..index];
        if subtags.is_empty()
            || !subtags
                .iter()
                .all(|part| is_alnum(part) && (2..=8).contains(&part.len()))
        {
            return false;
        }
        let valid = match letter {
            'u' => unicode_extension_valid(subtags),
            't' => transformed_extension_valid(subtags),
            _ => true,
        };
        if !valid {
            return false;
        }
    }
    true
}

/// `-u-` subtags: attributes (3-8), then keys (2: alphanumeric, alpha) each
/// followed by types (3-8).
fn unicode_extension_valid(subtags: &[&str]) -> bool {
    let mut in_keywords = false;
    for part in subtags {
        if part.len() == 2 {
            let bytes = part.as_bytes();
            if !bytes[1].is_ascii_alphabetic() {
                return false;
            }
            in_keywords = true;
        } else if !in_keywords && part.len() < 3 {
            return false;
        }
    }
    true
}

/// `-t-` subtags: an optional language id, then `tkey tvalue+` fields.
fn transformed_extension_valid(subtags: &[&str]) -> bool {
    let mut index = 0;
    if is_language(subtags[0]) {
        index = 1;
        if index < subtags.len() && is_script(subtags[index]) {
            index += 1;
        }
        if index < subtags.len() && is_region(subtags[index]) {
            index += 1;
        }
        while index < subtags.len() && is_variant(subtags[index]) {
            index += 1;
        }
    }
    let is_key = |part: &str| {
        part.len() == 2
            && part.as_bytes()[0].is_ascii_alphabetic()
            && part.as_bytes()[1].is_ascii_digit()
    };
    while index < subtags.len() {
        if !is_key(subtags[index]) {
            return false;
        }
        index += 1;
        let values_start = index;
        while index < subtags.len() && !is_key(subtags[index]) {
            if !(3..=8).contains(&subtags[index].len()) {
                return false;
            }
            index += 1;
        }
        if index == values_start {
            return false;
        }
    }
    true
}

/// The language subtag of a valid tag, or V8's error.
fn language_of_tag(tag: &str) -> Result<String, LocaleError> {
    if tag.is_empty() || !tag.is_ascii() {
        return Err(bad_locale());
    }
    let parts: Vec<&str> = tag.split('-').collect();
    if !is_language(parts[0]) {
        return Err(bad_locale());
    }
    let mut index = 1;
    let mut stage = 0; // 0 after language, 1 after script, 2 after region, 3 in variants
    let mut variants: Vec<String> = Vec::new();
    while index < parts.len() {
        let part = parts[index];
        if part.is_empty() {
            return Err(if index == parts.len() - 1 {
                invalid_tag(tag)
            } else {
                bad_locale()
            });
        }
        if part.len() == 1 && is_alnum(part) {
            break;
        }
        if stage == 0 && is_script(part) {
            stage = 1;
        } else if stage <= 1 && is_region(part) {
            stage = 2;
        } else if is_variant(part) {
            let lowered = part.to_ascii_lowercase();
            if variants.contains(&lowered) {
                return Err(invalid_tag(tag));
            }
            variants.push(lowered);
            stage = 3;
        } else {
            return Err(bad_locale());
        }
        index += 1;
    }
    if index < parts.len() && !extensions_valid(&parts[index..]) {
        return Err(invalid_tag(tag));
    }
    Ok(parts[0].to_ascii_lowercase())
}

/// The process's default locale as a language tag, as node and ICU resolve it:
/// `LC_ALL`, then `LC_MESSAGES`, then `LANG`; `C`, `POSIX`, or none set is
/// `en-US`; the codeset after `.` is dropped.
#[must_use]
pub fn default_locale() -> String {
    default_locale_from(|name| std::env::var(name).ok())
}

/// [`default_locale`] over an environment lookup.
#[must_use]
pub fn default_locale_from(lookup: impl Fn(&str) -> Option<String>) -> String {
    let id = lookup("LC_ALL")
        .or_else(|| lookup("LC_MESSAGES"))
        .or_else(|| lookup("LANG"));
    let Some(id) = id.filter(|id| id != "C" && id != "POSIX") else {
        return "en-US".to_owned();
    };
    let (body, modifier) = match id.split_once('@') {
        Some((body, modifier)) => (body, Some(modifier)),
        None => (id.as_str(), None),
    };
    let body = body.split('.').next().unwrap_or("");
    if body == "C" || body == "POSIX" {
        return "en-US".to_owned();
    }
    let mut parts = body.split(['_', '-']).filter(|part| !part.is_empty());
    let mut tag = parts
        .next()
        .map_or_else(|| "und".to_owned(), str::to_ascii_lowercase);
    for part in parts {
        tag.push('-');
        if is_script(part) {
            let mut letters = part.to_ascii_lowercase();
            letters[..1].make_ascii_uppercase();
            tag.push_str(&letters);
        } else if is_region(part) {
            tag.push_str(&part.to_ascii_uppercase());
        } else {
            tag.push_str(&part.to_ascii_lowercase());
        }
    }
    if let Some(modifier) = modifier.filter(|modifier| !modifier.is_empty()) {
        tag.push_str("-x-lvariant-");
        tag.push_str(&modifier.to_ascii_lowercase());
    }
    tag
}

/// Canonical combining class (`0` for a code point not in the table).
fn combining_class(character: char) -> u32 {
    let code = u32::from(character);
    let count = COMBINING_CLASS_RANGES.len() / 3;
    let index = (0..count)
        .collect::<Vec<_>>()
        .partition_point(|row| COMBINING_CLASS_RANGES[row * 3 + 1] < code);
    if index < count && code >= COMBINING_CLASS_RANGES[index * 3] {
        COMBINING_CLASS_RANGES[index * 3 + 2]
    } else {
        0
    }
}

/// Whether a `U+0307` follows, with only marks of other classes between.
fn followed_by_dot_above(rest: &str) -> bool {
    for character in rest.chars() {
        if character == '\u{307}' {
            return true;
        }
        let class = combining_class(character);
        if class == 0 || class == 230 {
            return false;
        }
    }
    false
}

/// Whether an `I` precedes, with only marks of other classes between.
fn preceded_by_capital_i(before: &str) -> bool {
    for character in before.chars().rev() {
        if character == 'I' {
            return true;
        }
        let class = combining_class(character);
        if class == 0 || class == 230 {
            return false;
        }
    }
    false
}

/// Whether a combining mark of class 230 follows, with only marks of other
/// non-zero classes between.
fn followed_by_more_above(rest: &str) -> bool {
    for character in rest.chars() {
        let class = combining_class(character);
        if class == 230 {
            return true;
        }
        if class == 0 {
            return false;
        }
    }
    false
}

fn lower_with(value: &str, tailoring: Tailoring) -> String {
    let value = js_text_canonical_cow(value);
    let mut lowered = String::with_capacity(value.len());
    for (index, character) in value.char_indices() {
        let rest = &value[index + character.len_utf8()..];
        match (tailoring, character) {
            (Tailoring::Turkic, '\u{130}') => lowered.push('i'),
            (Tailoring::Turkic, 'I') => {
                lowered.push(if followed_by_dot_above(rest) {
                    'i'
                } else {
                    '\u{131}'
                });
            }
            (Tailoring::Turkic, '\u{307}') => {
                if !preceded_by_capital_i(&value[..index]) {
                    lowered.push(character);
                }
            }
            (Tailoring::Lithuanian, 'I' | 'J' | '\u{12e}') => {
                lowercase_default(&value, index, character, &mut lowered);
                if followed_by_more_above(rest) {
                    lowered.push('\u{307}');
                }
            }
            (Tailoring::Lithuanian, '\u{cc}') => lowered.push_str("i\u{307}\u{300}"),
            (Tailoring::Lithuanian, '\u{cd}') => lowered.push_str("i\u{307}\u{301}"),
            (Tailoring::Lithuanian, '\u{128}') => lowered.push_str("i\u{307}\u{303}"),
            _ => lowercase_default(&value, index, character, &mut lowered),
        }
    }
    lowered
}

/// `text.toLocaleLowerCase(locale)`; `None` is the default locale (an
/// `undefined` argument).
///
/// # Errors
///
/// The `RangeError` V8 throws for a locale that is not a valid language tag.
pub fn js_to_locale_lower_case(text: &str, locale: Option<&str>) -> Result<String, LocaleError> {
    let language = match locale {
        Some(tag) => language_of_tag(tag)?,
        None => default_locale()
            .split('-')
            .next()
            .unwrap_or("und")
            .to_owned(),
    };
    Ok(lower_with(text, tailoring_of(&language)))
}
