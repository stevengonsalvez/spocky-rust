//! `JSON.parse` and `JSON.stringify` with JavaScript value semantics.
//!
//! `serde_json` rejects input that `JSON.parse` accepts: lone surrogate
//! escapes, numbers beyond the double range, and nesting deeper than 128.
//! Rejecting any of them made a whole registry load empty. This module
//! accepts exactly what `JSON.parse` accepts and writes exactly what
//! `JSON.stringify` writes.
//!
//! Strings are JavaScript text held in a Rust `String`. JavaScript strings
//! are UTF-16 and may hold lone surrogates, which a `String` cannot. Each
//! lone surrogate `u` is stored as [`JS_TEXT_ESCAPE`] followed by
//! `U+F0000 + (u - 0xD800)`, and a literal [`JS_TEXT_ESCAPE`] is stored
//! doubled. Text without lone surrogates or `U+10FFFF` is stored unchanged.
//! Use [`js_text`] to bring outside text into this form.
//!
//! Numbers are doubles. Overflowing literals become infinities, as in
//! JavaScript, and `JSON.stringify` writes non-finite numbers as `null`.

use std::fmt::{self, Display, Formatter, Write};

/// Escape prefix for lone surrogates in JavaScript text (a noncharacter).
pub const JS_TEXT_ESCAPE: char = '\u{10FFFF}';
const SURROGATE_BASE: u32 = 0xF0000;

/// Encodes outside text as JavaScript text (doubles any [`JS_TEXT_ESCAPE`]).
#[must_use]
pub fn js_text(text: &str) -> String {
    if !text.contains(JS_TEXT_ESCAPE) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 4);
    for character in text.chars() {
        push_char(&mut out, character);
    }
    out
}

fn push_char(out: &mut String, character: char) {
    if character == JS_TEXT_ESCAPE {
        out.push(JS_TEXT_ESCAPE);
    }
    out.push(character);
}

fn push_lone_surrogate(out: &mut String, unit: u16) {
    out.push(JS_TEXT_ESCAPE);
    let encoded = SURROGATE_BASE + u32::from(unit) - 0xD800;
    out.push(char::from_u32(encoded).unwrap_or(char::REPLACEMENT_CHARACTER));
}

/// One element of JavaScript text: a Unicode scalar or a lone surrogate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsTextUnit {
    Char(char),
    LoneSurrogate(u16),
}

/// Decodes JavaScript text into scalars and lone surrogates.
pub fn js_text_units(text: &str) -> impl Iterator<Item = JsTextUnit> + '_ {
    let mut chars = text.chars();
    std::iter::from_fn(move || {
        let character = chars.next()?;
        if character != JS_TEXT_ESCAPE {
            return Some(JsTextUnit::Char(character));
        }
        match chars.next() {
            Some(JS_TEXT_ESCAPE) | None => Some(JsTextUnit::Char(JS_TEXT_ESCAPE)),
            Some(encoded) => {
                let unit = u32::from(encoded) - SURROGATE_BASE + 0xD800;
                Some(JsTextUnit::LoneSurrogate(
                    u16::try_from(unit).unwrap_or(0xFFFD),
                ))
            }
        }
    })
}

/// UTF-16 code units of JavaScript text, as `String.prototype` indexes them.
pub fn js_text_utf16(text: &str) -> impl Iterator<Item = u16> + '_ {
    js_text_units(text).flat_map(|unit| {
        let mut buffer = [0_u16; 2];
        let units: Vec<u16> = match unit {
            JsTextUnit::Char(character) => character.encode_utf16(&mut buffer).to_vec(),
            JsTextUnit::LoneSurrogate(unit) => vec![unit],
        };
        units
    })
}

/// Builds JavaScript text from UTF-16 code units, keeping lone surrogates.
#[must_use]
pub fn js_text_from_utf16(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(character) => push_char(&mut out, character),
            Err(error) => push_lone_surrogate(&mut out, error.unpaired_surrogate()),
        }
    }
    out
}

/// A parsed JavaScript value.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsValue>),
    Object(JsObject),
}

/// An ordinary object: own data properties with JavaScript enumeration order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JsObject {
    entries: Vec<(String, JsValue)>,
}

impl JsObject {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `object[key] = value`: an existing key keeps its position.
    // ponytail: linear key lookup, O(n^2) for very wide objects; add an index if profiles show it.
    pub fn insert(&mut self, key: impl Into<String>, value: JsValue) {
        let key = key.into();
        match self
            .entries
            .iter_mut()
            .find(|(existing, _)| *existing == key)
        {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsValue> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Own keys in `OrdinaryOwnPropertyKeys` order: array indexes ascending,
    /// then the other keys in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &JsValue)> {
        let mut indexed: Vec<(u32, usize)> = Vec::new();
        let mut named: Vec<usize> = Vec::new();
        for (position, (key, _)) in self.entries.iter().enumerate() {
            match array_index(key) {
                Some(index) => indexed.push((index, position)),
                None => named.push(position),
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        indexed
            .into_iter()
            .map(|(_, position)| position)
            .chain(named)
            .map(|position| {
                let (key, value) = &self.entries[position];
                (key.as_str(), value)
            })
    }
}

/// An ECMAScript array index: canonical decimal below 2^32 - 1.
#[must_use]
pub fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

impl JsValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(number) => Some(*number),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_object(&self) -> Option<&JsObject> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[JsValue]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    fn take_children(&mut self, stack: &mut Vec<JsValue>) {
        match self {
            Self::Array(items) => stack.append(items),
            Self::Object(object) => {
                stack.extend(object.entries.drain(..).map(|(_, value)| value));
            }
            _ => {}
        }
    }
}

impl Drop for JsValue {
    /// Drops nested values iteratively so deep input cannot overflow the stack.
    fn drop(&mut self) {
        let mut stack = Vec::new();
        self.take_children(&mut stack);
        while let Some(mut value) = stack.pop() {
            value.take_children(&mut stack);
        }
    }
}

/// A `JSON.parse` `SyntaxError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSyntaxError {
    pub position: usize,
    pub message: &'static str,
}

impl Display for JsonSyntaxError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at position {}", self.message, self.position)
    }
}

impl std::error::Error for JsonSyntaxError {}

enum Frame {
    Array(Vec<JsValue>),
    Object(JsObject, String),
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    index: usize,
}

/// `JSON.parse(text)`.
///
/// # Errors
///
/// Returns a syntax error wherever `JSON.parse` throws one.
pub fn parse(text: &str) -> Result<JsValue, JsonSyntaxError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        index: 0,
    };
    let mut stack: Vec<Frame> = Vec::new();
    'value: loop {
        parser.skip_whitespace();
        let mut value = match parser.peek() {
            Some(b'{') => {
                parser.index += 1;
                parser.skip_whitespace();
                if parser.eat(b'}') {
                    JsValue::Object(JsObject::new())
                } else {
                    let key = parser.object_key()?;
                    stack.push(Frame::Object(JsObject::new(), key));
                    continue 'value;
                }
            }
            Some(b'[') => {
                parser.index += 1;
                parser.skip_whitespace();
                if parser.eat(b']') {
                    JsValue::Array(Vec::new())
                } else {
                    stack.push(Frame::Array(Vec::new()));
                    continue 'value;
                }
            }
            Some(b'"') => JsValue::String(parser.string()?),
            Some(b't') => parser.literal("true", JsValue::Bool(true))?,
            Some(b'f') => parser.literal("false", JsValue::Bool(false))?,
            Some(b'n') => parser.literal("null", JsValue::Null)?,
            Some(b'-' | b'0'..=b'9') => JsValue::Number(parser.number()?),
            _ => return Err(parser.error("Unexpected token")),
        };
        loop {
            parser.skip_whitespace();
            match stack.last_mut() {
                None => {
                    if parser.index != parser.bytes.len() {
                        return Err(parser.error("Unexpected non-whitespace character after JSON"));
                    }
                    return Ok(value);
                }
                Some(Frame::Array(items)) => {
                    items.push(value);
                    if parser.eat(b',') {
                        continue 'value;
                    }
                    if !parser.eat(b']') {
                        return Err(parser.error("Expected ',' or ']' after array element"));
                    }
                    let Some(Frame::Array(items)) = stack.pop() else {
                        unreachable!("top frame is an array");
                    };
                    value = JsValue::Array(items);
                }
                Some(Frame::Object(object, key)) => {
                    object.insert(std::mem::take(key), value);
                    if parser.eat(b',') {
                        parser.skip_whitespace();
                        *key = parser.object_key()?;
                        continue 'value;
                    }
                    if !parser.eat(b'}') {
                        return Err(parser.error("Expected ',' or '}' after property value"));
                    }
                    let Some(Frame::Object(object, _)) = stack.pop() else {
                        unreachable!("top frame is an object");
                    };
                    value = JsValue::Object(object);
                }
            }
        }
    }
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> JsonSyntaxError {
        JsonSyntaxError {
            position: self.index,
            message,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.index += 1;
        }
    }

    fn literal(&mut self, word: &str, value: JsValue) -> Result<JsValue, JsonSyntaxError> {
        if self.bytes[self.index..].starts_with(word.as_bytes()) {
            self.index += word.len();
            Ok(value)
        } else {
            Err(self.error("Unexpected token"))
        }
    }

    fn object_key(&mut self) -> Result<String, JsonSyntaxError> {
        if self.peek() != Some(b'"') {
            return Err(self.error("Expected property name"));
        }
        let key = self.string()?;
        self.skip_whitespace();
        if !self.eat(b':') {
            return Err(self.error("Expected ':' after property name"));
        }
        Ok(key)
    }

    fn digits(&mut self) -> usize {
        let start = self.index;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.index += 1;
        }
        self.index - start
    }

    fn number(&mut self) -> Result<f64, JsonSyntaxError> {
        let start = self.index;
        self.eat(b'-');
        if self.eat(b'0') {
            if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(self.error("Unexpected number"));
            }
        } else if self.digits() == 0 {
            return Err(self.error("No number after minus sign"));
        }
        if self.eat(b'.') && self.digits() == 0 {
            return Err(self.error("Unterminated fractional number"));
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if self.digits() == 0 {
                return Err(self.error("Exponent part is missing a number"));
            }
        }
        // Rust parses decimal literals with correct rounding and saturates to
        // infinity on overflow, the same result JavaScript produces.
        self.text[start..self.index]
            .parse::<f64>()
            .map_err(|_| self.error("Invalid number"))
    }

    fn hex4(&mut self) -> Result<u16, JsonSyntaxError> {
        let slice = self
            .bytes
            .get(self.index..self.index + 4)
            .ok_or_else(|| self.error("Bad Unicode escape"))?;
        let text = std::str::from_utf8(slice).map_err(|_| self.error("Bad Unicode escape"))?;
        if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(self.error("Bad Unicode escape"));
        }
        let unit = u16::from_str_radix(text, 16).map_err(|_| self.error("Bad Unicode escape"))?;
        self.index += 4;
        Ok(unit)
    }

    fn string(&mut self) -> Result<String, JsonSyntaxError> {
        self.index += 1;
        let mut out = String::new();
        let mut pending_high: Option<u16> = None;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("Unterminated string in JSON"));
            };
            if byte == b'\\' && self.bytes.get(self.index + 1) == Some(&b'u') {
                self.index += 2;
                let unit = self.hex4()?;
                match (pending_high.take(), unit) {
                    (Some(high), 0xDC00..=0xDFFF) => {
                        let combined = 0x10000
                            + ((u32::from(high) - 0xD800) << 10)
                            + (u32::from(unit) - 0xDC00);
                        push_char(
                            &mut out,
                            char::from_u32(combined).unwrap_or(char::REPLACEMENT_CHARACTER),
                        );
                    }
                    (high, _) => {
                        if let Some(high) = high {
                            push_lone_surrogate(&mut out, high);
                        }
                        match unit {
                            0xD800..=0xDBFF => pending_high = Some(unit),
                            0xDC00..=0xDFFF => push_lone_surrogate(&mut out, unit),
                            _ => push_char(
                                &mut out,
                                char::from_u32(u32::from(unit))
                                    .unwrap_or(char::REPLACEMENT_CHARACTER),
                            ),
                        }
                    }
                }
                continue;
            }
            if let Some(high) = pending_high.take() {
                push_lone_surrogate(&mut out, high);
            }
            match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let escaped = match self.bytes.get(self.index + 1) {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        _ => return Err(self.error("Bad escaped character")),
                    };
                    out.push(escaped);
                    self.index += 2;
                }
                0x00..=0x1F => return Err(self.error("Bad control character in string literal")),
                _ => {
                    let rest = &self.text[self.index..];
                    let character = rest.chars().next().unwrap_or(char::REPLACEMENT_CHARACTER);
                    push_char(&mut out, character);
                    self.index += character.len_utf8();
                }
            }
        }
    }
}

/// `Number.prototype.toString()` for a finite double; `null` otherwise, as
/// `JSON.stringify` writes it.
#[must_use]
pub fn js_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value < 0.0 {
        return format!("-{}", js_number(-value));
    }
    // `{:e}` writes the shortest round-trip digits as `d[.ddd]e<exp>`.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let count = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    let point = exponent + 1;
    if count <= point && point <= 21 {
        let zeros = usize::try_from(point - count).unwrap_or(0);
        return format!("{digits}{}", "0".repeat(zeros));
    }
    if 0 < point && point <= 21 {
        let split = usize::try_from(point).unwrap_or(0);
        return format!("{}.{}", &digits[..split], &digits[split..]);
    }
    if -6 < point && point <= 0 {
        let zeros = usize::try_from(-point).unwrap_or(0);
        return format!("0.{}{digits}", "0".repeat(zeros));
    }
    let sign = if point > 0 { '+' } else { '-' };
    let magnitude = (point - 1).abs();
    if digits.len() == 1 {
        format!("{digits}e{sign}{magnitude}")
    } else {
        format!("{}.{}e{sign}{magnitude}", &digits[..1], &digits[1..])
    }
}

fn write_string(out: &mut String, text: &str) {
    out.push('"');
    for unit in js_text_units(text) {
        match unit {
            JsTextUnit::LoneSurrogate(unit) => {
                let _ = write!(out, "\\u{unit:04x}");
            }
            JsTextUnit::Char(character) => match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{0}'..='\u{1f}' => {
                    let _ = write!(out, "\\u{:04x}", u32::from(character));
                }
                _ => out.push(character),
            },
        }
    }
    out.push('"');
}

fn write_value(out: &mut String, value: &JsValue, indent: Option<&str>, depth: usize) {
    match value {
        JsValue::Null => out.push_str("null"),
        JsValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        JsValue::Number(number) => out.push_str(&js_number(*number)),
        JsValue::String(text) => write_string(out, text),
        JsValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        JsValue::Object(object) => {
            if object.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (position, (key, item)) in object.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_string(out, key);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_value(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<&str>, depth: usize) {
    if let Some(indent) = indent {
        out.push('\n');
        for _ in 0..depth {
            out.push_str(indent);
        }
    }
}

// ponytail: the writer recurses per nesting level; V8 `JSON.stringify` throws
// a RangeError at its own stack limit, which this does not reproduce.

/// `JSON.stringify(value)`.
#[must_use]
pub fn stringify(value: &JsValue) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0);
    out
}

/// `JSON.stringify(value, null, 2)`.
#[must_use]
pub fn stringify_pretty(value: &JsValue) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some("  "), 0);
    out
}

#[cfg(test)]
mod tests {
    use super::{
        JsTextUnit, JsValue, js_number, js_text, js_text_from_utf16, js_text_units, js_text_utf16,
        parse, stringify, stringify_pretty,
    };

    // Expected strings below were printed by node v22 with
    // `JSON.stringify(JSON.parse(input))`.

    #[test]
    fn lone_surrogates_round_trip_like_node() {
        let parsed = parse(r#""\ud83d x \uDE00 \ud83d\ude00""#).expect("JSON.parse accepts");
        assert_eq!(stringify(&parsed), r#""\ud83d x \ude00 😀""#);
        let text = parsed.as_str().expect("string");
        let units: Vec<u16> = js_text_utf16(text).collect();
        assert_eq!(
            units,
            vec![0xD83D, 0x20, 0x78, 0x20, 0xDE00, 0x20, 0xD83D, 0xDE00]
        );
        assert_eq!(js_text_from_utf16(&units), text);
    }

    #[test]
    fn escape_character_is_preserved_literally() {
        let outside = js_text("a\u{10FFFF}b");
        assert_eq!(
            js_text_units(&outside).collect::<Vec<_>>(),
            vec![
                JsTextUnit::Char('a'),
                JsTextUnit::Char('\u{10FFFF}'),
                JsTextUnit::Char('b')
            ]
        );
        let parsed = parse("\"\\udbff\\udfff\"").expect("pair decodes to U+10FFFF");
        assert_eq!(stringify(&parsed), "\"\u{10FFFF}\"");
    }

    #[test]
    fn numbers_match_node() {
        let parsed = parse(
            "[1e400,-1e400,12345678901234567890,-0,0.1,1e21,1e-7,123e-20,5e-324,1.7976931348623157e308,100,1.5e300]",
        )
        .expect("JSON.parse accepts");
        assert_eq!(
            stringify(&parsed),
            "[null,null,12345678901234567000,0,0.1,1e+21,1e-7,1.23e-18,5e-324,1.7976931348623157e+308,100,1.5e+300]"
        );
        assert_eq!(js_number(123_456.789), "123456.789");
        assert_eq!(js_number(1e20), "100000000000000000000");
        assert_eq!(js_number(0.000_001), "0.000001");
        assert_eq!(js_number(-2.5e-7), "-2.5e-7");
    }

    #[test]
    fn deep_nesting_parses_and_drops() {
        let depth = 100_000;
        let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let parsed = parse(&text).expect("JSON.parse accepts deep nesting");
        assert!(matches!(parsed, JsValue::Array(_)));
        drop(parsed);
    }

    #[test]
    fn syntax_errors_match_node() {
        for accepted in [" 1", "1 ", "{\"a\":1,\"a\":2}", "\"\\/\"", "-0.5E+2"] {
            assert!(parse(accepted).is_ok(), "{accepted:?}");
        }
        for rejected in [
            " \u{a0}1",
            "",
            " ",
            "01",
            "1.",
            "-",
            "+1",
            "1e",
            "[1,]",
            "{\"a\":1,}",
            "\"\\x\"",
            "\"\t\"",
            "nul",
            "[] x",
            "\u{feff}[]",
        ] {
            assert!(parse(rejected).is_err(), "{rejected:?}");
        }
    }

    #[test]
    fn stringify_escapes_and_orders_like_node() {
        let parsed = parse(
            r#"{"b":"\u0000\u001f\u007f\u2028\b\f\n\r\t\"\\/","10":[],"2":{},"a":[1,{"x":null}],"b":true}"#,
        )
        .expect("JSON.parse accepts");
        assert_eq!(
            stringify(&parsed),
            "{\"2\":{},\"10\":[],\"b\":true,\"a\":[1,{\"x\":null}]}"
        );
        assert_eq!(
            stringify_pretty(&parsed),
            "{\n  \"2\": {},\n  \"10\": [],\n  \"b\": true,\n  \"a\": [\n    1,\n    {\n      \"x\": null\n    }\n  ]\n}"
        );
        let text = parse(r#""\u0000\u001f\u007f\u2028\b\f\n\r\t\"\\/""#).expect("string");
        assert_eq!(
            stringify(&text),
            "\"\\u0000\\u001f\u{7f}\u{2028}\\b\\f\\n\\r\\t\\\"\\\\/\""
        );
    }
}
