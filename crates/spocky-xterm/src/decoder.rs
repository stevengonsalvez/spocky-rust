//! PTY byte decoding: a port of Node's `StringDecoder` for `utf8`.
//!
//! Paseo reads PTY output through node-pty's `socket.setEncoding("utf8")`,
//! which runs each chunk through a `StringDecoder` before `onData`. This port
//! follows Node v22.20.0 `src/string_decoder.cc` (`DecodeData`): bytes of a
//! character cut off at a chunk end are held back until the next chunk, a
//! held character that meets a non-continuation byte is decoded as is, and
//! every decode replaces invalid sequences with U+FFFD per maximal subpart,
//! as V8's `String::NewFromUtf8` does.

/// Streaming UTF-8 decoder matching Node's `StringDecoder("utf8").write`.
#[derive(Debug, Default, Clone)]
pub struct Utf8Decoder {
    incomplete: [u8; 4],
    buffered: usize,
    missing: usize,
}

impl Utf8Decoder {
    /// Creates a decoder with no held bytes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes one chunk, holding back a trailing incomplete character.
    #[must_use]
    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut data = chunk;
        let mut prepend: Option<String> = None;
        if self.missing > 0 {
            let limit = data.len().min(self.missing);
            if let Some(stop) = data[..limit].iter().position(|byte| byte & 0xC0 != 0x80) {
                // Not a continuation byte: decode the held bytes plus the
                // continuation bytes before it, and start over at this byte.
                self.missing = 0;
                self.incomplete[self.buffered..self.buffered + stop].copy_from_slice(&data[..stop]);
                self.buffered += stop;
                data = &data[stop..];
            }
            let found = data.len().min(self.missing);
            self.incomplete[self.buffered..self.buffered + found].copy_from_slice(&data[..found]);
            data = &data[found..];
            self.missing -= found;
            self.buffered += found;
            if self.missing == 0 {
                prepend =
                    Some(String::from_utf8_lossy(&self.incomplete[..self.buffered]).into_owned());
                self.buffered = 0;
            }
        }
        if data.is_empty() {
            return prepend.unwrap_or_default();
        }
        if data[data.len() - 1] & 0x80 != 0 {
            self.find_incomplete_tail(data);
        }
        let mut body_len = data.len();
        if self.buffered > 0 {
            body_len -= self.buffered;
            self.incomplete[..self.buffered].copy_from_slice(&data[body_len..]);
        }
        let body = String::from_utf8_lossy(&data[..body_len]);
        match prepend {
            Some(mut text) => {
                text.push_str(&body);
                text
            }
            None => body.into_owned(),
        }
    }

    /// Sets `buffered` and `missing` for a chunk ending on a non-ASCII byte.
    fn find_incomplete_tail(&mut self, data: &[u8]) {
        let mut index = data.len() - 1;
        loop {
            self.buffered += 1;
            let byte = data[index];
            if byte & 0xC0 == 0x80 {
                if self.buffered >= 4 || index == 0 {
                    self.buffered = 0;
                    return;
                }
            } else {
                self.missing = if byte & 0xE0 == 0xC0 {
                    2
                } else if byte & 0xF0 == 0xE0 {
                    3
                } else if byte & 0xF8 == 0xF0 {
                    4
                } else {
                    self.buffered = 0;
                    return;
                };
                if self.buffered >= self.missing {
                    self.missing = 0;
                    self.buffered = 0;
                }
                self.missing -= self.buffered;
                return;
            }
            index -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Utf8Decoder;

    fn decode(chunks: &[&[u8]]) -> Vec<String> {
        let mut decoder = Utf8Decoder::new();
        chunks.iter().map(|chunk| decoder.write(chunk)).collect()
    }

    #[test]
    fn holds_split_characters_until_complete() {
        assert_eq!(
            decode(&[b"\xe4\xb8", b"\xad", b"\xe6", b"\x96\x87_"]),
            ["", "\u{4e2d}", "", "\u{6587}_"]
        );
        assert_eq!(
            decode(&[b"\xf0\x9f", b"\x98", b"\x80\r\n"]),
            ["", "", "\u{1f600}\r\n"]
        );
    }

    #[test]
    fn replaces_invalid_and_interrupted_sequences() {
        assert_eq!(
            decode(&[b"A\xffB\xc3", b"(C\r\n"]),
            ["A\u{fffd}B", "\u{fffd}(C\r\n"]
        );
        assert_eq!(
            decode(&[b"\xed", b"\xa0\x80", b"D"]),
            ["", "\u{fffd}\u{fffd}\u{fffd}", "D"]
        );
    }

    #[test]
    fn decodes_lone_trailing_bytes_without_holding() {
        assert_eq!(decode(&[b"\x80\x80"]), ["\u{fffd}\u{fffd}"]);
        assert_eq!(decode(&[b"\xf8"]), ["\u{fffd}"]);
    }
}
