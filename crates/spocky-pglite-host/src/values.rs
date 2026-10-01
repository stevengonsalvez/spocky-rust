//! Typed values as the retained JavaScript host produces them: `PGlite`'s
//! serializers and parsers, the host's `jsonParsers`, `decodeParams` and
//! `encodeValue`, including JavaScript number, `Date` and JSON semantics.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::jsdate::{JsClock, time_clip, utc_fields};
use crate::protocol::BindValue;

/// The retained host's IPC value, with the same serde shape as the Rust
/// adapter in `spocky-hub-pilot`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum IpcValue {
    Null,
    Boolean(bool),
    String(String),
    Binary(Vec<u8>),
    Timestamp(String),
    Numeric(String),
    Json(serde_json::Value),
}

/// A JavaScript exception raised while serializing or encoding values. The
/// retained host reports it with code `REMOTE_ERROR`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsError {
    pub name: &'static str,
    pub message: String,
}

impl JsError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}

/// A JavaScript value produced by a parser or by `decodeParams`.
#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Null,
    Boolean(bool),
    Number(f64),
    BigInt(String),
    String(String),
    Date(f64),
    Bytes(Vec<u8>),
    Array(Vec<JsValue>),
    /// A parsed JSON document (`JSON.parse`).
    Json(Json),
    /// The retained host's `{ [jsonValue]: ... }` wrapper.
    Wrapped(Json),
}

/// `JSON.parse` output: numbers are doubles, objects keep JavaScript
/// property order.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

// ---------------------------------------------------------------------------
// Number formatting
// ---------------------------------------------------------------------------

/// ECMAScript `Number::toString(10)`.
#[must_use]
pub fn js_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = i32::try_from(digits.len()).unwrap_or(1);
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!(
            "{digits}{}",
            "0".repeat(usize::try_from(n - k).unwrap_or(0))
        )
    } else if 0 < n && n <= 21 {
        let split = usize::try_from(n).unwrap_or(0);
        format!("{}.{}", &digits[..split], &digits[split..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(usize::try_from(-n).unwrap_or(0)))
    } else {
        let exponent = n - 1;
        let sign = if exponent >= 0 { '+' } else { '-' };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{first}e{sign}{}", exponent.abs())
        } else {
            format!("{first}.{rest}e{sign}{}", exponent.abs())
        }
    };
    format!("{sign}{body}")
}

/// JavaScript `+text` (`ToNumber` of a string).
#[must_use]
pub fn js_to_number(text: &str) -> f64 {
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return 0.0;
    }
    match trimmed {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            return u128::from_str_radix(digits, radix).map_or(f64::NAN, |value| {
                #[allow(clippy::cast_precision_loss, reason = "ToNumber rounds to a double")]
                let value = value as f64;
                value
            });
        }
    }
    let valid = trimmed
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !valid {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `String.prototype.trim`: ECMAScript white space and line terminators.
#[must_use]
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// `Date.prototype.toISOString`, or the `RangeError` it throws.
///
/// # Errors
///
/// Returns `RangeError: Invalid time value` for an invalid date.
pub fn to_iso_string(value: f64) -> Result<String, JsError> {
    if !value.is_finite() {
        return Err(JsError::new("RangeError", "Invalid time value"));
    }
    let fields = utc_fields(value);
    let millisecond = value - (value / 1000.0).floor() * 1000.0;
    let year = if (0..=9999).contains(&fields.year) {
        format!("{:04}", fields.year)
    } else if fields.year < 0 {
        format!("-{:06}", -i64::from(fields.year))
    } else {
        format!("+{:06}", fields.year)
    };
    Ok(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{}Z",
        fields.month + 1,
        fields.day,
        fields.hour,
        fields.minute,
        fields.second,
        millisecond_text(millisecond)
    ))
}

fn digits_at(bytes: &[u8], start: usize) -> (u64, usize) {
    let mut value = 0_u64;
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() && end - start < 18 {
        value = value * 10 + u64::from(bytes[end] - b'0');
        end += 1;
    }
    (value, end)
}

fn millisecond_text(millisecond: f64) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the remainder is an integer in [0, 1000)"
    )]
    let value = millisecond as u32;
    format!("{value:03}")
}

/// `new Date(text).getTime()` for the text `PostgreSQL` prints with
/// `DateStyle = ISO`. Date-only values with a four-digit year follow the
/// ECMAScript ISO rule (UTC); everything else follows V8's legacy parser
/// (local time unless an offset is given, two-digit year mapping below 100).
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one port of the V8 legacy date grammar for ISO output"
)]
pub fn parse_postgres_date(clock: &JsClock, text: &str) -> f64 {
    let bytes = text.as_bytes();
    let (year, after_year) = digits_at(bytes, 0);
    let year_digits = after_year;
    if year_digits == 0 || bytes.get(after_year) != Some(&b'-') {
        return f64::NAN;
    }
    let (month, after_month) = digits_at(bytes, after_year + 1);
    if after_month - after_year - 1 != 2 || bytes.get(after_month) != Some(&b'-') {
        return f64::NAN;
    }
    let (day, after_day) = digits_at(bytes, after_month + 1);
    if after_day - after_month - 1 != 2 {
        return f64::NAN;
    }
    #[allow(clippy::cast_precision_loss, reason = "calendar fields are small")]
    let (year_f, month_f, day_f) = (year as f64, month as f64, day as f64);
    if after_day == bytes.len() {
        if year_digits == 4 {
            if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
                return f64::NAN;
            }
            let days = crate::jsdate::make_day_public(year_f, month_f - 1.0, day_f);
            return time_clip(days * 86_400_000.0);
        }
        return clock.local_constructor(year_f, month_f - 1.0, day_f, 0.0, 0.0, 0.0, 0.0);
    }
    if bytes.get(after_day) != Some(&b' ') {
        return f64::NAN;
    }
    let (hour, after_hour) = digits_at(bytes, after_day + 1);
    if after_hour - after_day - 1 != 2 || bytes.get(after_hour) != Some(&b':') {
        return f64::NAN;
    }
    let (minute, after_minute) = digits_at(bytes, after_hour + 1);
    if after_minute - after_hour - 1 != 2 || bytes.get(after_minute) != Some(&b':') {
        return f64::NAN;
    }
    let (second, mut cursor) = digits_at(bytes, after_minute + 1);
    if cursor - after_minute - 1 != 2 {
        return f64::NAN;
    }
    let mut millisecond = 0_u64;
    if bytes.get(cursor) == Some(&b'.') {
        let start = cursor + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return f64::NAN;
        }
        let fraction = &text[start..end];
        let padded = format!("{fraction:0<3}");
        millisecond = padded[..3].parse().unwrap_or(0);
        cursor = end;
    }
    let mut offset_minutes: Option<i64> = None;
    if cursor < bytes.len() {
        let sign = match bytes[cursor] {
            b'+' => 1,
            b'-' => -1,
            _ => return f64::NAN,
        };
        let (hours, after) = digits_at(bytes, cursor + 1);
        if after - cursor - 1 != 2 {
            return f64::NAN;
        }
        let mut minutes = 0;
        let mut end = after;
        if bytes.get(after) == Some(&b':') {
            let (value, after_minutes) = digits_at(bytes, after + 1);
            if after_minutes - after - 1 != 2 {
                return f64::NAN;
            }
            minutes = value;
            end = after_minutes;
        }
        if end != bytes.len() {
            return f64::NAN;
        }
        offset_minutes = Some(sign * i64::try_from(hours * 60 + minutes).unwrap_or(0));
    }
    let year_f = if year < 50 {
        year_f + 2000.0
    } else if year < 100 {
        year_f + 1900.0
    } else {
        year_f
    };
    #[allow(clippy::cast_precision_loss, reason = "clock fields are small")]
    let (hour_f, minute_f, second_f, millisecond_f) = (
        hour as f64,
        minute as f64,
        second as f64,
        millisecond as f64,
    );
    if hour > 24
        || minute > 59
        || second > 59
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return f64::NAN;
    }
    match offset_minutes {
        None => clock.local_constructor(
            year_f,
            month_f - 1.0,
            day_f,
            hour_f,
            minute_f,
            second_f,
            millisecond_f,
        ),
        Some(offset) => {
            let day_number = crate::jsdate::make_day_public(year_f, month_f - 1.0, day_f);
            let time = ((hour_f * 60.0 + minute_f) * 60.0 + second_f) * 1000.0 + millisecond_f;
            #[allow(clippy::cast_precision_loss, reason = "offsets are small")]
            let offset_ms = offset as f64 * 60_000.0;
            time_clip(day_number * 86_400_000.0 + time - offset_ms)
        }
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// JavaScript property order: integer-like keys first ascending.
fn js_key_order(entries: &[(String, Json)]) -> Vec<&(String, Json)> {
    let index = |key: &str| -> Option<u32> {
        if key.is_empty()
            || (key.len() > 1 && key.starts_with('0'))
            || !key.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        key.parse::<u64>()
            .ok()
            .filter(|value| *value < 4_294_967_295)
            .and_then(|value| u32::try_from(value).ok())
    };
    let mut numeric: Vec<(u32, &(String, Json))> = entries
        .iter()
        .filter_map(|entry| index(&entry.0).map(|value| (value, entry)))
        .collect();
    numeric.sort_by_key(|(value, _)| *value);
    let mut ordered: Vec<&(String, Json)> = numeric.into_iter().map(|(_, entry)| entry).collect();
    ordered.extend(entries.iter().filter(|entry| index(&entry.0).is_none()));
    ordered
}

fn insert_property(entries: &mut Vec<(String, Json)>, key: String, value: Json) {
    if let Some(entry) = entries.iter_mut().find(|entry| entry.0 == key) {
        entry.1 = value;
    } else {
        entries.push((key, value));
    }
}

struct JsonParser<'a> {
    text: &'a [u8],
    offset: usize,
}

impl JsonParser<'_> {
    fn error(&self) -> JsError {
        JsError::new(
            "SyntaxError",
            format!("Unexpected token in JSON at position {}", self.offset),
        )
    }

    fn whitespace(&mut self) {
        while self.offset < self.text.len()
            && matches!(self.text[self.offset], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.offset += 1;
        }
    }

    fn value(&mut self) -> Result<Json, JsError> {
        self.whitespace();
        match self.text.get(self.offset) {
            Some(b'{') => {
                self.offset += 1;
                let mut entries = Vec::new();
                self.whitespace();
                if self.text.get(self.offset) == Some(&b'}') {
                    self.offset += 1;
                    return Ok(Json::Object(entries));
                }
                loop {
                    self.whitespace();
                    let key = self.string()?;
                    self.whitespace();
                    if self.text.get(self.offset) != Some(&b':') {
                        return Err(self.error());
                    }
                    self.offset += 1;
                    let value = self.value()?;
                    insert_property(&mut entries, key, value);
                    self.whitespace();
                    match self.text.get(self.offset) {
                        Some(b',') => self.offset += 1,
                        Some(b'}') => {
                            self.offset += 1;
                            return Ok(Json::Object(entries));
                        }
                        _ => return Err(self.error()),
                    }
                }
            }
            Some(b'[') => {
                self.offset += 1;
                let mut items = Vec::new();
                self.whitespace();
                if self.text.get(self.offset) == Some(&b']') {
                    self.offset += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.whitespace();
                    match self.text.get(self.offset) {
                        Some(b',') => self.offset += 1,
                        Some(b']') => {
                            self.offset += 1;
                            return Ok(Json::Array(items));
                        }
                        _ => return Err(self.error()),
                    }
                }
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Boolean(true)),
            Some(b'f') => self.literal("false", Json::Boolean(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(_) => self.number(),
            None => Err(self.error()),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, JsError> {
        if self.text[self.offset..].starts_with(word.as_bytes()) {
            self.offset += word.len();
            Ok(value)
        } else {
            Err(self.error())
        }
    }

    fn number(&mut self) -> Result<Json, JsError> {
        let start = self.offset;
        while self.offset < self.text.len()
            && matches!(
                self.text[self.offset],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.offset += 1;
        }
        let lexeme =
            std::str::from_utf8(&self.text[start..self.offset]).map_err(|_| self.error())?;
        lexeme
            .parse::<f64>()
            .map(Json::Number)
            .map_err(|_| self.error())
    }

    fn string(&mut self) -> Result<String, JsError> {
        if self.text.get(self.offset) != Some(&b'"') {
            return Err(self.error());
        }
        self.offset += 1;
        let mut units: Vec<u16> = Vec::new();
        loop {
            let Some(&byte) = self.text.get(self.offset) else {
                return Err(self.error());
            };
            match byte {
                b'"' => {
                    self.offset += 1;
                    break;
                }
                b'\\' => {
                    let escape = *self.text.get(self.offset + 1).ok_or_else(|| self.error())?;
                    self.offset += 2;
                    match escape {
                        b'"' => units.push(u16::from(b'"')),
                        b'\\' => units.push(u16::from(b'\\')),
                        b'/' => units.push(u16::from(b'/')),
                        b'b' => units.push(8),
                        b'f' => units.push(12),
                        b'n' => units.push(10),
                        b'r' => units.push(13),
                        b't' => units.push(9),
                        b'u' => {
                            let hex = self
                                .text
                                .get(self.offset..self.offset + 4)
                                .and_then(|digits| std::str::from_utf8(digits).ok())
                                .and_then(|digits| u16::from_str_radix(digits, 16).ok())
                                .ok_or_else(|| self.error())?;
                            units.push(hex);
                            self.offset += 4;
                        }
                        _ => return Err(self.error()),
                    }
                }
                _ => {
                    let rest =
                        std::str::from_utf8(&self.text[self.offset..]).map_err(|_| self.error())?;
                    let character = rest.chars().next().ok_or_else(|| self.error())?;
                    let mut buffer = [0_u16; 2];
                    units.extend_from_slice(character.encode_utf16(&mut buffer));
                    self.offset += character.len_utf8();
                }
            }
        }
        String::from_utf16(&units).map_err(|_| {
            JsError::new(
                "SyntaxError",
                "lone surrogate in JSON string cannot cross the IPC boundary",
            )
        })
    }
}

/// `JSON.parse(text)`.
///
/// # Errors
///
/// Returns a `SyntaxError` for invalid JSON.
pub fn json_parse(text: &str) -> Result<Json, JsError> {
    let mut parser = JsonParser {
        text: text.as_bytes(),
        offset: 0,
    };
    let value = parser.value()?;
    parser.whitespace();
    if parser.offset != parser.text.len() {
        return Err(parser.error());
    }
    Ok(value)
}

/// The JSON text value the Rust adapter would decode after `JSON.stringify`.
fn json_to_serde(value: &Json) -> serde_json::Value {
    match value {
        Json::Null => serde_json::Value::Null,
        Json::Boolean(flag) => serde_json::Value::Bool(*flag),
        Json::Number(number) => number_to_serde(*number),
        Json::String(text) => serde_json::Value::String(text.clone()),
        Json::Array(items) => serde_json::Value::Array(items.iter().map(json_to_serde).collect()),
        Json::Object(entries) => serde_json::Value::Object(
            js_key_order(entries)
                .into_iter()
                .map(|(key, value)| (key.clone(), json_to_serde(value)))
                .collect(),
        ),
    }
}

/// A JavaScript number written by `JSON.stringify` and read by serde.
fn number_to_serde(number: f64) -> serde_json::Value {
    if !number.is_finite() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(&js_number_to_string(number)).unwrap_or(serde_json::Value::Null)
}

/// `JSON.stringify` of a string.
fn quote_json(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(output, "\\u{:04x}", u32::from(control));
            }
            other => output.push(other),
        }
    }
    output.push('"');
}

/// `JSON.stringify` of an IPC JSON value after `JSON.parse` of the request
/// frame: numbers become doubles and object keys follow JavaScript order.
#[must_use]
pub fn stringify_ipc_json(value: &serde_json::Value) -> String {
    let mut output = String::new();
    write_ipc_json(value, &mut output);
    output
}

fn write_ipc_json(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Null => output.push_str("null"),
        serde_json::Value::Bool(flag) => output.push_str(if *flag { "true" } else { "false" }),
        serde_json::Value::Number(number) => {
            let double = number.as_f64().unwrap_or(f64::NAN);
            if double.is_finite() {
                output.push_str(&js_number_to_string(double));
            } else {
                output.push_str("null");
            }
        }
        serde_json::Value::String(text) => quote_json(text, output),
        serde_json::Value::Array(items) => {
            output.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    output.push(',');
                }
                write_ipc_json(item, output);
            }
            output.push(']');
        }
        serde_json::Value::Object(map) => {
            let entries: Vec<(String, Json)> = map
                .iter()
                .map(|(key, _)| (key.clone(), Json::Null))
                .collect();
            output.push('{');
            for (position, (key, _)) in js_key_order(&entries).into_iter().enumerate() {
                if position > 0 {
                    output.push(',');
                }
                quote_json(key, output);
                output.push(':');
                if let Some(item) = map.get(key) {
                    write_ipc_json(item, output);
                }
            }
            output.push('}');
        }
    }
}

// ---------------------------------------------------------------------------
// Array parser (`pn`) and value parsing
// ---------------------------------------------------------------------------

/// `PGlite` `arrayParser`, including its handling of quotes and `NULL`.
fn parse_array(text: &str, element: &dyn Fn(&str) -> JsValue, type_oid: i32) -> JsValue {
    let characters: Vec<char> = text.chars().collect();
    let delimiter = if type_oid == 1020 { ';' } else { ',' };
    let mut state = ArrayState {
        index: 0,
        last: 0,
        quoted: false,
        previous: None,
        text: String::new(),
    };
    let mut items = parse_array_level(&mut state, &characters, element, delimiter);
    if items.is_empty() {
        JsValue::Null
    } else {
        items.remove(0)
    }
}

struct ArrayState {
    index: usize,
    last: usize,
    quoted: bool,
    previous: Option<char>,
    text: String,
}

fn parse_array_level(
    state: &mut ArrayState,
    characters: &[char],
    element: &dyn Fn(&str) -> JsValue,
    delimiter: char,
) -> Vec<JsValue> {
    let slice = |from: usize, to: usize| -> String {
        characters[from.min(characters.len())..to.min(characters.len())]
            .iter()
            .collect()
    };
    let mut items = Vec::new();
    while state.index < characters.len() {
        let character = characters[state.index];
        if state.quoted {
            if character == '\\' {
                state.index += 1;
                if let Some(next) = characters.get(state.index) {
                    state.text.push(*next);
                }
            } else if character == '"' {
                let value = std::mem::take(&mut state.text);
                items.push(element(&value));
                state.quoted = characters.get(state.index + 1) == Some(&'"');
                state.last = state.index + 2;
            } else {
                state.text.push(character);
            }
        } else if character == '"' {
            state.quoted = true;
        } else if character == '{' {
            state.index += 1;
            state.last = state.index;
            items.push(JsValue::Array(parse_array_level(
                state, characters, element, delimiter,
            )));
        } else if character == '}' {
            if state.last < state.index {
                let value = slice(state.last, state.index);
                items.push(if value == "NULL" && !state.quoted {
                    JsValue::Null
                } else {
                    element(&value)
                });
            }
            state.quoted = false;
            state.last = state.index + 1;
            break;
        } else if character == delimiter
            && state.previous != Some('}')
            && state.previous != Some('"')
        {
            let value = slice(state.last, state.index);
            items.push(if value == "NULL" && !state.quoted {
                JsValue::Null
            } else {
                element(&value)
            });
            state.last = state.index + 1;
        }
        state.previous = Some(character);
        state.index += 1;
    }
    if state.last < state.index {
        let value = slice(state.last, state.index + 1);
        items.push(element(&value));
    }
    items
}

/// Type information needed to parse result cells.
#[derive(Debug, Default, Clone)]
pub struct TypeRegistry {
    /// Array type OID to element type OID, from `_initArrayTypes`.
    pub arrays: HashMap<i32, i32>,
}

pub const JSON_OID: i32 = 114;
pub const JSONB_OID: i32 = 3802;

impl TypeRegistry {
    /// The `PGlite` default parser for `oid` (`this.parsers`), applied to the
    /// cell text.
    fn parse_default(&self, clock: &JsClock, oid: i32, text: &str) -> Result<JsValue, JsError> {
        Ok(match oid {
            25 | 1043 | 1042 => JsValue::String(text.to_owned()),
            21 | 23 | 26 | 700 | 701 => JsValue::Number(js_to_number(text)),
            20 => {
                let value: i128 = text.parse().map_err(|_| {
                    JsError::new("SyntaxError", format!("Cannot convert {text} to a BigInt"))
                })?;
                if (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value) {
                    #[allow(clippy::cast_precision_loss, reason = "value is within 2^53")]
                    let number = value as f64;
                    JsValue::Number(number)
                } else {
                    JsValue::BigInt(value.to_string())
                }
            }
            JSON_OID | JSONB_OID => JsValue::Json(json_parse(text)?),
            16 => JsValue::Boolean(text == "t"),
            1082 | 1114 | 1184 => JsValue::Date(parse_postgres_date(clock, text)),
            17 => {
                let hex = text.get(2..).unwrap_or("");
                let length = hex.len() / 2;
                let mut bytes = Vec::with_capacity(length);
                for index in 0..length {
                    let pair = &hex[index * 2..index * 2 + 2];
                    // parseInt of a non-hex pair is NaN, stored as 0.
                    bytes.push(u8::from_str_radix(pair, 16).unwrap_or(0));
                }
                JsValue::Bytes(bytes)
            }
            _ => {
                if let Some(&element) = self.arrays.get(&oid) {
                    let parse_element = |value: &str| {
                        self.parse_default(clock, element, value)
                            .unwrap_or_else(|_| JsValue::String(value.to_owned()))
                    };
                    let parsed = parse_array(text, &parse_element, oid);
                    // Element parse errors surface when the array is used.
                    return Ok(parsed);
                }
                JsValue::String(text.to_owned())
            }
        })
    }

    /// A result cell parsed with the retained host's `jsonParsers` override.
    ///
    /// # Errors
    ///
    /// Returns the JavaScript exception a parser throws.
    pub fn parse_cell(
        &self,
        clock: &JsClock,
        oid: i32,
        text: Option<&str>,
    ) -> Result<JsValue, JsError> {
        let Some(text) = text else {
            return Ok(JsValue::Null);
        };
        if oid == JSON_OID || oid == JSONB_OID {
            return Ok(JsValue::Wrapped(json_parse(text)?));
        }
        self.parse_default(clock, oid, text)
    }
}

/// The retained host's `encodeValue(value, oid)`.
///
/// # Errors
///
/// Returns the exception `toISOString` or `JSON.stringify` throws.
pub fn encode_value(value: &JsValue, oid: i32) -> Result<IpcValue, JsError> {
    Ok(match value {
        JsValue::Wrapped(json) => IpcValue::Json(json_to_serde(json)),
        JsValue::Null => IpcValue::Null,
        _ if oid == JSON_OID || oid == JSONB_OID => IpcValue::Json(js_to_serde(value)?),
        JsValue::Bytes(bytes) => IpcValue::Binary(bytes.clone()),
        JsValue::Date(time) => IpcValue::Timestamp(to_iso_string(*time)?),
        JsValue::BigInt(digits) => IpcValue::Numeric(digits.clone()),
        JsValue::Number(number) => IpcValue::Numeric(js_number_to_string(*number)),
        JsValue::String(text) if matches!(oid, 20 | 21 | 23 | 700 | 701 | 1700) => {
            IpcValue::Numeric(text.clone())
        }
        JsValue::Boolean(flag) => IpcValue::Boolean(*flag),
        JsValue::String(text) => IpcValue::String(text.clone()),
        JsValue::Array(_) | JsValue::Json(_) => IpcValue::Json(js_to_serde(value)?),
    })
}

/// `JSON.stringify` of a parsed JavaScript value, as serde reads it back.
fn js_to_serde(value: &JsValue) -> Result<serde_json::Value, JsError> {
    Ok(match value {
        JsValue::Null => serde_json::Value::Null,
        JsValue::Boolean(flag) => serde_json::Value::Bool(*flag),
        JsValue::Number(number) => number_to_serde(*number),
        JsValue::BigInt(_) => {
            return Err(JsError::new(
                "TypeError",
                "Do not know how to serialize a BigInt",
            ));
        }
        JsValue::String(text) => serde_json::Value::String(text.clone()),
        JsValue::Date(time) => {
            to_iso_string(*time).map_or(serde_json::Value::Null, serde_json::Value::String)
        }
        JsValue::Bytes(bytes) => serde_json::Value::Object(
            bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| (index.to_string(), serde_json::Value::from(*byte)))
                .collect(),
        ),
        JsValue::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(js_to_serde)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        JsValue::Json(json) | JsValue::Wrapped(json) => json_to_serde(json),
    })
}

/// `decodeParams` then `PGlite`'s serializer for the described parameter type.
///
/// # Errors
///
/// Returns the JavaScript exception a serializer throws.
pub fn serialize_param(
    value: &IpcValue,
    oid: i32,
    registry: &TypeRegistry,
) -> Result<BindValue, JsError> {
    // decodeParams
    let decoded = match value {
        IpcValue::Null => return Ok(BindValue::Null),
        IpcValue::Boolean(flag) => JsValue::Boolean(*flag),
        IpcValue::String(text) | IpcValue::Timestamp(text) | IpcValue::Numeric(text) => {
            JsValue::String(text.clone())
        }
        IpcValue::Json(json) => JsValue::String(stringify_ipc_json(json)),
        IpcValue::Binary(bytes) => JsValue::Bytes(bytes.clone()),
    };
    let to_string = |value: &JsValue| match value {
        JsValue::String(text) => text.clone(),
        JsValue::Boolean(flag) => flag.to_string(),
        JsValue::Bytes(bytes) => bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(","),
        _ => String::new(),
    };
    Ok(match oid {
        25 | 1043 | 1042 | 0 | 21 | 23 | 26 | 700 | 701 | 20 => {
            BindValue::Text(to_string(&decoded))
        }
        JSON_OID | JSONB_OID => match &decoded {
            JsValue::String(text) => BindValue::Text(text.clone()),
            JsValue::Boolean(flag) => BindValue::Text(flag.to_string()),
            JsValue::Bytes(bytes) => {
                let mut text = String::from("{");
                for (index, byte) in bytes.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    let _ = write!(text, "\"{index}\":{byte}");
                }
                text.push('}');
                BindValue::Text(text)
            }
            _ => BindValue::Text(String::new()),
        },
        16 => match &decoded {
            JsValue::Boolean(flag) => BindValue::Text(if *flag { "t" } else { "f" }.to_owned()),
            JsValue::String(text) => {
                let lowered = js_trim(text).to_lowercase();
                if ["true", "t", "yes", "y", "on", "1"].contains(&lowered.as_str()) {
                    BindValue::Text("t".to_owned())
                } else if ["false", "f", "no", "n", "off", "0"].contains(&lowered.as_str()) {
                    BindValue::Text("f".to_owned())
                } else {
                    return Err(JsError::new("Error", "Invalid input for boolean type"));
                }
            }
            _ => return Err(JsError::new("Error", "Invalid input for boolean type")),
        },
        1184 | 1082 | 1114 => match &decoded {
            JsValue::String(text) => BindValue::Text(text.clone()),
            _ => return Err(JsError::new("Error", "Invalid input for date type")),
        },
        17 => match &decoded {
            JsValue::Bytes(bytes) => {
                let mut text = String::from("\\x");
                for byte in bytes {
                    let _ = write!(text, "{byte:02x}");
                }
                BindValue::Text(text)
            }
            _ => return Err(JsError::new("Error", "Invalid input for bytea type")),
        },
        _ if registry.arrays.contains_key(&oid) => match decoded {
            JsValue::Bytes(bytes) => BindValue::Binary(bytes),
            other => BindValue::Text(to_string(&other)),
        },
        _ => BindValue::Text(to_string(&decoded)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_javascript() {
        for (value, text) in [
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (0.1, "0.1"),
            (-0.0, "0"),
            (123_456_789_012_345_680_000.0, "123456789012345680000"),
            (1.5e300, "1.5e+300"),
            (5e-324, "5e-324"),
            (3.402_823_466_385_288_6e38, "3.4028234663852886e+38"),
            (-2.5, "-2.5"),
            (100.0, "100"),
            (0.000_001, "0.000001"),
        ] {
            assert_eq!(js_number_to_string(value), text);
        }
        assert_eq!(js_number_to_string(f64::NAN), "NaN");
        assert_eq!(js_number_to_string(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn postgres_dates_parse_like_v8_in_london() {
        let clock = JsClock::for_zone("Europe/London");
        let iso = |text: &str| {
            to_iso_string(parse_postgres_date(&clock, text)).unwrap_or_else(|error| error.message)
        };
        assert_eq!(
            iso("2026-10-01 00:00:00.123+00"),
            "2026-10-01T00:00:00.123Z"
        );
        assert_eq!(iso("2026-10-01 00:00:00"), "2026-09-30T23:00:00.000Z");
        assert_eq!(iso("2026-10-01"), "2026-10-01T00:00:00.000Z");
        assert_eq!(
            iso("2026-07-01 12:34:56.789123"),
            "2026-07-01T11:34:56.789Z"
        );
        assert_eq!(
            iso("2026-07-01 12:34:56.789123+05:30"),
            "2026-07-01T07:04:56.789Z"
        );
        assert_eq!(iso("2026-07-01 12:34:56-08"), "2026-07-01T20:34:56.000Z");
        assert_eq!(iso("0044-03-15 12:00:00 BC"), "Invalid time value");
        assert_eq!(iso("infinity"), "Invalid time value");
        assert_eq!(iso("1900-01-01 00:00:00+00:01:15"), "Invalid time value");
        assert_eq!(iso("0044-03-15 12:00:00"), "2044-03-15T12:00:00.000Z");
        assert_eq!(iso("0044-03-15"), "0044-03-15T00:00:00.000Z");
        assert_eq!(
            iso("275760-09-13 00:00:00+00"),
            "+275760-09-13T00:00:00.000Z"
        );
        assert_eq!(iso("275760-09-13 00:00:01+00"), "Invalid time value");
        assert_eq!(iso("2026-07-01 24:00:00"), "2026-07-01T23:00:00.000Z");
    }

    #[test]
    fn json_round_trips_through_javascript_numbers_and_order() {
        let parsed = json_parse(r#"{"b":1,"a":2.50,"1":12345678901234567890,"b":3,"x":1e400}"#)
            .expect("json");
        assert_eq!(
            json_to_serde(&parsed),
            serde_json::json!({"1": 12_345_678_901_234_567_000_u64, "b": 3, "a": 2.5, "x": null})
        );
        let request = serde_json::json!({"z": [1, 2.0, "q\n"], "10": true, "2": null});
        assert_eq!(
            stringify_ipc_json(&request),
            r#"{"2":null,"10":true,"z":[1,2,"q\n"]}"#
        );
    }

    #[test]
    fn arrays_and_serializers_follow_pglite() {
        let mut registry = TypeRegistry::default();
        registry.arrays.insert(1007, 23);
        registry.arrays.insert(1009, 25);
        let clock = JsClock::for_zone("UTC");
        let ints = registry
            .parse_cell(&clock, 1007, Some("{1,2,NULL}"))
            .expect("array");
        assert_eq!(
            encode_value(&ints, 1007).expect("encode"),
            IpcValue::Json(serde_json::json!([1, 2, null]))
        );
        let texts = registry
            .parse_cell(&clock, 1009, Some(r#"{a,"b c","x\"y"}"#))
            .expect("array");
        assert_eq!(
            encode_value(&texts, 1009).expect("encode"),
            IpcValue::Json(serde_json::json!(["a", "b c", "x\"y"]))
        );
        assert_eq!(
            serialize_param(&IpcValue::Binary(vec![0, 255]), 17, &registry),
            Ok(BindValue::Text("\\x00ff".into()))
        );
        assert_eq!(
            serialize_param(&IpcValue::String("x".into()), 17, &registry),
            Err(JsError::new("Error", "Invalid input for bytea type"))
        );
        assert_eq!(
            serialize_param(&IpcValue::Boolean(true), 25, &registry),
            Ok(BindValue::Text("true".into()))
        );
    }
}
