//! Node's `StringDecoder("utf8")`, which node-pty applies to PTY output with
//! `socket.setEncoding("utf8")` before `onData`. A character split across
//! reads is held back until it completes; malformed bytes become U+FFFD with
//! the same maximal-subpart rule as V8's decoder.
//!
//! Follows `src/string_decoder.cc` of Node v22.20.0: buffered lead bytes are
//! completed from the next chunk, a non-continuation byte ends the held
//! character early, and the tail of each chunk is scanned back for a lead
//! byte whose character it cuts off.

/// Incremental UTF-8 decoder with Node `StringDecoder` chunk semantics.
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    buffered: Vec<u8>,
    missing: usize,
}

impl Utf8Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `decoder.write(chunk)`.
    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut data = chunk;
        let mut prepend: Option<String> = None;
        if self.missing > 0 {
            let limit = data.len().min(self.missing);
            if let Some(stop) = data[..limit].iter().position(|byte| byte & 0xC0 != 0x80) {
                // The held character ends early; the unexpected byte starts a
                // new one.
                self.missing = 0;
                self.buffered.extend_from_slice(&data[..stop]);
                data = &data[stop..];
            }
            let found = data.len().min(self.missing);
            self.buffered.extend_from_slice(&data[..found]);
            data = &data[found..];
            self.missing -= found;
            if self.missing == 0 {
                prepend = Some(String::from_utf8_lossy(&self.buffered).into_owned());
                self.buffered.clear();
            }
        }
        if data.is_empty() {
            return prepend.unwrap_or_default();
        }

        let held = Self::held_tail(data, &mut self.missing);
        let (body, tail) = data.split_at(data.len() - held);
        self.buffered.extend_from_slice(tail);
        let body = String::from_utf8_lossy(body);
        match prepend {
            Some(mut text) => {
                text.push_str(&body);
                text
            }
            None => body.into_owned(),
        }
    }

    /// `decoder.end()`: the held bytes as one malformed character, if any.
    pub fn end(&mut self) -> String {
        self.missing = 0;
        if self.buffered.is_empty() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&self.buffered).into_owned();
        self.buffered.clear();
        text
    }

    /// How many trailing bytes of `data` start a character the chunk cuts
    /// off, setting `missing` to the bytes still needed.
    fn held_tail(data: &[u8], missing: &mut usize) -> usize {
        if data[data.len() - 1] & 0x80 == 0 {
            return 0;
        }
        let mut held = 0;
        for index in (0..data.len()).rev() {
            held += 1;
            let byte = data[index];
            if byte & 0xC0 == 0x80 {
                if held >= 4 || index == 0 {
                    return 0;
                }
                continue;
            }
            let length = if byte & 0xE0 == 0xC0 {
                2
            } else if byte & 0xF0 == 0xE0 {
                3
            } else if byte & 0xF8 == 0xF0 {
                4
            } else {
                return 0;
            };
            if held >= length {
                return 0;
            }
            *missing = length - held;
            return held;
        }
        0
    }
}
