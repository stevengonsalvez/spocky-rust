//! Ordered JSON with the JavaScript object and number semantics the Hub public API exposes.
//!
//! Object key order is observable in Hub responses, so objects are vectors of pairs. Parsing
//! follows `JSON.parse` (duplicate keys keep the first position and the last value, integer-like
//! keys come first in ascending order) and printing follows `JSON.stringify` without spacing.

use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Builds an object from key and value pairs, keeping the given order.
    #[must_use]
    pub fn object<const N: usize>(fields: [(&str, Json); N]) -> Self {
        Self::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    #[must_use]
    pub fn string(value: &str) -> Self {
        Self::String(value.to_owned())
    }

    /// Hub numbers are JavaScript doubles, so integers above 2^53 lose precision there too.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn integer(value: i64) -> Self {
        Self::Number(value as f64)
    }

    /// The `typeof`-style name zod prints in `received` clauses.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// `JSON.stringify(value)`: compact, non-finite numbers print as `null`.
    #[must_use]
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => out.push_str(&number_text(*value)),
            Self::String(value) => write_string(value, out),
            Self::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// `Number.prototype.toString()` for the values `JSON.stringify` prints (non-finite is `null`).
#[must_use]
pub fn number_text(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let position = exponent + 1;
    let sign = if value < 0.0 { "-" } else { "" };
    let body = if count <= position && position <= 21 {
        format!(
            "{digits}{}",
            "0".repeat(usize::try_from(position - count).unwrap_or(0))
        )
    } else if 0 < position && position <= 21 {
        let split = usize::try_from(position).unwrap_or(0);
        format!("{}.{}", &digits[..split], &digits[split..])
    } else if -6 < position && position <= 0 {
        format!(
            "0.{}{digits}",
            "0".repeat(usize::try_from(-position).unwrap_or(0))
        )
    } else {
        let power = position - 1;
        let power_sign = if power < 0 { '-' } else { '+' };
        let head = &digits[..1];
        let tail = &digits[1..];
        let fraction = if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        };
        format!("{head}{fraction}e{power_sign}{}", power.abs())
    };
    format!("{sign}{body}")
}

/// Decodes a request body the way `Request.json()` does: the body reader drops one leading byte
/// order mark, the lossy UTF-8 decode drops one more, then `JSON.parse` runs. Observed on the
/// baseline runtime: two leading marks parse, three do not.
#[must_use]
pub fn decode_request_json(body: &[u8]) -> Option<Json> {
    let body = body.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(body);
    let decoded = String::from_utf8_lossy(body);
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
    parse(decoded)
}

enum Frame {
    Array(Vec<Json>),
    Object(Vec<(String, Json)>, Option<String>),
}

/// `JSON.parse`. A lone surrogate escape becomes U+FFFD because Rust strings cannot hold one.
#[must_use]
pub fn parse(text: &str) -> Option<Json> {
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        at: 0,
    };
    parser.run()
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn run(&mut self) -> Option<Json> {
        let mut stack: Vec<Frame> = Vec::new();
        loop {
            let mut value = self.start_value(&mut stack)?;
            // Attach the finished value to its parents until a new value has to be parsed.
            loop {
                let Some(frame) = stack.last_mut() else {
                    self.skip_whitespace();
                    return (self.at == self.bytes.len()).then_some(value);
                };
                match frame {
                    Frame::Array(items) => {
                        items.push(value);
                        self.skip_whitespace();
                        match self.peek()? {
                            b',' => {
                                self.at += 1;
                                break;
                            }
                            b']' => {
                                self.at += 1;
                                let Some(Frame::Array(items)) = stack.pop() else {
                                    return None;
                                };
                                value = Json::Array(items);
                            }
                            _ => return None,
                        }
                    }
                    Frame::Object(fields, key) => {
                        set_property(fields, key.take()?, value);
                        self.skip_whitespace();
                        match self.peek()? {
                            b',' => {
                                self.at += 1;
                                self.skip_whitespace();
                                let next = self.key()?;
                                if let Some(Frame::Object(_, key)) = stack.last_mut() {
                                    *key = Some(next);
                                }
                                break;
                            }
                            b'}' => {
                                self.at += 1;
                                let Some(Frame::Object(fields, _)) = stack.pop() else {
                                    return None;
                                };
                                value = Json::Object(javascript_order(fields));
                            }
                            _ => return None,
                        }
                    }
                }
            }
        }
    }

    /// Parses one value. A container that has content pushes a frame and returns its first child.
    fn start_value(&mut self, stack: &mut Vec<Frame>) -> Option<Json> {
        loop {
            self.skip_whitespace();
            match self.peek()? {
                b'[' => {
                    self.at += 1;
                    self.skip_whitespace();
                    if self.peek()? == b']' {
                        self.at += 1;
                        return Some(Json::Array(Vec::new()));
                    }
                    stack.push(Frame::Array(Vec::new()));
                }
                b'{' => {
                    self.at += 1;
                    self.skip_whitespace();
                    if self.peek()? == b'}' {
                        self.at += 1;
                        return Some(Json::Object(Vec::new()));
                    }
                    let key = self.key()?;
                    stack.push(Frame::Object(Vec::new(), Some(key)));
                }
                b'"' => return self.string().map(Json::String),
                b't' => return self.literal("true", Json::Bool(true)),
                b'f' => return self.literal("false", Json::Bool(false)),
                b'n' => return self.literal("null", Json::Null),
                b'-' | b'0'..=b'9' => return self.number(),
                _ => return None,
            }
        }
    }

    fn key(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        let key = self.string()?;
        self.skip_whitespace();
        if self.peek()? != b':' {
            return None;
        }
        self.at += 1;
        Some(key)
    }

    fn literal(&mut self, word: &str, value: Json) -> Option<Json> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Some(value)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek()? {
            b'0' => self.at += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.digits();
        }
        self.text[start..self.at].parse().ok().map(Json::Number)
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.at += 1;
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let slice = self.text.get(self.at..self.at + 4)?;
        if !slice.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.at += 4;
        u32::from_str_radix(slice, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.at += 1;
            }
            out.push_str(&self.text[start..self.at]);
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = self.peek()?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let unit = self.hex4()?;
                            out.push(self.scalar(unit));
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    }

    /// Joins a high and low surrogate escape pair; anything unpaired becomes U+FFFD.
    fn scalar(&mut self, unit: u32) -> char {
        if (0xd800..0xdc00).contains(&unit) && self.bytes[self.at..].starts_with(b"\\u") {
            let saved = self.at;
            self.at += 2;
            if let Some(low) = self.hex4()
                && (0xdc00..0xe000).contains(&low)
            {
                let code = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                return char::from_u32(code).unwrap_or('\u{fffd}');
            }
            self.at = saved;
        }
        char::from_u32(unit).unwrap_or('\u{fffd}')
    }
}

fn set_property(fields: &mut Vec<(String, Json)>, key: String, value: Json) {
    match fields.iter_mut().find(|(name, _)| *name == key) {
        Some(slot) => slot.1 = value,
        None => fields.push((key, value)),
    }
}

/// Canonical array index keys (`"0"`, `"7"`, up to 2^32 - 2) sort first, ascending.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || key.len() > 10 {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

fn javascript_order(fields: Vec<(String, Json)>) -> Vec<(String, Json)> {
    let (mut indexed, named): (Vec<_>, Vec<_>) = fields
        .into_iter()
        .partition(|(key, _)| array_index(key).is_some());
    indexed.sort_by_key(|(key, _)| array_index(key));
    indexed.extend(named);
    indexed
}

#[cfg(test)]
mod tests {
    use super::{Json, number_text, parse};

    #[test]
    fn numbers_print_like_javascript() {
        for (value, expected) in [
            (1.0, "1"),
            (-0.0, "0"),
            (100.0, "100"),
            (1.5e300, "1.5e+300"),
            (1e21, "1e+21"),
            (1e-7, "1e-7"),
            (0.000_001, "0.000001"),
            (123_456_789_012_345_680_000.0, "123456789012345680000"),
            (5e-324, "5e-324"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-1.5e-10, "-1.5e-10"),
            (f64::INFINITY, "null"),
        ] {
            assert_eq!(number_text(value), expected);
        }
    }

    #[test]
    fn objects_follow_javascript_key_order() {
        let parsed = parse("{\"b\":1,\"2\":2,\"a\":3,\"1\":4,\"b\":5}").expect("parses");
        assert_eq!(parsed.stringify(), "{\"1\":4,\"2\":2,\"b\":5,\"a\":3}");
    }

    #[test]
    fn rejects_what_json_parse_rejects() {
        for text in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "NaN",
            "'a'",
            "{\"a\":\"\u{1}\"}",
            "1 2",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
        assert_eq!(
            parse(" [1, {\"a\": null}] "),
            Some(Json::Array(vec![
                Json::Number(1.0),
                Json::object([("a", Json::Null)]),
            ]))
        );
    }

    #[test]
    fn parsing_is_iterative() {
        // Nesting depth is bounded by the test thread stack only because dropping and printing a
        // value still recurse; parsing itself keeps its containers on the heap.
        let text = format!("{}{}", "[".repeat(1_000), "]".repeat(1_000));
        assert!(parse(&text).is_some());
    }
}
