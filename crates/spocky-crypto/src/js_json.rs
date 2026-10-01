//! `JSON.parse` acceptance and the shallow value shape the relay handshake
//! inspects.
//!
//! The pinned channel calls `JSON.parse` on handshake and stray text frames
//! and then reads at most two levels of the result (`type`, `key`, and
//! `capabilities.binaryCiphertext`). This parser accepts exactly the
//! ECMA-404 grammar `JSON.parse` accepts, without a nesting limit, because
//! V8 parses iteratively. String values keep their UTF-16 code units, so
//! escaped lone surrogates survive. Objects record their members only at
//! the top level and one level below it; deeper values are validated and
//! then reduced to their kind.

use crate::js_string::JsString;

/// The kind and, where the handshake reads it, the content of a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number,
    String(JsString),
    Array,
    /// Members in source order, duplicates included, or `None` when the
    /// object lies deeper than the recorded levels.
    Object(Option<Vec<(JsString, JsonValue)>>),
}

impl JsonValue {
    /// Returns true for a non-null, non-array object (`isRecord`).
    #[must_use]
    pub const fn is_record(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// Reads a member of a recorded object. A repeated key resolves to its
    /// last occurrence, as `JSON.parse` keeps the last one. `None` means the
    /// member is absent (`undefined`).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        let Self::Object(Some(members)) = self else {
            return None;
        };
        members
            .iter()
            .rev()
            .find(|(name, _)| name.iter().copied().eq(key.encode_utf16()))
            .map(|(_, value)| value)
    }
}

/// Levels of object nesting whose members are recorded.
const RECORDED_OBJECT_LEVELS: usize = 2;

enum Frame {
    Array,
    Object {
        members: Option<Vec<(JsString, JsonValue)>>,
        key: JsString,
    },
}

/// Parses text exactly as `JSON.parse` accepts it. `None` is the
/// `SyntaxError` case.
#[must_use]
pub fn parse(text: &str) -> Option<JsonValue> {
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        position: 0,
    };
    let mut stack: Vec<Frame> = Vec::new();
    parser.skip_whitespace();
    loop {
        let mut value = match parser.peek()? {
            b'{' => {
                parser.position += 1;
                parser.skip_whitespace();
                let members = (stack.len() < RECORDED_OBJECT_LEVELS).then(Vec::new);
                if parser.peek() == Some(b'}') {
                    parser.position += 1;
                    JsonValue::Object(members)
                } else {
                    let key = parser.member_key()?;
                    stack.push(Frame::Object { members, key });
                    continue;
                }
            }
            b'[' => {
                parser.position += 1;
                parser.skip_whitespace();
                if parser.peek() == Some(b']') {
                    parser.position += 1;
                    JsonValue::Array
                } else {
                    stack.push(Frame::Array);
                    continue;
                }
            }
            b'"' => JsonValue::String(parser.string()?),
            b't' => parser.literal("true", JsonValue::Bool(true))?,
            b'f' => parser.literal("false", JsonValue::Bool(false))?,
            b'n' => parser.literal("null", JsonValue::Null)?,
            b'-' | b'0'..=b'9' => parser.number()?,
            _ => return None,
        };
        // Attach the completed value to its parents, closing finished
        // containers, until one expects another element.
        loop {
            parser.skip_whitespace();
            match stack.last_mut() {
                None => return (parser.position == parser.bytes.len()).then_some(value),
                Some(Frame::Array) => match parser.peek()? {
                    b',' => {
                        parser.position += 1;
                        parser.skip_whitespace();
                        break;
                    }
                    b']' => {
                        parser.position += 1;
                        stack.pop();
                        value = JsonValue::Array;
                    }
                    _ => return None,
                },
                Some(Frame::Object { members, key }) => {
                    if let Some(members) = members {
                        members.push((std::mem::take(key), value));
                    }
                    match parser.peek()? {
                        b',' => {
                            parser.position += 1;
                            parser.skip_whitespace();
                            *key = parser.member_key()?;
                            break;
                        }
                        b'}' => {
                            parser.position += 1;
                            let Some(Frame::Object { members, .. }) = stack.pop() else {
                                return None;
                            };
                            value = JsonValue::Object(members);
                        }
                        _ => return None,
                    }
                }
            }
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    /// Parses `"key"` `:` and leaves the position at the member value.
    fn member_key(&mut self) -> Option<JsString> {
        if self.peek()? != b'"' {
            return None;
        }
        let key = self.string()?;
        self.skip_whitespace();
        if self.peek()? != b':' {
            return None;
        }
        self.position += 1;
        self.skip_whitespace();
        Some(key)
    }

    fn literal(&mut self, word: &str, value: JsonValue) -> Option<JsonValue> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Some(value)
        } else {
            None
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        self.position - start
    }

    fn number(&mut self) -> Option<JsonValue> {
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek()? {
            b'0' => self.position += 1,
            b'1'..=b'9' => {
                self.digits();
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if self.digits() == 0 {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        Some(JsonValue::Number)
    }

    fn string(&mut self) -> Option<JsString> {
        self.position += 1;
        let mut units = JsString::new();
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    self.position += 1;
                    return Some(units);
                }
                b'\\' => {
                    self.position += 1;
                    let escaped = self.peek()?;
                    self.position += 1;
                    let unit = match escaped {
                        b'"' => 0x22,
                        b'\\' => 0x5c,
                        b'/' => 0x2f,
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => 0x0a,
                        b'r' => 0x0d,
                        b't' => 0x09,
                        b'u' => self.hex_unit()?,
                        _ => return None,
                    };
                    units.push(unit);
                }
                0x00..=0x1f => return None,
                0x20..=0x7f => {
                    units.push(u16::from(byte));
                    self.position += 1;
                }
                _ => {
                    let character = self.text[self.position..].chars().next()?;
                    let mut buffer = [0_u16; 2];
                    units.extend_from_slice(character.encode_utf16(&mut buffer));
                    self.position += character.len_utf8();
                }
            }
        }
    }

    fn hex_unit(&mut self) -> Option<u16> {
        let digits = self.bytes.get(self.position..self.position + 4)?;
        let mut unit = 0_u16;
        for digit in digits {
            let value = char::from(*digit).to_digit(16)?;
            unit = (unit << 4) | u16::try_from(value).ok()?;
        }
        self.position += 4;
        Some(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js_string::utf16;

    #[test]
    fn accepts_exactly_the_json_parse_grammar() {
        for valid in [
            "0",
            "-0",
            "-1.5e+3",
            "1E9",
            " \t\r\n[ ] ",
            "{}",
            "\"\\u00e9\\/\"",
            "[1,{\"a\":[null,true,false]}]",
            "\"é😀\"",
        ] {
            assert!(parse(valid).is_some(), "{valid}");
        }
        for invalid in [
            "",
            " ",
            "01",
            "1.",
            ".5",
            "+1",
            "1e",
            "-",
            "[1,]",
            "{\"a\":1,}",
            "{'a':1}",
            "\"\t\"",
            "\"\\x\"",
            "\"\\u12g4\"",
            "\u{feff}{}",
            "{} x",
            "nul",
            "\u{a0}1",
            "[1 2]",
            "{\"a\" 1}",
            "{1:2}",
        ] {
            assert!(parse(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn keeps_two_object_levels_and_the_last_duplicate() {
        let value = parse(
            r#"{"type":"first","type":"e2ee_hello","key":"\ud800","capabilities":{"binaryCiphertext":true,"nested":{"a":1}}}"#,
        )
        .unwrap();
        assert_eq!(
            value.get("type"),
            Some(&JsonValue::String(utf16("e2ee_hello")))
        );
        assert_eq!(value.get("key"), Some(&JsonValue::String(vec![0xd800])));
        let capabilities = value.get("capabilities").unwrap();
        assert_eq!(
            capabilities.get("binaryCiphertext"),
            Some(&JsonValue::Bool(true))
        );
        assert_eq!(capabilities.get("nested"), Some(&JsonValue::Object(None)));
        assert_eq!(value.get("missing"), None);
        assert!(!JsonValue::Array.is_record());
        assert!(!JsonValue::Null.is_record());
    }

    #[test]
    fn nesting_has_no_limit() {
        let depth = 200_000;
        let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(parse(&text), Some(JsonValue::Array));
        let objects = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        assert!(parse(&objects).unwrap().is_record());
        assert!(parse(&"[".repeat(depth)).is_none());
    }
}
