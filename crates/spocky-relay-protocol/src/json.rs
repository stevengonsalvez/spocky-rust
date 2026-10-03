//! A validating scan with the acceptance rules of Jason 1.4 as the pinned relay uses it.
//!
//! The relay never keeps a decoded document. It only needs to know whether a payload
//! decodes at all and what its first top-level `type` and `key` fields hold. The scan
//! is iterative, so nesting depth costs heap and not stack.

/// What a top-level field holds.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Field {
    #[default]
    Absent,
    /// Present, but not a string.
    Other,
    Text(String),
}

/// The fields the relay reads from a decoded document.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Fields {
    pub type_field: Field,
    pub key_field: Field,
}

#[derive(Clone, Copy)]
enum Capture {
    Type,
    Key,
}

#[derive(Clone, Copy)]
enum Expect {
    Value(Option<Capture>),
    ArrayFirstValueOrEnd,
    ArrayValue,
    ArrayCommaOrEnd,
    ObjectFirstKeyOrEnd { top_level: bool },
    ObjectKey { top_level: bool },
    ObjectCommaOrEnd { top_level: bool },
    DocumentEnd,
}

/// Largest integer literal Jason accepts, in bytes, including a leading minus sign.
const MAXIMUM_INTEGER_BYTES: usize = 1_024;

/// Returns `None` when Jason rejects the whole document. Duplicate object fields keep
/// the first value, as Jason does.
#[must_use]
pub fn scan(payload: &[u8]) -> Option<Fields> {
    let text = std::str::from_utf8(payload).ok()?;
    let mut scanner = Scanner {
        bytes: text.as_bytes(),
        index: 0,
        fields: Fields::default(),
    };
    let mut pending = vec![Expect::DocumentEnd];
    scanner.skip_whitespace();
    if scanner.consume_if(b'{') {
        pending.push(Expect::ObjectFirstKeyOrEnd { top_level: true });
    } else {
        pending.push(Expect::Value(None));
    }
    while let Some(expectation) = pending.pop() {
        scanner.apply(expectation, &mut pending)?;
    }
    Some(scanner.fields)
}

struct Scanner<'a> {
    bytes: &'a [u8],
    index: usize,
    fields: Fields,
}

impl Scanner<'_> {
    fn apply(&mut self, expectation: Expect, pending: &mut Vec<Expect>) -> Option<()> {
        match expectation {
            Expect::Value(capture) => self.parse_value(capture, pending),
            Expect::ArrayFirstValueOrEnd => {
                self.skip_whitespace();
                if !self.consume_if(b']') {
                    pending.push(Expect::ArrayCommaOrEnd);
                    pending.push(Expect::Value(None));
                }
                Some(())
            }
            Expect::ArrayValue => {
                pending.push(Expect::ArrayCommaOrEnd);
                pending.push(Expect::Value(None));
                Some(())
            }
            Expect::ArrayCommaOrEnd => {
                self.skip_whitespace();
                if !self.consume_if(b']') {
                    self.consume(b',')?;
                    pending.push(Expect::ArrayValue);
                }
                Some(())
            }
            Expect::ObjectFirstKeyOrEnd { top_level } => {
                self.skip_whitespace();
                if !self.consume_if(b'}') {
                    self.parse_object_field(top_level, pending)?;
                }
                Some(())
            }
            Expect::ObjectKey { top_level } => {
                self.skip_whitespace();
                self.parse_object_field(top_level, pending)
            }
            Expect::ObjectCommaOrEnd { top_level } => {
                self.skip_whitespace();
                if !self.consume_if(b'}') {
                    self.consume(b',')?;
                    pending.push(Expect::ObjectKey { top_level });
                }
                Some(())
            }
            Expect::DocumentEnd => {
                self.skip_whitespace();
                (self.index == self.bytes.len()).then_some(())
            }
        }
    }

    fn parse_value(&mut self, capture: Option<Capture>, pending: &mut Vec<Expect>) -> Option<()> {
        self.skip_whitespace();
        match self.peek()? {
            b'"' => {
                let value = self.parse_string()?;
                match capture {
                    Some(Capture::Type) => self.fields.type_field = Field::Text(value),
                    Some(Capture::Key) => self.fields.key_field = Field::Text(value),
                    None => {}
                }
                Some(())
            }
            b'{' => {
                self.index += 1;
                pending.push(Expect::ObjectFirstKeyOrEnd { top_level: false });
                Some(())
            }
            b'[' => {
                self.index += 1;
                pending.push(Expect::ArrayFirstValueOrEnd);
                Some(())
            }
            b't' => self.parse_literal(b"true"),
            b'f' => self.parse_literal(b"false"),
            b'n' => self.parse_literal(b"null"),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_object_field(&mut self, top_level: bool, pending: &mut Vec<Expect>) -> Option<()> {
        let name = self.parse_string()?;
        self.skip_whitespace();
        self.consume(b':')?;
        let capture = if top_level {
            match name.as_str() {
                "type" if self.fields.type_field == Field::Absent => {
                    self.fields.type_field = Field::Other;
                    Some(Capture::Type)
                }
                "key" if self.fields.key_field == Field::Absent => {
                    self.fields.key_field = Field::Other;
                    Some(Capture::Key)
                }
                _ => None,
            }
        } else {
            None
        };
        pending.push(Expect::ObjectCommaOrEnd { top_level });
        pending.push(Expect::Value(capture));
        Some(())
    }

    fn parse_string(&mut self) -> Option<String> {
        self.consume(b'"')?;
        let start = self.index;
        let end = string_end(self.bytes, start)?;
        let decoded = std::str::from_utf8(&self.bytes[start..end])
            .ok()
            .and_then(decode_string)?;
        self.index = end + 1;
        Some(decoded)
    }

    fn parse_number(&mut self) -> Option<()> {
        let start = self.index;
        self.consume_if(b'-');
        match self.peek()? {
            b'0' => self.index += 1,
            b'1'..=b'9' => {
                self.index += 1;
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.index += 1;
                }
            }
            _ => return None,
        }
        if self.consume_if(b'.') {
            self.consume_digits()?;
        }
        if self.peek().is_some_and(|byte| matches!(byte, b'e' | b'E')) {
            self.index += 1;
            if self.peek().is_some_and(|byte| matches!(byte, b'+' | b'-')) {
                self.index += 1;
            }
            self.consume_digits()?;
        }
        let number = std::str::from_utf8(&self.bytes[start..self.index]).ok()?;
        if number
            .bytes()
            .any(|byte| matches!(byte, b'.' | b'e' | b'E'))
        {
            if !number.parse::<f64>().ok()?.is_finite() {
                return None;
            }
        } else if number.len() > MAXIMUM_INTEGER_BYTES {
            return None;
        }
        Some(())
    }

    fn consume_digits(&mut self) -> Option<()> {
        let start = self.index;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.index += 1;
        }
        (self.index > start).then_some(())
    }

    fn parse_literal(&mut self, literal: &[u8]) -> Option<()> {
        (self.bytes.get(self.index..self.index + literal.len())? == literal).then(|| {
            self.index += literal.len();
        })
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.index += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> Option<()> {
        self.consume_if(expected).then_some(())
    }

    fn consume_if(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }
}

fn string_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut escaped = false;
    while let Some(byte) = bytes.get(index) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn decode_string(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x00..=0x1f => return None,
            b'\\' => {
                index += 1;
                match *bytes.get(index)? {
                    b'"' => decoded.push(b'"'),
                    b'\\' => decoded.push(b'\\'),
                    b'/' => decoded.push(b'/'),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    b'n' => decoded.push(b'\n'),
                    b'r' => decoded.push(b'\r'),
                    b't' => decoded.push(b'\t'),
                    b'u' => {
                        let high = hex_quad(bytes, index + 1)?;
                        index += 4;
                        let scalar = if (0xd800..=0xdbff).contains(&high) {
                            if bytes.get(index + 1..index + 3) != Some(b"\\u") {
                                return None;
                            }
                            let low = hex_quad(bytes, index + 3)?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return None;
                            }
                            index += 6;
                            0x1_0000 + (u32::from(high - 0xd800) << 10) + u32::from(low - 0xdc00)
                        } else if (0xdc00..=0xdfff).contains(&high) {
                            return None;
                        } else {
                            u32::from(high)
                        };
                        let character = char::from_u32(scalar)?;
                        let mut buffer = [0_u8; 4];
                        decoded.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                    }
                    _ => return None,
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

fn hex_quad(bytes: &[u8], start: usize) -> Option<u16> {
    let digits = bytes.get(start..start + 4)?;
    digits.iter().try_fold(0_u16, |value, digit| {
        Some((value << 4) | u16::try_from(char::from(*digit).to_digit(16)?).ok()?)
    })
}
