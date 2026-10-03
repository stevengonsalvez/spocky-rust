//! `PaseoRelay.HandshakeValidation.check/2`: the only place the relay looks inside a
//! client frame. A frame is a handshake when it decodes as a JSON object whose first
//! `type` is `"hello"` or `"e2ee_hello"`; everything else stays opaque.

use crate::json::{Field, scan};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeType {
    Hello,
    E2eeHello,
}

impl HandshakeType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::E2eeHello => "e2ee_hello",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Handshake {
    NotHandshake,
    Accept(HandshakeType),
    Reject(HandshakeType),
}

/// X25519 encodings the relay refuses: zero, one, the two points of order eight,
/// and the field prime minus one, the prime, and the prime plus one.
const UNSUPPORTED_PUBLIC_KEYS: [[u8; 32]; 7] = [
    [0; 32],
    unsupported(&[(0, 0x01)]),
    hex32("E0EB7A7C3B41B8AE1656E3FAF19FC46ADA098DEB9C32B1FD866205165F49B800"),
    hex32("5F9C95BCA3508C24B1D0B1559C83EF5B04445CC4581C8E86D8224EDDD09F1157"),
    prime_relative(0xec),
    prime_relative(0xed),
    prime_relative(0xee),
];

const fn unsupported(bytes: &[(usize, u8)]) -> [u8; 32] {
    let mut key = [0; 32];
    let mut index = 0;
    while index < bytes.len() {
        key[bytes[index].0] = bytes[index].1;
        index += 1;
    }
    key
}

const fn prime_relative(low: u8) -> [u8; 32] {
    let mut key = [0xff; 32];
    key[0] = low;
    key[31] = 0x7f;
    key
}

const fn hex32(encoded: &str) -> [u8; 32] {
    let digits = encoded.as_bytes();
    let mut key = [0; 32];
    let mut index = 0;
    while index < 32 {
        key[index] = (nibble(digits[index * 2]) << 4) | nibble(digits[index * 2 + 1]);
        index += 1;
    }
    key
}

const fn nibble(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'A'..=b'F' => digit - b'A' + 10,
        _ => panic!("hexadecimal digit expected"),
    }
}

/// Classifies one client payload. Text and binary frames are treated alike.
#[must_use]
pub fn check(payload: &[u8]) -> Handshake {
    let Some(fields) = scan(payload) else {
        return Handshake::NotHandshake;
    };
    let handshake_type = match &fields.type_field {
        Field::Text(text) if text == "hello" => HandshakeType::Hello,
        Field::Text(text) if text == "e2ee_hello" => HandshakeType::E2eeHello,
        _ => return Handshake::NotHandshake,
    };
    match &fields.key_field {
        Field::Text(encoded) if valid_public_key(encoded) => Handshake::Accept(handshake_type),
        _ => Handshake::Reject(handshake_type),
    }
}

fn valid_public_key(encoded: &str) -> bool {
    let Some(key) = decode_canonical_base64(encoded) else {
        return false;
    };
    canonical_coordinate(&key) && !UNSUPPORTED_PUBLIC_KEYS.contains(&key)
}

/// Strict padded Base64 of exactly 32 bytes that re-encodes to the same text.
fn decode_canonical_base64(encoded: &str) -> Option<[u8; 32]> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = encoded.as_bytes();
    if bytes.len() != 44 || bytes[43] != b'=' {
        return None;
    }
    let mut key = [0_u8; 32];
    let mut written = 0;
    for chunk in bytes[..43].chunks(4) {
        let mut accumulator = 0_u32;
        for symbol in chunk {
            let value = ALPHABET.iter().position(|candidate| candidate == symbol)?;
            accumulator = (accumulator << 6) | u32::try_from(value).ok()?;
        }
        let triple = if chunk.len() == 4 {
            accumulator
        } else {
            // Three symbols carry two bytes; the two unused low bits must be zero.
            if accumulator & 0b11 != 0 {
                return None;
            }
            accumulator << 6
        };
        for shift in [16, 8, 0] {
            if chunk.len() < 4 && shift == 0 {
                break;
            }
            *key.get_mut(written)? = u8::try_from((triple >> shift) & 0xff).ok()?;
            written += 1;
        }
    }
    (written == 32).then_some(key)
}

/// The little-endian 256-bit coordinate must be below 2^255 - 19.
fn canonical_coordinate(key: &[u8; 32]) -> bool {
    key[31] < 0x7f
        || (key[31] == 0x7f && (key[1..31].iter().any(|byte| *byte != 0xff) || key[0] < 0xed))
}
