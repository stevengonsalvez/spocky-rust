//! `JSON.parse` as a value tree, for the control messages the relay sends the daemon.
//!
//! Accepts exactly the ECMA-404 grammar `JSON.parse` accepts, without a nesting limit
//! (V8 parses iteratively). Strings keep their UTF-16 code units, so escaped lone
//! surrogates survive. A repeated object key resolves to its last occurrence.

use spocky_crypto::js_string::JsString;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number,
    String(JsString),
    Array(Vec<Value>),
    Object(Vec<(JsString, Value)>),
}

impl Drop for Value {
    /// Dropping a deeply nested value must not recurse: V8 parses 100,000 levels, so a
    /// hostile control frame can build them.
    fn drop(&mut self) {
        let mut pending: Vec<Value> = Vec::new();
        take_children(self, &mut pending);
        while let Some(mut value) = pending.pop() {
            take_children(&mut value, &mut pending);
        }
    }
}

fn take_children(value: &mut Value, into: &mut Vec<Value>) {
    match value {
        Value::Array(elements) => into.append(elements),
        Value::Object(members) => into.extend(members.drain(..).map(|(_, member)| member)),
        _ => {}
    }
}

impl Value {
    /// `parsed[key]` on an object: the last member with that name.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        let Self::Object(members) = self else {
            return None;
        };
        members
            .iter()
            .rev()
            .find(|(name, _)| name.iter().copied().eq(key.encode_utf16()))
            .map(|(_, value)| value)
    }

    /// `typeof value === "object" && value !== null`: objects and arrays.
    #[must_use]
    pub const fn is_record(&self) -> bool {
        matches!(self, Self::Object(_) | Self::Array(_))
    }
}

enum Frame {
    Array(Vec<Value>),
    Object {
        members: Vec<(JsString, Value)>,
        key: JsString,
    },
}

/// Parses text as `JSON.parse` does. `None` is the `SyntaxError` case.
#[must_use]
pub fn parse(text: &str) -> Option<Value> {
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
                if parser.peek() == Some(b'}') {
                    parser.position += 1;
                    Value::Object(Vec::new())
                } else {
                    let key = parser.member_key()?;
                    stack.push(Frame::Object {
                        members: Vec::new(),
                        key,
                    });
                    continue;
                }
            }
            b'[' => {
                parser.position += 1;
                parser.skip_whitespace();
                if parser.peek() == Some(b']') {
                    parser.position += 1;
                    Value::Array(Vec::new())
                } else {
                    stack.push(Frame::Array(Vec::new()));
                    continue;
                }
            }
            b'"' => Value::String(parser.string()?),
            b't' => parser.literal("true", Value::Bool(true))?,
            b'f' => parser.literal("false", Value::Bool(false))?,
            b'n' => parser.literal("null", Value::Null)?,
            b'-' | b'0'..=b'9' => parser.number()?,
            _ => return None,
        };
        // Attach the completed value to its parents, closing finished containers, until
        // one expects another element.
        loop {
            parser.skip_whitespace();
            match stack.last_mut() {
                None => return (parser.position == parser.bytes.len()).then_some(value),
                Some(Frame::Array(elements)) => {
                    elements.push(value);
                    match parser.peek()? {
                        b',' => {
                            parser.position += 1;
                            parser.skip_whitespace();
                            break;
                        }
                        b']' => {
                            parser.position += 1;
                            let Some(Frame::Array(elements)) = stack.pop() else {
                                return None;
                            };
                            value = Value::Array(elements);
                        }
                        _ => return None,
                    }
                }
                Some(Frame::Object { members, key }) => {
                    members.push((std::mem::take(key), value));
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
                            value = Value::Object(members);
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

    fn literal(&mut self, word: &str, value: Value) -> Option<Value> {
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

    fn number(&mut self) -> Option<Value> {
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
        Some(Value::Number)
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
