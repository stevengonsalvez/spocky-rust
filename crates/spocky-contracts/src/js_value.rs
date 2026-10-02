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

/// Decodes JavaScript text into scalars and lone surrogates. A
/// [`JS_TEXT_ESCAPE`] that is not followed by a doubled escape or an encoded
/// surrogate (raw text that never went through [`js_text`]) stands for itself.
pub fn js_text_units(text: &str) -> impl Iterator<Item = JsTextUnit> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        let character = chars.next()?;
        if character != JS_TEXT_ESCAPE {
            return Some(JsTextUnit::Char(character));
        }
        match chars.peek().copied() {
            Some(JS_TEXT_ESCAPE) => {
                chars.next();
                Some(JsTextUnit::Char(JS_TEXT_ESCAPE))
            }
            Some(encoded) => match encoded_surrogate(encoded) {
                Some(unit) => {
                    chars.next();
                    Some(JsTextUnit::LoneSurrogate(unit))
                }
                None => Some(JsTextUnit::Char(JS_TEXT_ESCAPE)),
            },
            None => Some(JsTextUnit::Char(JS_TEXT_ESCAPE)),
        }
    })
}

/// The lone surrogate an escape payload encodes, if it is one.
fn encoded_surrogate(encoded: char) -> Option<u16> {
    let offset = u32::from(encoded).checked_sub(SURROGATE_BASE)?;
    if offset > 0x7FF {
        return None;
    }
    u16::try_from(0xD800 + offset).ok()
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

/// A parsed JavaScript value. `Clone`, `PartialEq`, `Debug`, and `Drop` walk
/// the tree with explicit stacks, so input as deep as `JSON.parse` accepts
/// never overflows the Rust stack.
pub enum JsValue {
    /// JavaScript `undefined`. `JSON.parse` never produces it; it models an
    /// own property whose value is `undefined` (a spread or assignment of a
    /// missing value), which keeps its key slot but `JSON.stringify` omits.
    /// In an array or at the top level it is written as `null`.
    Undefined,
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

    /// `Object.keys(object).length`: own properties holding `undefined`
    /// count, though `JSON.stringify` omits them.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `Object.keys(object).length === 0`; see [`Self::len`].
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

    #[must_use]
    pub const fn is_string(&self) -> bool {
        matches!(self, Self::String(_))
    }

    #[must_use]
    pub const fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// `value[key]` for an object; `None` for a missing key or a non-object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&JsValue> {
        self.as_object().and_then(|object| object.get(key))
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

enum CloneWork<'a> {
    Visit(&'a JsValue),
    Array(usize),
    Object(Vec<String>),
}

impl Clone for JsValue {
    fn clone(&self) -> Self {
        let mut work = vec![CloneWork::Visit(self)];
        let mut built: Vec<JsValue> = Vec::new();
        while let Some(step) = work.pop() {
            match step {
                CloneWork::Visit(value) => match value {
                    Self::Undefined => built.push(Self::Undefined),
                    Self::Null => built.push(Self::Null),
                    Self::Bool(flag) => built.push(Self::Bool(*flag)),
                    Self::Number(number) => built.push(Self::Number(*number)),
                    Self::String(text) => built.push(Self::String(text.clone())),
                    Self::Array(items) => {
                        work.push(CloneWork::Array(items.len()));
                        work.extend(items.iter().rev().map(CloneWork::Visit));
                    }
                    Self::Object(object) => {
                        work.push(CloneWork::Object(
                            object.entries.iter().map(|(key, _)| key.clone()).collect(),
                        ));
                        work.extend(
                            object
                                .entries
                                .iter()
                                .rev()
                                .map(|(_, item)| CloneWork::Visit(item)),
                        );
                    }
                },
                CloneWork::Array(length) => {
                    let items = built.split_off(built.len() - length);
                    built.push(Self::Array(items));
                }
                CloneWork::Object(keys) => {
                    let values = built.split_off(built.len() - keys.len());
                    built.push(Self::Object(JsObject {
                        entries: keys.into_iter().zip(values).collect(),
                    }));
                }
            }
        }
        built.pop().unwrap_or(Self::Null)
    }
}

impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((left, right)) = pending.pop() {
            match (left, right) {
                (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => {}
                (Self::Bool(a), Self::Bool(b)) if a == b => {}
                (Self::String(a), Self::String(b)) if a == b => {}
                (Self::Number(a), Self::Number(b)) if a == b => {}
                (Self::Array(a), Self::Array(b)) if a.len() == b.len() => {
                    pending.extend(a.iter().zip(b));
                }
                (Self::Object(a), Self::Object(b)) if a.entries.len() == b.entries.len() => {
                    for ((left_key, left_value), (right_key, right_value)) in
                        a.entries.iter().zip(&b.entries)
                    {
                        if left_key != right_key {
                            return false;
                        }
                        pending.push((left_value, right_value));
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

impl From<&serde_json::Value> for JsValue {
    /// The value `JSON.parse` yields for the JSON text `serde_json` writes
    /// for `value`: every number is an IEEE double (an integer beyond 2^53
    /// rounds to the nearest one, `-0` keeps its sign), and object keys keep
    /// their order. Iterative, so depth never overflows the stack.
    fn from(value: &serde_json::Value) -> Self {
        use serde_json::Value;
        enum Work<'a> {
            Visit(&'a Value),
            Array(usize),
            Object(Vec<&'a str>),
        }
        let mut work = vec![Work::Visit(value)];
        let mut built: Vec<JsValue> = Vec::new();
        while let Some(step) = work.pop() {
            match step {
                Work::Visit(Value::Null) => built.push(JsValue::Null),
                Work::Visit(Value::Bool(flag)) => built.push(JsValue::Bool(*flag)),
                // `as_f64` is `n as f64` for an integer, which rounds to the
                // nearest double, and the double itself otherwise.
                Work::Visit(Value::Number(number)) => {
                    built.push(JsValue::Number(number.as_f64().unwrap_or(f64::NAN)));
                }
                Work::Visit(Value::String(text)) => built.push(JsValue::String(text.clone())),
                Work::Visit(Value::Array(items)) => {
                    work.push(Work::Array(items.len()));
                    work.extend(items.iter().rev().map(Work::Visit));
                }
                Work::Visit(Value::Object(map)) => {
                    work.push(Work::Object(map.keys().map(String::as_str).collect()));
                    work.extend(map.values().rev().map(Work::Visit));
                }
                Work::Array(length) => {
                    let items = built.split_off(built.len() - length);
                    built.push(JsValue::Array(items));
                }
                Work::Object(keys) => {
                    let values = built.split_off(built.len() - keys.len());
                    let mut object = JsObject::new();
                    for (key, value) in keys.into_iter().zip(values) {
                        object.insert(key, value);
                    }
                    built.push(JsValue::Object(object));
                }
            }
        }
        built.pop().unwrap_or(JsValue::Undefined)
    }
}

impl fmt::Debug for JsValue {
    /// Writes the value as `JSON.stringify` would, and a top-level
    /// `undefined` as `undefined`.
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        if matches!(self, Self::Undefined) {
            return formatter.write_str("undefined");
        }
        formatter.write_str(&stringify(self))
    }
}

/// A `JSON.parse` `SyntaxError` with the exact V8 message of node 22.20.0.
/// `position` counts UTF-16 code units, as V8 reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSyntaxError {
    pub position: usize,
    /// JavaScript text: an unexpected token may be a lone surrogate.
    pub message: String,
}

impl Display for JsonSyntaxError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
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

/// V8 `kMaxContextCharacters` and `kMinOriginalSourceLengthForContext`.
const CONTEXT_CHARACTERS: usize = 10;
const MIN_LENGTH_FOR_CONTEXT: usize = CONTEXT_CHARACTERS * 2 + 1;

/// `JSON.parse(text)`.
///
/// # Errors
///
/// Returns a syntax error, with V8's message, wherever `JSON.parse` throws.
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
            None => return Err(parser.end_of_input()),
            Some(b'{') => {
                parser.index += 1;
                parser.skip_whitespace();
                match parser.peek() {
                    Some(b'}') => {
                        parser.index += 1;
                        JsValue::Object(JsObject::new())
                    }
                    Some(b'"') => {
                        let key = parser.object_key()?;
                        stack.push(Frame::Object(JsObject::new(), key));
                        continue 'value;
                    }
                    _ => return Err(parser.at("Expected property name or '}' in JSON")),
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
            Some(_) => return Err(parser.unexpected_token()),
        };
        loop {
            parser.skip_whitespace();
            match stack.last_mut() {
                None => {
                    if parser.index != parser.bytes.len() {
                        return Err(parser.at("Unexpected non-whitespace character after JSON"));
                    }
                    return Ok(value);
                }
                Some(Frame::Array(items)) => {
                    items.push(value);
                    if parser.eat(b',') {
                        continue 'value;
                    }
                    if !parser.eat(b']') {
                        return Err(parser.at("Expected ',' or ']' after array element in JSON"));
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
                        if parser.peek() != Some(b'"') {
                            return Err(parser.at("Expected double-quoted property name in JSON"));
                        }
                        *key = parser.object_key()?;
                        continue 'value;
                    }
                    if !parser.eat(b'}') {
                        return Err(parser.at("Expected ',' or '}' after property value in JSON"));
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
    fn utf16_index(&self, byte_index: usize) -> usize {
        self.text[..byte_index].encode_utf16().count()
    }

    /// "Unexpected end of JSON input".
    fn end_of_input(&self) -> JsonSyntaxError {
        JsonSyntaxError {
            position: self.utf16_index(self.bytes.len()),
            message: "Unexpected end of JSON input".to_owned(),
        }
    }

    /// `<what> at position P (line L column C)` at the current index.
    fn at(&self, what: &str) -> JsonSyntaxError {
        self.at_index(what, self.index)
    }

    fn at_index(&self, what: &str, byte_index: usize) -> JsonSyntaxError {
        let position = self.utf16_index(byte_index);
        // Lines break at "\n", "\r", and "\r\n"; columns count UTF-16 units.
        let mut line = 1;
        let mut line_start = 0;
        let mut units = 0;
        let mut previous_carriage_return = false;
        for character in self.text[..byte_index].chars() {
            units += character.len_utf16();
            match character {
                '\n' if previous_carriage_return => line_start = units,
                '\n' | '\r' => {
                    line += 1;
                    line_start = units;
                }
                _ => {}
            }
            previous_carriage_return = character == '\r';
        }
        JsonSyntaxError {
            position,
            message: format!(
                "{what} at position {position} (line {line} column {})",
                position - line_start + 1
            ),
        }
    }

    /// `Unexpected token 'c', "<source or context>" is not valid JSON`, or
    /// the end-of-input message past the last character.
    fn unexpected_token(&self) -> JsonSyntaxError {
        let Some(character) = self.text[self.index..].chars().next() else {
            return self.end_of_input();
        };
        let units: Vec<u16> = self.text.encode_utf16().collect();
        let position = self.utf16_index(self.index);
        let mut first_unit = [0_u16; 2];
        let token = js_text_from_utf16(&character.encode_utf16(&mut first_unit)[..1]);
        let context = if units.len() < MIN_LENGTH_FOR_CONTEXT {
            format!("\"{}\"", js_text(self.text))
        } else {
            let start = position.saturating_sub(CONTEXT_CHARACTERS);
            let end = (position + CONTEXT_CHARACTERS).min(units.len());
            format!(
                "{}\"{}\"{}",
                if start > 0 { "..." } else { "" },
                js_text_from_utf16(&units[start..end]),
                if end < units.len() { "..." } else { "" }
            )
        };
        JsonSyntaxError {
            position,
            message: format!("Unexpected token '{token}', {context} is not valid JSON"),
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
        for expected in word.bytes() {
            match self.peek() {
                None => return Err(self.end_of_input()),
                Some(found) if found == expected => self.index += 1,
                Some(_) => return Err(self.unexpected_token()),
            }
        }
        Ok(value)
    }

    /// Reads a key string and the `:` after it.
    fn object_key(&mut self) -> Result<String, JsonSyntaxError> {
        let key = self.string()?;
        self.skip_whitespace();
        if !self.eat(b':') {
            return Err(self.at("Expected ':' after property name in JSON"));
        }
        Ok(key)
    }

    fn digit_next(&self) -> bool {
        self.peek().is_some_and(|byte| byte.is_ascii_digit())
    }

    fn digits(&mut self) {
        while self.digit_next() {
            self.index += 1;
        }
    }

    fn number(&mut self) -> Result<f64, JsonSyntaxError> {
        let start = self.index;
        if self.eat(b'-') && !self.digit_next() {
            return Err(self.at("No number after minus sign in JSON"));
        }
        if self.eat(b'0') {
            if self.digit_next() {
                return Err(self.at("Unexpected number in JSON"));
            }
        } else {
            self.digits();
        }
        if self.eat(b'.') {
            if !self.digit_next() {
                return Err(self.at("Unterminated fractional number in JSON"));
            }
            self.digits();
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !self.digit_next() {
                return Err(self.at("Exponent part is missing a number in JSON"));
            }
            self.digits();
        }
        // Rust parses decimal literals with correct rounding and saturates to
        // infinity on overflow, the same result JavaScript produces.
        Ok(self.text[start..self.index]
            .parse::<f64>()
            .unwrap_or(f64::NAN))
    }

    fn hex4(&mut self) -> Result<u16, JsonSyntaxError> {
        let mut unit = 0_u16;
        for _ in 0..4 {
            let digit = self
                .peek()
                .and_then(|byte| char::from(byte).to_digit(16))
                .ok_or_else(|| self.at("Bad Unicode escape in JSON"))?;
            unit = unit * 16 + u16::try_from(digit).unwrap_or(0);
            self.index += 1;
        }
        Ok(unit)
    }

    fn string(&mut self) -> Result<String, JsonSyntaxError> {
        self.index += 1;
        let mut out = String::new();
        let mut pending_high: Option<u16> = None;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.at("Unterminated string in JSON"));
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
                        None => return Err(self.end_of_input()),
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(_) => {
                            return Err(
                                self.at_index("Bad escaped character in JSON", self.index + 1)
                            );
                        }
                    };
                    out.push(escaped);
                    self.index += 2;
                }
                0x00..=0x1F => {
                    return Err(self.at("Bad control character in string literal in JSON"));
                }
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

enum WriteWork<'a> {
    Value(&'a JsValue, usize),
    Item(usize, bool),
    Key(&'a str, usize, bool),
    Close(char, usize),
}

// V8 `JSON.stringify` recurses and throws a RangeError somewhere between
// 3,000 and 5,000 nesting levels (it depends on the stack in use), so the
// baseline fails to write such values. This writer uses an explicit stack
// and writes them; no fixed depth reproduces V8's limit.
fn write_value(out: &mut String, root: &JsValue, indent: Option<&str>) {
    let mut work = vec![WriteWork::Value(root, 0)];
    while let Some(step) = work.pop() {
        match step {
            WriteWork::Item(depth, first) => {
                if !first {
                    out.push(',');
                }
                newline(out, indent, depth);
            }
            WriteWork::Close(bracket, depth) => {
                newline(out, indent, depth);
                out.push(bracket);
            }
            WriteWork::Key(key, depth, first) => {
                if !first {
                    out.push(',');
                }
                newline(out, indent, depth);
                write_string(out, key);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
            }
            WriteWork::Value(value, depth) => match value {
                JsValue::Undefined | JsValue::Null => out.push_str("null"),
                JsValue::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
                JsValue::Number(number) => out.push_str(&js_number(*number)),
                JsValue::String(text) => write_string(out, text),
                JsValue::Array(items) if items.is_empty() => out.push_str("[]"),
                JsValue::Object(object)
                    if object
                        .iter()
                        .all(|(_, item)| matches!(item, JsValue::Undefined)) =>
                {
                    out.push_str("{}");
                }
                JsValue::Array(items) => {
                    out.push('[');
                    work.push(WriteWork::Close(']', depth));
                    for (position, item) in items.iter().enumerate().rev() {
                        work.push(WriteWork::Value(item, depth + 1));
                        work.push(WriteWork::Item(depth + 1, position == 0));
                    }
                }
                JsValue::Object(object) => {
                    out.push('{');
                    work.push(WriteWork::Close('}', depth));
                    let entries: Vec<(&str, &JsValue)> = object
                        .iter()
                        .filter(|(_, item)| !matches!(item, JsValue::Undefined))
                        .collect();
                    for (position, (key, item)) in entries.into_iter().enumerate().rev() {
                        work.push(WriteWork::Value(item, depth + 1));
                        work.push(WriteWork::Key(key, depth + 1, position == 0));
                    }
                }
            },
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

fn assert_defined(value: &JsValue) {
    assert!(
        !matches!(value, JsValue::Undefined),
        "JSON.stringify(undefined) returns undefined, not JSON text"
    );
}

/// `JSON.stringify(value)`.
///
/// # Panics
///
/// Panics when `value` is [`JsValue::Undefined`]: `JSON.stringify(undefined)`
/// returns `undefined`, not text, so a caller must not write it.
#[must_use]
pub fn stringify(value: &JsValue) -> String {
    assert_defined(value);
    let mut out = String::new();
    write_value(&mut out, value, None);
    out
}

/// `JSON.stringify(value, null, 2)`.
///
/// # Panics
///
/// Panics when `value` is [`JsValue::Undefined`], as [`stringify`] does.
#[must_use]
pub fn stringify_pretty(value: &JsValue) -> String {
    assert_defined(value);
    let mut out = String::new();
    write_value(&mut out, value, Some("  "));
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
    fn undefined_properties_are_omitted_like_json_stringify() {
        // node: JSON.stringify({a: undefined, b: 1, c: [undefined]}) and {x: undefined}
        let mut object = super::JsObject::new();
        object.insert("a", JsValue::Undefined);
        object.insert("b", JsValue::Number(1.0));
        object.insert("c", JsValue::Array(vec![JsValue::Undefined]));
        assert_eq!(stringify(&JsValue::Object(object)), r#"{"b":1,"c":[null]}"#);
        let mut only = super::JsObject::new();
        only.insert("x", JsValue::Undefined);
        let only = JsValue::Object(only);
        assert_eq!(stringify(&only), "{}");
        assert_eq!(stringify_pretty(&only), "{}");
        // The slot stays: a later defined value keeps the first position.
        let mut slotted = super::JsObject::new();
        slotted.insert("m", JsValue::Undefined);
        slotted.insert("e", JsValue::Null);
        slotted.insert("m", JsValue::Number(2.0));
        assert_eq!(stringify(&JsValue::Object(slotted)), r#"{"m":2,"e":null}"#);
        // node: Object.keys({x: undefined}).length === 1
        let JsValue::Object(only) = &only else {
            unreachable!("built as an object")
        };
        assert_eq!((only.len(), only.is_empty()), (1, false));
    }

    #[test]
    #[should_panic(expected = "JSON.stringify(undefined) returns undefined")]
    fn top_level_undefined_has_no_json_text() {
        // node: JSON.stringify(undefined) === undefined
        assert_eq!(format!("{:?}", JsValue::Undefined), "undefined");
        let _ = stringify(&JsValue::Undefined);
    }

    #[test]
    fn accessors_follow_value_kind() {
        let parsed = parse(r#"{"a":"x","b":{}}"#).expect("object");
        assert!(parsed.is_object());
        assert!(parsed.get("a").is_some_and(JsValue::is_string));
        assert!(parsed.get("b").is_some_and(JsValue::is_object));
        assert!(parsed.get("missing").is_none());
        assert!(JsValue::Null.get("a").is_none());
    }

    #[test]
    fn deep_values_clone_compare_and_stringify_without_recursion() {
        let depth = 100_000;
        let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let parsed = parse(&text).expect("JSON.parse accepts deep nesting");
        let copy = parsed.clone();
        assert!(copy == parsed);
        assert_eq!(stringify(&copy), text);
        assert_eq!(format!("{copy:?}"), text, "Debug writes JSON iteratively");
        let objects = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        let nested = parse(&objects).expect("deep objects parse");
        assert_eq!(stringify(&nested.clone()), objects);
        assert!(nested != copy);
    }

    #[test]
    fn raw_escape_character_without_payload_stands_for_itself() {
        // Text that never went through `js_text`: a lone U+10FFFF followed by
        // ordinary characters, or at the end, must not panic or vanish.
        for raw in ["\u{10FFFF}a", "a\u{10FFFF}", "\u{10FFFF}\u{10FFFE}"] {
            let units: Vec<JsTextUnit> = js_text_units(raw).collect();
            assert!(units.contains(&JsTextUnit::Char('\u{10FFFF}')), "{raw:?}");
            assert_eq!(
                stringify(&JsValue::String(raw.to_owned())),
                format!("\"{raw}\"")
            );
        }
    }

    #[test]
    fn deep_nesting_parses_and_drops() {
        let depth = 100_000;
        let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let parsed = parse(&text).expect("JSON.parse accepts deep nesting");
        assert!(matches!(parsed, JsValue::Array(_)));
        drop(parsed);
    }

    /// Messages printed by node 22.20.0 `JSON.parse`, generated by
    /// `tests/oracle/v8-json-errors.mjs`.
    #[test]
    fn syntax_error_messages_match_v8() {
        let fixture =
            parse(include_str!("../tests/fixtures/v8-json-errors.json")).expect("fixture is JSON");
        let rows = fixture.as_array().expect("rows");
        assert_eq!(rows.len(), 83, "every V8 case is checked");
        for row in rows {
            let row = row.as_array().expect("pair");
            let input = row[0].as_str().expect("input");
            let expected = row[1].as_str();
            // The fixture input is JavaScript text without lone surrogates or
            // U+10FFFF, so it is also the raw source string.
            let actual = parse(input).err().map(|error| error.message);
            assert_eq!(actual.as_deref(), expected, "input {input:?}");
        }
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

    /// (text, `String(v)`, `JSON.stringify(v)`, `Object.is(v, -0)`) for
    /// `v = JSON.parse(text)`, printed by node v22.20.0.
    const SERDE_JSON_CASES: [(&str, &str, &str, bool); 28] = [
        ("0", "0", "0", false),
        ("-0", "0", "0", true),
        ("1", "1", "1", false),
        ("-1", "-1", "-1", false),
        (
            "9007199254740991",
            "9007199254740991",
            "9007199254740991",
            false,
        ),
        (
            "9007199254740992",
            "9007199254740992",
            "9007199254740992",
            false,
        ),
        (
            "9007199254740993",
            "9007199254740992",
            "9007199254740992",
            false,
        ),
        (
            "-9007199254740993",
            "-9007199254740992",
            "-9007199254740992",
            false,
        ),
        (
            "18446744073709551615",
            "18446744073709552000",
            "18446744073709552000",
            false,
        ),
        (
            "18446744073709551616",
            "18446744073709552000",
            "18446744073709552000",
            false,
        ),
        (
            "9223372036854775807",
            "9223372036854776000",
            "9223372036854776000",
            false,
        ),
        (
            "-9223372036854775808",
            "-9223372036854776000",
            "-9223372036854776000",
            false,
        ),
        (
            "123456789012345680000",
            "123456789012345680000",
            "123456789012345680000",
            false,
        ),
        ("1e21", "1e+21", "1e+21", false),
        ("1e-7", "1e-7", "1e-7", false),
        ("0.1", "0.1", "0.1", false),
        ("1.5", "1.5", "1.5", false),
        ("-1.5e-7", "-1.5e-7", "-1.5e-7", false),
        ("5e-324", "5e-324", "5e-324", false),
        (
            "1.7976931348623157e308",
            "1.7976931348623157e+308",
            "1.7976931348623157e+308",
            false,
        ),
        (
            "100000000000000000000",
            "100000000000000000000",
            "100000000000000000000",
            false,
        ),
        ("0.000001", "0.000001", "0.000001", false),
        ("1e300", "1e+300", "1e+300", false),
        (
            "[1,[2,[3]],-0,1e21]",
            "1,2,3,0,1e+21",
            "[1,[2,[3]],0,1e+21]",
            false,
        ),
        (
            "{\"b\":1,\"a\":2,\"10\":3,\"2\":4,\"__proto__\":5,\"a\":9}",
            "[object Object]",
            "{\"2\":4,\"10\":3,\"b\":1,\"a\":9,\"__proto__\":5}",
            false,
        ),
        (
            "{\"x\":{\"b\":[],\"a\":{}},\"1\":null,\"0\":true}",
            "[object Object]",
            "{\"0\":true,\"1\":null,\"x\":{\"b\":[],\"a\":{}}}",
            false,
        ),
        (
            "\"é\\u0000\\ud83d\\ude00\"",
            "é\u{0}😀",
            "\"é\\u0000😀\"",
            false,
        ),
        (
            "[null,true,false,\"s\"]",
            ",true,false,s",
            "[null,true,false,\"s\"]",
            false,
        ),
    ];

    // (text, `String(v)`, `JSON.stringify(v)`, `Object.is(v, -0)`), printed by
    // node v22.20.0 for `v = JSON.parse(text)`; `From<&serde_json::Value>`
    // of the same text must give the same.
    #[test]
    fn serde_json_values_convert_like_json_parse() {
        for (text, string, json, negative_zero) in SERDE_JSON_CASES {
            let source: serde_json::Value = serde_json::from_str(text).expect(text);
            let value = JsValue::from(&source);
            assert_eq!(stringify(&value), json, "{text}");
            assert_eq!(crate::js::js_string(Some(&value)), string, "{text}");
            let is_negative_zero =
                matches!(value, JsValue::Number(n) if n == 0.0 && n.is_sign_negative());
            assert_eq!(is_negative_zero, negative_zero, "{text}");
            assert_eq!(value, parse(text).expect("JSON.parse accepts"), "{text}");
        }
    }

    #[test]
    fn deep_serde_json_values_do_not_overflow_the_stack() {
        let depth = 100_000;
        let mut source = serde_json::Value::Null;
        for _ in 0..depth {
            source = serde_json::Value::Array(vec![source]);
        }
        let value = JsValue::from(&source);
        assert!(matches!(&value, JsValue::Array(items) if items.len() == 1));
        // Dropping deep values is iterative too.
        drop(value);
        let mut owned = source;
        while let serde_json::Value::Array(mut items) = owned {
            owned = items.pop().unwrap_or(serde_json::Value::Null);
        }
    }
}
