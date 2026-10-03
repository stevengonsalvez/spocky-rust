//! A WHATWG URL parser that matches node v22.20.0, which runs ada 2.9.2.
//!
//! This is a port of ada's `parse_url_impl` (`src/parser.cpp`), its helpers
//! (`src/helpers.cpp`, `src/unicode.cpp`, `src/checkers.cpp`), and the host
//! steps of `ada::url` (`src/url.cpp`), over the tables of
//! [`crate::url_tables`]. The `url` crate follows a different revision of the
//! standard and a newer IDNA table, and disagrees with node on special-URL
//! slash handling and on many IDNA hosts; this port keeps ada's behavior
//! where it departs from the standard. Hosts go through [`idna::to_ascii`].
//!
//! [`Url::parse`] is `new URL(input, base)`; the getters are the `URL`
//! attributes. Setters and `URLSearchParams` are not ported.

// Narrowing casts (`u64` to `u8`, `u32` to `u16`) and the long state machine
// follow ada's C++ one for one, so the port reads against `parser.cpp`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

pub mod idna;

use std::fmt::Write as _;

use crate::url_tables::{
    C0_CONTROL_SET, FRAGMENT_SET, PATH_SET, QUERY_SET, SPECIAL_QUERY_SET, USERINFO_SET,
};

/// ada's `scheme::type`, in the order of its `is_special_list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Http,
    NotSpecial,
    Https,
    Ws,
    Ftp,
    Wss,
    File,
}

impl Scheme {
    fn of(text: &[u8]) -> Self {
        match text {
            b"http" => Self::Http,
            b"https" => Self::Https,
            b"ws" => Self::Ws,
            b"ftp" => Self::Ftp,
            b"wss" => Self::Wss,
            b"file" => Self::File,
            _ => Self::NotSpecial,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Ws => "ws",
            Self::Ftp => "ftp",
            Self::Wss => "wss",
            Self::File => "file",
            Self::NotSpecial => " ",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Http | Self::Ws => 80,
            Self::Https | Self::Wss => 443,
            Self::Ftp => 21,
            Self::File | Self::NotSpecial => 0,
        }
    }

    fn is_special(self) -> bool {
        self != Self::NotSpecial
    }
}

/// A parsed URL: ada's `ada::url` fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    scheme: Scheme,
    non_special_scheme: Vec<u8>,
    username: Vec<u8>,
    password: Vec<u8>,
    host: Option<Vec<u8>>,
    port: Option<u16>,
    path: Vec<u8>,
    query: Option<Vec<u8>>,
    hash: Option<Vec<u8>>,
    has_opaque_path: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    RelativeScheme,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Port,
    PathStart,
    Path,
    Query,
    OpaquePath,
    File,
    FileSlash,
    FileHost,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn bit_at(set: &[u8; 32], byte: u8) -> bool {
    set[usize::from(byte >> 3)] & (1 << (byte & 7)) != 0
}

fn percent_encode(input: &[u8], set: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for &byte in input {
        if bit_at(set, byte) {
            out.extend_from_slice(format!("%{byte:02X}").as_bytes());
        } else {
            out.push(byte);
        }
    }
    out
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        let hex = |at: usize| {
            input
                .get(at)
                .and_then(|digit| (*digit as char).to_digit(16))
        };
        if byte == b'%'
            && index + 2 < input.len()
            && hex(index + 1).is_some()
            && hex(index + 2).is_some()
        {
            out.push((hex(index + 1).unwrap_or(0) * 16 + hex(index + 2).unwrap_or(0)) as u8);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    out
}

fn is_alpha(byte: u8) -> bool {
    // ada: `(x | 0x20)` in `a..=z`, with `x` a signed `char`.
    byte.is_ascii_alphabetic()
}

fn is_alnum_plus(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

fn is_windows_drive_letter(input: &[u8]) -> bool {
    input.len() >= 2
        && is_alpha(input[0])
        && matches!(input[1], b':' | b'|')
        && (input.len() == 2 || matches!(input[2], b'/' | b'\\' | b'?' | b'#'))
}

fn is_normalized_windows_drive_letter(input: &[u8]) -> bool {
    input.len() >= 2 && is_alpha(input[0]) && input[1] == b':'
}

/// `is_forbidden_host_code_point`.
fn is_forbidden_host_code_point(byte: u8) -> bool {
    matches!(
        byte,
        0 | b'\t'
            | b'\n'
            | b'\r'
            | b' '
            | b'#'
            | b'/'
            | b':'
            | b'<'
            | b'>'
            | b'?'
            | b'@'
            | b'['
            | b'\\'
            | b']'
            | b'^'
            | b'|'
    )
}

/// `is_forbidden_domain_code_point`: the host set, `%`, C0 controls and
/// space, and `0x7F..0xFE` (ada's table stops short of `0xFF`).
fn is_forbidden_domain_code_point(byte: u8) -> bool {
    is_forbidden_host_code_point(byte) || byte == b'%' || byte <= 32 || (127..255).contains(&byte)
}

fn is_ipv4(host: &[u8]) -> bool {
    let mut view = host;
    let mut last = view[view.len() - 1];
    if last == b'.' {
        view = &view[..view.len() - 1];
        if view.is_empty() {
            return false;
        }
        last = view[view.len() - 1];
    }
    if !(last.is_ascii_digit() || (b'a'..=b'f').contains(&last) || last == b'x') {
        return false;
    }
    if let Some(dot) = view.iter().rposition(|byte| *byte == b'.') {
        view = &view[dot + 1..];
    }
    if view.iter().all(u8::is_ascii_digit) {
        return true;
    }
    if view.len() == 1 || !view.starts_with(b"0x") {
        return false;
    }
    view.len() == 2
        || view[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

/// `std::from_chars` into a `uint32_t`: the value and the digits consumed,
/// or `None` for no digits or overflow.
fn from_chars(input: &[u8], radix: u32) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    let mut count = 0;
    for &byte in input {
        let Some(digit) = (byte as char).to_digit(radix) else {
            break;
        };
        value = value.checked_mul(radix)?.checked_add(digit)?;
        count += 1;
    }
    (count > 0).then_some((value, count))
}

/// `url::parse_ipv4`: the serialized host, or `None`.
fn parse_ipv4(host: &[u8]) -> Option<Vec<u8>> {
    let mut input = host;
    if input.last() == Some(&b'.') {
        input = &input[..input.len() - 1];
    }
    let original = input;
    let mut pure_decimal = 0;
    let mut ipv4: u64 = 0;
    let mut digit_count: u64 = 0;
    while digit_count < 4 && !input.is_empty() {
        let hex_prefix = input.len() >= 2 && input[0] == b'0' && matches!(input[1], b'x' | b'X');
        let segment: u32;
        if hex_prefix && (input.len() == 2 || (input.len() > 2 && input[2] == b'.')) {
            segment = 0;
            input = &input[2..];
        } else {
            let parsed = if hex_prefix {
                from_chars(&input[2..], 16).map(|(value, used)| (value, used + 2))
            } else if input.len() >= 2 && input[0] == b'0' && input[1].is_ascii_digit() {
                from_chars(&input[1..], 8).map(|(value, used)| (value, used + 1))
            } else {
                pure_decimal += 1;
                from_chars(input, 10)
            };
            let (value, used) = parsed?;
            segment = value;
            input = &input[used..];
        }
        if input.is_empty() {
            if u64::from(segment) >= 1_u64 << (32 - digit_count * 8) {
                return None;
            }
            ipv4 <<= 32 - digit_count * 8;
            ipv4 |= u64::from(segment);
            return Some(finish_ipv4(pure_decimal, original, ipv4));
        }
        if segment > 255 || input[0] != b'.' {
            return None;
        }
        ipv4 <<= 8;
        ipv4 |= u64::from(segment);
        input = &input[1..];
        digit_count += 1;
    }
    if digit_count != 4 || !input.is_empty() {
        return None;
    }
    Some(finish_ipv4(pure_decimal, original, ipv4))
}

fn finish_ipv4(pure_decimal: i32, original: &[u8], ipv4: u64) -> Vec<u8> {
    if pure_decimal == 4 {
        original.to_vec()
    } else {
        format!(
            "{}.{}.{}.{}",
            (ipv4 >> 24) as u8,
            (ipv4 >> 16) as u8,
            (ipv4 >> 8) as u8,
            ipv4 as u8
        )
        .into_bytes()
    }
}

/// `serializers::ipv6`.
fn serialize_ipv6(address: &[u16; 8]) -> Vec<u8> {
    let mut compress = 0;
    let mut compress_length = 0;
    let mut index = 0;
    while index < 8 {
        if address[index] == 0 {
            let mut next = index + 1;
            while next != 8 && address[next] == 0 {
                next += 1;
            }
            let count = next - index;
            if compress_length < count {
                compress_length = count;
                compress = index;
                if next == 8 {
                    break;
                }
                index = next;
            }
        }
        index += 1;
    }
    if compress_length <= 1 {
        compress = 8;
        compress_length = 8;
    }
    let mut out = String::from("[");
    let mut piece_index = 0;
    loop {
        if piece_index == compress {
            out.push(':');
            if piece_index == 0 {
                out.push(':');
            }
            piece_index += compress_length;
            if piece_index == 8 {
                break;
            }
        }
        write!(out, "{:x}", address[piece_index]).expect("write to a string");
        piece_index += 1;
        if piece_index == 8 {
            break;
        }
        out.push(':');
    }
    out.push(']');
    out.into_bytes()
}

/// `url::parse_ipv6` over the text between the brackets.
fn parse_ipv6(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() {
        return None;
    }
    let mut address = [0_u16; 8];
    let mut piece_index: usize = 0;
    let mut compress: Option<usize> = None;
    let mut pointer = 0;
    if input[0] == b':' {
        if input.len() == 1 || input[1] != b':' {
            return None;
        }
        pointer += 2;
        piece_index += 1;
        compress = Some(piece_index);
    }
    while pointer != input.len() {
        if piece_index == 8 {
            return None;
        }
        if input[pointer] == b':' {
            if compress.is_some() {
                return None;
            }
            pointer += 1;
            piece_index += 1;
            compress = Some(piece_index);
            continue;
        }
        let mut value: u16 = 0;
        let mut length = 0;
        while length < 4 && pointer != input.len() && input[pointer].is_ascii_hexdigit() {
            value = value
                .wrapping_mul(16)
                .wrapping_add((input[pointer] as char).to_digit(16)? as u16);
            pointer += 1;
            length += 1;
        }
        if pointer != input.len() && input[pointer] == b'.' {
            if length == 0 {
                return None;
            }
            pointer -= length;
            if piece_index > 6 {
                return None;
            }
            let mut numbers_seen = 0;
            while pointer != input.len() {
                let mut ipv4_piece: Option<u16> = None;
                if numbers_seen > 0 {
                    if input[pointer] == b'.' && numbers_seen < 4 {
                        pointer += 1;
                    } else {
                        return None;
                    }
                }
                if pointer == input.len() || !input[pointer].is_ascii_digit() {
                    return None;
                }
                while pointer != input.len() && input[pointer].is_ascii_digit() {
                    let number = u16::from(input[pointer] - b'0');
                    match ipv4_piece {
                        None => ipv4_piece = Some(number),
                        Some(0) => return None,
                        Some(previous) => ipv4_piece = Some(previous * 10 + number),
                    }
                    if ipv4_piece > Some(255) {
                        return None;
                    }
                    pointer += 1;
                }
                address[piece_index] = address[piece_index]
                    .wrapping_mul(0x100)
                    .wrapping_add(ipv4_piece.unwrap_or(0));
                numbers_seen += 1;
                if numbers_seen == 2 || numbers_seen == 4 {
                    piece_index += 1;
                }
            }
            if numbers_seen != 4 {
                return None;
            }
            break;
        } else if pointer != input.len() && input[pointer] == b':' {
            pointer += 1;
            if pointer == input.len() {
                return None;
            }
        } else if pointer != input.len() {
            return None;
        }
        address[piece_index] = value;
        piece_index += 1;
    }
    if let Some(start) = compress {
        let mut swaps = piece_index as i32 - start as i32;
        let mut index: usize = 7;
        while index != 0 && swaps > 0 {
            address.swap(index, start + swaps as usize - 1);
            index -= 1;
            swaps -= 1;
        }
    } else if piece_index != 8 {
        return None;
    }
    Some(serialize_ipv6(&address))
}

impl Url {
    fn empty() -> Self {
        Self {
            scheme: Scheme::NotSpecial,
            non_special_scheme: Vec::new(),
            username: Vec::new(),
            password: Vec::new(),
            host: None,
            port: None,
            path: Vec::new(),
            query: None,
            hash: None,
            has_opaque_path: false,
        }
    }

    fn is_special(&self) -> bool {
        self.scheme.is_special()
    }

    fn has_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    fn set_scheme(&mut self, scheme: &[u8]) {
        self.scheme = Scheme::of(scheme);
        if !self.is_special() {
            self.non_special_scheme = scheme.to_vec();
        }
    }

    fn copy_scheme(&mut self, other: &Self) {
        self.non_special_scheme
            .clone_from(&other.non_special_scheme);
        self.scheme = other.scheme;
    }

    /// `url::parse_host`; `false` is a failure.
    fn parse_host(&mut self, input: &[u8]) -> bool {
        if input.is_empty() {
            return false;
        }
        if input[0] == b'[' {
            if input[input.len() - 1] != b']' {
                return false;
            }
            return match parse_ipv6(&input[1..input.len() - 1]) {
                Some(host) => {
                    self.host = Some(host);
                    true
                }
                None => false,
            };
        }
        if !self.is_special() {
            if input.iter().copied().any(is_forbidden_host_code_point) {
                return false;
            }
            self.host = Some(percent_encode(input, &C0_CONTROL_SET));
            return true;
        }
        let lowered = input.to_ascii_lowercase();
        let forbidden = lowered.iter().copied().any(is_forbidden_domain_code_point);
        let host = if !forbidden && !lowered.windows(3).any(|window| window == b"xn-") {
            lowered
        } else {
            let decoded = if input.contains(&b'%') {
                percent_decode(input)
            } else {
                input.to_vec()
            };
            let ascii = idna::to_ascii(&decoded);
            if ascii.is_empty() || ascii.iter().copied().any(is_forbidden_domain_code_point) {
                return false;
            }
            ascii
        };
        if is_ipv4(&host) {
            return match parse_ipv4(&host) {
                Some(address) => {
                    self.host = Some(address);
                    true
                }
                None => false,
            };
        }
        self.host = Some(host);
        true
    }

    /// `url::parse_port(view, true)`: the bytes consumed, `None` on failure.
    fn parse_port(&mut self, view: &[u8]) -> Option<usize> {
        if view.first() == Some(&b'-') {
            return None;
        }
        let digits = view.iter().take_while(|byte| byte.is_ascii_digit()).count();
        let mut parsed: u32 = 0;
        for &byte in &view[..digits] {
            parsed = parsed
                .saturating_mul(10)
                .saturating_add(u32::from(byte - b'0'));
            if parsed > 0xffff {
                return None;
            }
        }
        let consumed = digits;
        if !(consumed == view.len()
            || matches!(view[consumed], b'/' | b'?')
            || (self.is_special() && view[consumed] == b'\\'))
        {
            return None;
        }
        let default_port = u32::from(self.scheme.default_port());
        let is_port_valid = (default_port == 0 && parsed == 0) || default_port != parsed;
        self.port = (digits > 0 && is_port_valid).then_some(parsed as u16);
        Some(consumed)
    }

    fn shorten_path(&mut self) -> bool {
        let path = &mut self.path;
        let first_delimiter = path.iter().skip(1).position(|byte| *byte == b'/');
        if self.scheme == Scheme::File
            && first_delimiter.is_none()
            && !path.is_empty()
            && is_normalized_windows_drive_letter(&path[1..])
        {
            return false;
        }
        if let Some(last) = path.iter().rposition(|byte| *byte == b'/') {
            path.truncate(last);
            return true;
        }
        false
    }

    /// `helpers::parse_prepared_path`.
    fn parse_prepared_path(&mut self, input: &[u8]) {
        const NEED_ENCODING: u8 = 1;
        const BACKSLASH: u8 = 2;
        const DOT: u8 = 4;
        const PERCENT: u8 = 8;
        let signature = |byte: u8| -> u8 {
            if byte <= 0x20
                || matches!(byte, 0x22 | 0x23 | 0x3c | 0x3e | 0x3f | 0x60 | 0x7b | 0x7d)
                || byte > 0x7e
            {
                1
            } else if byte == 0x25 {
                8
            } else if byte == 0x2e {
                4
            } else if byte == 0x5c {
                2
            } else {
                0
            }
        };
        let accumulator = input.iter().fold(0_u8, |all, byte| all | signature(*byte));
        let special = self.is_special();
        let may_need_slow_file_handling =
            self.scheme == Scheme::File && is_windows_drive_letter(input);
        let mut trivial = (if special {
            accumulator == 0
        } else {
            accumulator & (NEED_ENCODING | DOT | PERCENT) == 0
        }) && !may_need_slow_file_handling;
        if accumulator == DOT && !may_need_slow_file_handling && input[0] != b'.' {
            match input.windows(2).position(|pair| pair == b"/.") {
                None => trivial = true,
                Some(slash_dot) => {
                    trivial = !(slash_dot + 2 == input.len()
                        || input[slash_dot + 2] == b'.'
                        || input[slash_dot + 2] == b'/');
                }
            }
        }
        if trivial {
            self.path.push(b'/');
            self.path.extend_from_slice(input);
            return;
        }
        let fast = special
            && accumulator & (NEED_ENCODING | BACKSLASH | PERCENT) == 0
            && self.scheme != Scheme::File;
        if fast {
            let mut previous = 0;
            loop {
                let found = input[previous..]
                    .iter()
                    .position(|byte| *byte == b'/')
                    .map(|at| at + previous);
                let Some(new_location) = found else {
                    let segment = &input[previous..];
                    if segment == b".." {
                        if self.path.is_empty() {
                            self.path = vec![b'/'];
                            return;
                        }
                        if self.path.last() == Some(&b'/') {
                            return;
                        }
                        let keep = self
                            .path
                            .iter()
                            .rposition(|byte| *byte == b'/')
                            .map_or(0, |at| at + 1);
                        self.path.truncate(keep);
                        return;
                    }
                    self.path.push(b'/');
                    if segment != b"." {
                        self.path.extend_from_slice(segment);
                    }
                    return;
                };
                let segment = &input[previous..new_location];
                previous = new_location + 1;
                if segment == b".." {
                    if let Some(last) = self.path.iter().rposition(|byte| *byte == b'/') {
                        self.path.truncate(last);
                    }
                } else if segment != b"." {
                    self.path.push(b'/');
                    self.path.extend_from_slice(segment);
                }
            }
        }
        let needs_percent_encoding = accumulator & 1 == 1;
        let mut rest = input;
        loop {
            let location = if special && accumulator & 2 == 2 {
                rest.iter().position(|byte| matches!(byte, b'/' | b'\\'))
            } else {
                rest.iter().position(|byte| *byte == b'/')
            };
            let mut segment = rest;
            if let Some(at) = location {
                segment = &rest[..at];
                rest = &rest[at + 1..];
            }
            let encoded;
            let mut buffer: &[u8] = if needs_percent_encoding {
                encoded = percent_encode(segment, &PATH_SET);
                &encoded
            } else {
                segment
            };
            let lowered = buffer.to_ascii_lowercase();
            if matches!(lowered.as_slice(), b".." | b".%2e" | b"%2e." | b"%2e%2e") {
                if (self.shorten_path() || special) && location.is_none() {
                    self.path.push(b'/');
                }
            } else if matches!(buffer, b"." | b"%2e" | b"%2E") && location.is_none() {
                self.path.push(b'/');
            } else if !matches!(buffer, b"." | b"%2e" | b"%2E") {
                if self.scheme == Scheme::File
                    && self.path.is_empty()
                    && is_windows_drive_letter(buffer)
                {
                    self.path.push(b'/');
                    self.path.push(buffer[0]);
                    self.path.push(b':');
                    buffer = &buffer[2..];
                    self.path.extend_from_slice(buffer);
                } else {
                    self.path.push(b'/');
                    self.path.extend_from_slice(buffer);
                }
            }
            if location.is_none() {
                return;
            }
        }
    }
}

/// `helpers::get_host_delimiter_location`: where the host ends, and whether a
/// `:` outside brackets ended it.
fn host_delimiter_location(special: bool, view: &[u8]) -> (usize, bool) {
    let is_delimiter =
        |byte: u8| matches!(byte, b':' | b'/' | b'[' | b'?') || (special && byte == b'\\');
    let next = |from: usize| {
        view[from..]
            .iter()
            .position(|byte| is_delimiter(*byte))
            .map_or(view.len(), |at| at + from)
    };
    let mut location = next(0);
    let mut found_colon = false;
    while location < view.len() {
        if view[location] == b'[' {
            if let Some(at) = view[location..].iter().position(|byte| *byte == b']') {
                location += at;
            } else {
                location = view.len();
                break;
            }
            location = next(location);
        } else {
            found_colon = view[location] == b':';
            break;
        }
    }
    (location, found_colon)
}

fn find_authority_delimiter(special: bool, view: &[u8]) -> usize {
    view.iter()
        .position(|byte| matches!(byte, b'@' | b'/' | b'?') || (special && *byte == b'\\'))
        .unwrap_or(view.len())
}

fn trim_c0_whitespace(mut input: &[u8]) -> &[u8] {
    while let Some((first, rest)) = input.split_first() {
        if *first <= b' ' {
            input = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = input.split_last() {
        if *last <= b' ' {
            input = rest;
        } else {
            break;
        }
    }
    input
}

fn substring(data: &[u8], position: usize) -> &[u8] {
    data.get(position..).unwrap_or(&[])
}

/// `parse_url_impl`: `new URL(input, base)`; `None` is a failure.
fn parse_url(user_input: &[u8], base: Option<&Url>) -> Option<Url> {
    let mut url = Url::empty();
    let cleaned: Vec<u8> = user_input
        .iter()
        .copied()
        .filter(|byte| !matches!(byte, b'\t' | b'\n' | b'\r'))
        .collect();
    let trimmed = trim_c0_whitespace(&cleaned);
    let (url_data, fragment) = match trimmed.iter().position(|byte| *byte == b'#') {
        Some(at) => (&trimmed[..at], Some(&trimmed[at + 1..])),
        None => (trimmed, None),
    };
    let encode_fragment = |url: &mut Url| {
        if let Some(fragment) = fragment {
            url.hash = Some(percent_encode(fragment, &FRAGMENT_SET));
        }
    };
    let input_size = url_data.len();
    let mut position = 0;
    let mut state = State::SchemeStart;
    while position <= input_size {
        match state {
            State::SchemeStart => {
                if position != input_size && is_alpha(url_data[position]) {
                    state = State::Scheme;
                    position += 1;
                } else {
                    state = State::NoScheme;
                }
            }
            State::Scheme => {
                while position != input_size && is_alnum_plus(url_data[position]) {
                    position += 1;
                }
                if position != input_size && url_data[position] == b':' {
                    let scheme = &url_data[..position];
                    let lowered = scheme.to_ascii_lowercase();
                    url.set_scheme(&lowered);
                    if url.scheme == Scheme::File {
                        state = State::File;
                    } else if url.is_special() && base.is_some_and(|base| base.scheme == url.scheme)
                    {
                        state = State::SpecialRelativeOrAuthority;
                    } else if url.is_special() {
                        state = State::SpecialAuthoritySlashes;
                    } else if position + 1 < input_size && url_data[position + 1] == b'/' {
                        state = State::PathOrAuthority;
                        position += 1;
                    } else {
                        state = State::OpaquePath;
                    }
                } else {
                    state = State::NoScheme;
                    position = 0;
                    continue;
                }
                position += 1;
            }
            State::NoScheme => {
                let base = base?;
                if base.has_opaque_path && fragment.is_none() {
                    return None;
                }
                if base.has_opaque_path && fragment.is_some() && position == input_size {
                    url.copy_scheme(base);
                    url.has_opaque_path = base.has_opaque_path;
                    url.path.clone_from(&base.path);
                    url.query.clone_from(&base.query);
                    encode_fragment(&mut url);
                    return Some(url);
                }
                state = if base.scheme == Scheme::File {
                    State::File
                } else {
                    State::RelativeScheme
                };
            }
            State::SpecialRelativeOrAuthority => {
                if substring(url_data, position).starts_with(b"//") {
                    state = State::SpecialAuthorityIgnoreSlashes;
                    position += 2;
                } else {
                    state = State::RelativeScheme;
                }
            }
            State::PathOrAuthority => {
                if position != input_size && url_data[position] == b'/' {
                    state = State::Authority;
                    position += 1;
                } else {
                    state = State::Path;
                }
            }
            State::RelativeScheme => {
                let base = base?;
                url.copy_scheme(base);
                if position != input_size
                    && (url_data[position] == b'/'
                        || (url.is_special() && url_data[position] == b'\\'))
                {
                    state = State::RelativeSlash;
                } else {
                    url.username.clone_from(&base.username);
                    url.password.clone_from(&base.password);
                    url.host.clone_from(&base.host);
                    url.port = base.port;
                    url.has_opaque_path = base.has_opaque_path;
                    url.path.clone_from(&base.path);
                    url.query.clone_from(&base.query);
                    if position != input_size && url_data[position] == b'?' {
                        state = State::Query;
                    } else if position != input_size {
                        url.query = None;
                        url.shorten_path();
                        state = State::Path;
                        continue;
                    }
                }
                position += 1;
            }
            State::RelativeSlash => {
                if url.is_special()
                    && position != input_size
                    && matches!(url_data[position], b'/' | b'\\')
                {
                    state = State::SpecialAuthorityIgnoreSlashes;
                } else if position != input_size && url_data[position] == b'/' {
                    state = State::Authority;
                } else {
                    let base = base?;
                    url.username.clone_from(&base.username);
                    url.password.clone_from(&base.password);
                    url.host.clone_from(&base.host);
                    url.port = base.port;
                    state = State::Path;
                    continue;
                }
                position += 1;
            }
            State::SpecialAuthoritySlashes => {
                if substring(url_data, position).starts_with(b"//") {
                    position += 2;
                }
                state = State::SpecialAuthorityIgnoreSlashes;
            }
            State::SpecialAuthorityIgnoreSlashes => {
                while position != input_size && matches!(url_data[position], b'/' | b'\\') {
                    position += 1;
                }
                state = State::Authority;
            }
            State::Authority => {
                if !substring(url_data, position).contains(&b'@') {
                    state = State::Host;
                    continue;
                }
                let mut at_sign_seen = false;
                let mut password_token_seen = false;
                loop {
                    let view = substring(url_data, position);
                    let location = find_authority_delimiter(url.is_special(), view);
                    let authority = &view[..location];
                    let end_of_authority = position + authority.len();
                    if end_of_authority != input_size && url_data[end_of_authority] == b'@' {
                        if at_sign_seen {
                            if password_token_seen {
                                url.password.extend_from_slice(b"%40");
                            } else {
                                url.username.extend_from_slice(b"%40");
                            }
                        }
                        at_sign_seen = true;
                        if password_token_seen {
                            url.password
                                .extend(percent_encode(authority, &USERINFO_SET));
                        } else {
                            let token = authority.iter().position(|byte| *byte == b':');
                            password_token_seen = token.is_some();
                            match token {
                                None => url
                                    .username
                                    .extend(percent_encode(authority, &USERINFO_SET)),
                                Some(at) => {
                                    url.username
                                        .extend(percent_encode(&authority[..at], &USERINFO_SET));
                                    url.password.extend(percent_encode(
                                        &authority[at + 1..],
                                        &USERINFO_SET,
                                    ));
                                }
                            }
                        }
                    } else if end_of_authority == input_size
                        || matches!(url_data[end_of_authority], b'/' | b'?')
                        || (url.is_special() && url_data[end_of_authority] == b'\\')
                    {
                        if at_sign_seen && authority.is_empty() {
                            return None;
                        }
                        state = State::Host;
                        break;
                    }
                    if end_of_authority == input_size {
                        encode_fragment(&mut url);
                        return Some(url);
                    }
                    position = end_of_authority + 1;
                }
            }
            State::Host => {
                let view = substring(url_data, position);
                let (location, found_colon) = host_delimiter_location(url.is_special(), view);
                let host_view = &view[..location];
                position += location;
                if found_colon {
                    if !url.parse_host(host_view) {
                        return None;
                    }
                    state = State::Port;
                    position += 1;
                } else {
                    if url.is_special() && host_view.is_empty() {
                        return None;
                    }
                    if host_view.is_empty() {
                        url.host = Some(Vec::new());
                    } else if !url.parse_host(host_view) {
                        return None;
                    }
                    state = State::PathStart;
                }
            }
            State::OpaquePath => {
                let mut view = substring(url_data, position);
                if let Some(location) = view.iter().position(|byte| *byte == b'?') {
                    view = &view[..location];
                    state = State::Query;
                    position += location + 1;
                } else {
                    position = input_size + 1;
                }
                url.has_opaque_path = true;
                url.path = percent_encode(view, &C0_CONTROL_SET);
            }
            State::Port => {
                let consumed = url.parse_port(substring(url_data, position))?;
                position += consumed;
                state = State::PathStart;
            }
            State::PathStart => {
                if url.is_special() {
                    state = State::Path;
                    if position == input_size {
                        url.path = b"/".to_vec();
                        encode_fragment(&mut url);
                        return Some(url);
                    }
                    if !matches!(url_data[position], b'/' | b'\\') {
                        continue;
                    }
                } else if position != input_size && url_data[position] == b'?' {
                    state = State::Query;
                } else if position != input_size {
                    state = State::Path;
                    if url_data[position] != b'/' {
                        continue;
                    }
                }
                position += 1;
            }
            State::Path => {
                let mut view = substring(url_data, position);
                if let Some(location) = view.iter().position(|byte| *byte == b'?') {
                    state = State::Query;
                    view = &view[..location];
                    position += location + 1;
                } else {
                    position = input_size + 1;
                }
                url.parse_prepared_path(view);
            }
            State::Query => {
                let set = if url.is_special() {
                    &SPECIAL_QUERY_SET
                } else {
                    &QUERY_SET
                };
                url.query = Some(percent_encode(substring(url_data, position), set));
                encode_fragment(&mut url);
                return Some(url);
            }
            State::FileSlash => {
                if position != input_size && matches!(url_data[position], b'/' | b'\\') {
                    state = State::FileHost;
                    position += 1;
                } else {
                    if let Some(base) = base.filter(|base| base.scheme == Scheme::File) {
                        url.host.clone_from(&base.host);
                        if !base.path.is_empty()
                            && !is_windows_drive_letter(substring(url_data, position))
                        {
                            let first = &base.path[1..];
                            let first = first
                                .iter()
                                .position(|byte| *byte == b'/')
                                .map_or(first, |at| &first[..at]);
                            if is_normalized_windows_drive_letter(first) {
                                url.path.push(b'/');
                                url.path.extend_from_slice(first);
                            }
                        }
                    }
                    state = State::Path;
                }
            }
            State::FileHost => {
                let view = substring(url_data, position);
                let end = view
                    .iter()
                    .position(|byte| matches!(byte, b'/' | b'\\' | b'?'))
                    .unwrap_or(view.len());
                let file_host = &view[..end];
                if is_windows_drive_letter(file_host) {
                    state = State::Path;
                } else if file_host.is_empty() {
                    url.host = Some(Vec::new());
                    state = State::PathStart;
                } else {
                    position += file_host.len();
                    if !url.parse_host(file_host) {
                        return None;
                    }
                    if url.host.as_deref() == Some(b"localhost") {
                        url.host = Some(Vec::new());
                    }
                    state = State::PathStart;
                }
            }
            State::File => {
                let file_view = substring(url_data, position);
                url.scheme = Scheme::File;
                url.host = Some(Vec::new());
                if position != input_size && matches!(url_data[position], b'/' | b'\\') {
                    state = State::FileSlash;
                } else if let Some(base) = base.filter(|base| base.scheme == Scheme::File) {
                    url.host.clone_from(&base.host);
                    url.path.clone_from(&base.path);
                    url.query.clone_from(&base.query);
                    url.has_opaque_path = base.has_opaque_path;
                    if position != input_size && url_data[position] == b'?' {
                        state = State::Query;
                    } else if position != input_size {
                        url.query = None;
                        if is_windows_drive_letter(file_view) {
                            url.path.clear();
                            url.has_opaque_path = true;
                        } else {
                            url.shorten_path();
                        }
                        state = State::Path;
                        continue;
                    }
                } else {
                    state = State::Path;
                    continue;
                }
                position += 1;
            }
        }
    }
    encode_fragment(&mut url);
    Some(url)
}

impl Url {
    /// `new URL(input, base)`; `None` where it throws.
    #[must_use]
    pub fn parse(input: &str, base: Option<&str>) -> Option<Self> {
        let base = match base {
            Some(base) => Some(parse_url(base.as_bytes(), None)?),
            None => None,
        };
        parse_url(input.as_bytes(), base.as_ref())
    }

    /// `url.protocol`.
    #[must_use]
    pub fn protocol(&self) -> String {
        if self.is_special() {
            format!("{}:", self.scheme.name())
        } else {
            format!("{}:", text(&self.non_special_scheme))
        }
    }

    /// `url.username`.
    #[must_use]
    pub fn username(&self) -> String {
        text(&self.username)
    }

    /// `url.password`.
    #[must_use]
    pub fn password(&self) -> String {
        text(&self.password)
    }

    /// `url.hostname`.
    #[must_use]
    pub fn hostname(&self) -> String {
        self.host.as_deref().map(text).unwrap_or_default()
    }

    /// `url.port`.
    #[must_use]
    pub fn port(&self) -> String {
        self.port.map(|port| port.to_string()).unwrap_or_default()
    }

    /// `url.host`.
    #[must_use]
    pub fn host(&self) -> String {
        match (&self.host, self.port) {
            (None, _) => String::new(),
            (Some(host), Some(port)) => format!("{}:{port}", text(host)),
            (Some(host), None) => text(host),
        }
    }

    /// `url.pathname`.
    #[must_use]
    pub fn pathname(&self) -> String {
        text(&self.path)
    }

    /// `url.search`.
    #[must_use]
    pub fn search(&self) -> String {
        match &self.query {
            Some(query) if !query.is_empty() => format!("?{}", text(query)),
            _ => String::new(),
        }
    }

    /// `url.hash`.
    #[must_use]
    pub fn hash(&self) -> String {
        match &self.hash {
            Some(hash) if !hash.is_empty() => format!("#{}", text(hash)),
            _ => String::new(),
        }
    }

    /// `url.href`.
    #[must_use]
    pub fn href(&self) -> String {
        let mut output = self.protocol();
        if let Some(host) = &self.host {
            output.push_str("//");
            if self.has_credentials() {
                output.push_str(&text(&self.username));
                if !self.password.is_empty() {
                    output.push(':');
                    output.push_str(&text(&self.password));
                }
                output.push('@');
            }
            output.push_str(&text(host));
            if let Some(port) = self.port {
                output.push(':');
                output.push_str(&port.to_string());
            }
        } else if !self.has_opaque_path && self.path.starts_with(b"//") {
            output.push_str("/.");
        }
        output.push_str(&text(&self.path));
        if let Some(query) = &self.query {
            output.push('?');
            output.push_str(&text(query));
        }
        if let Some(hash) = &self.hash {
            output.push('#');
            output.push_str(&text(hash));
        }
        output
    }

    /// `url.origin`.
    #[must_use]
    pub fn origin(&self) -> String {
        if self.is_special() {
            if self.scheme == Scheme::File {
                return "null".to_owned();
            }
            return format!("{}//{}", self.protocol(), self.host());
        }
        if self.protocol() == "blob:"
            && !self.path.is_empty()
            && let Some(inner) = parse_url(&self.path, None)
            && matches!(inner.scheme, Scheme::Http | Scheme::Https)
        {
            return format!("{}//{}", inner.protocol(), inner.host());
        }
        "null".to_owned()
    }
}
