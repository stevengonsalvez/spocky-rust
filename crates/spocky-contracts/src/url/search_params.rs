//! `URLSearchParams` as node v22.20.0 implements it: the
//! `application/x-www-form-urlencoded` list of name/value pairs behind
//! `url.searchParams`, for the operations Paseo uses (`get`, `getAll`, `has`,
//! `keys`, `set`, plus `append`, `delete`, and `toString`).
//!
//! Names and values are strings of Unicode scalar values (a USVString): the
//! caller passes a lone surrogate as U+FFFD, as `URLSearchParams` does.

use std::fmt;

/// An `application/x-www-form-urlencoded` list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UrlSearchParams {
    list: Vec<(String, String)>,
}

/// The byte serializer: `*-._` and ASCII alphanumerics stay, space is `+`,
/// every other byte is `%XX` in uppercase.
fn serialize(input: &str, out: &mut String) {
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
        }
    }
}

/// `+` as space, then percent-decoding, then lossy UTF-8.
fn parse_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let hex = |at: usize| {
            bytes
                .get(at)
                .and_then(|digit| (*digit as char).to_digit(16))
        };
        if byte == b'%'
            && index + 2 < bytes.len()
            && hex(index + 1).is_some()
            && hex(index + 2).is_some()
        {
            decoded.push((hex(index + 1).unwrap_or(0) * 16 + hex(index + 2).unwrap_or(0)) as u8);
            index += 3;
        } else {
            decoded.push(if byte == b'+' { b' ' } else { byte });
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

impl UrlSearchParams {
    /// `new URLSearchParams()`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `new URLSearchParams(query)`: a leading `?` is dropped.
    #[must_use]
    pub fn parse(query: &str) -> Self {
        let query = query.strip_prefix('?').unwrap_or(query);
        let list = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) => (parse_component(name), parse_component(value)),
                None => (parse_component(pair), String::new()),
            })
            .collect();
        Self { list }
    }

    /// `new URLSearchParams([[name, value], ...])` (or a record, in key order).
    #[must_use]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            list: pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    /// `params.append(name, value)`.
    pub fn append(&mut self, name: &str, value: &str) {
        self.list.push((name.to_owned(), value.to_owned()));
    }

    /// `params.delete(name[, value])`.
    pub fn delete(&mut self, name: &str, value: Option<&str>) {
        self.list.retain(|(key, current)| {
            !(key == name && value.is_none_or(|wanted| wanted == current))
        });
    }

    /// `params.get(name)`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.list
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `params.getAll(name)`.
    #[must_use]
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.list
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// `params.has(name[, value])`.
    #[must_use]
    pub fn has(&self, name: &str, value: Option<&str>) -> bool {
        self.list
            .iter()
            .any(|(key, current)| key == name && value.is_none_or(|wanted| wanted == current))
    }

    /// `params.set(name, value)`: the first match takes the value, the rest go.
    pub fn set(&mut self, name: &str, value: &str) {
        match self.list.iter().position(|(key, _)| key == name) {
            Some(first) => {
                value.clone_into(&mut self.list[first].1);
                let mut index = first + 1;
                while index < self.list.len() {
                    if self.list[index].0 == name {
                        self.list.remove(index);
                    } else {
                        index += 1;
                    }
                }
            }
            None => self.append(name, value),
        }
    }

    /// `[...params.keys()]`.
    #[must_use]
    pub fn keys(&self) -> Vec<&str> {
        self.list.iter().map(|(key, _)| key.as_str()).collect()
    }
}

impl fmt::Display for UrlSearchParams {
    /// `params.toString()`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        for (index, (name, value)) in self.list.iter().enumerate() {
            if index > 0 {
                out.push('&');
            }
            serialize(name, &mut out);
            out.push('=');
            serialize(value, &mut out);
        }
        formatter.write_str(&out)
    }
}
