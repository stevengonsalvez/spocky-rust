//! `base64-js` 1.5.1 and the relay `base64.ts` helpers, byte for byte.
//!
//! `toByteArray` does not reject malformed input: characters outside the
//! alphabet decode as zero bits, text after the first `=` is ignored, and
//! the output length follows the first `=` position. The relay channel
//! feeds untrusted text frames through `base64ToArrayBuffer`, so these
//! quirks decide which error closes the channel and are reproduced exactly.

use std::{error::Error, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::js_string::{JsString, trim, utf16};

/// Errors thrown by `toByteArray`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Base64JsError {
    /// `getLens` on a string whose length is not a multiple of four.
    InvalidLength,
    /// `new Uint8Array(length)` with a negative length (a `RangeError`).
    InvalidTypedArrayLength(i64),
}

impl fmt::Display for Base64JsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => {
                formatter.write_str("Invalid string. Length must be a multiple of 4")
            }
            Self::InvalidTypedArrayLength(length) => {
                write!(formatter, "Invalid typed array length: {length}")
            }
        }
    }
}

impl Error for Base64JsError {}

/// `fromByteArray`: canonical padded base64.
#[must_use]
pub fn from_byte_array(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// `arrayBufferToBase64` from the relay `base64.ts`.
#[must_use]
pub fn array_buffer_to_base64(bytes: &[u8]) -> String {
    from_byte_array(bytes)
}

/// `base64ToArrayBuffer` from the relay `base64.ts`: trims, maps the URL
/// alphabet to the standard one, pads to a multiple of four, then decodes
/// with `toByteArray`.
///
/// # Errors
///
/// Returns the `toByteArray` error for text that pads to a leading `=`.
pub fn base64_to_array_buffer(text: &str) -> Result<Vec<u8>, Base64JsError> {
    let units = utf16(text);
    let mut normalized: JsString = trim(&units)
        .iter()
        .map(|unit| match *unit {
            0x2d => 0x2b,
            0x5f => 0x2f,
            other => other,
        })
        .collect();
    let padding = (4 - normalized.len() % 4) % 4;
    normalized.resize(normalized.len() + padding, u16::from(b'='));
    to_byte_array(&normalized)
}

/// `toByteArray` over JavaScript UTF-16 code units.
///
/// # Errors
///
/// Returns an error for a length that is not a multiple of four, or for a
/// leading `=`, which asks `Uint8Array` for length -1.
pub fn to_byte_array(units: &[u16]) -> Result<Vec<u8>, Base64JsError> {
    let length = units.len();
    if !length.is_multiple_of(4) {
        return Err(Base64JsError::InvalidLength);
    }
    let valid_length = units
        .iter()
        .position(|unit| *unit == u16::from(b'='))
        .unwrap_or(length);
    let placeholders = if valid_length == length {
        0
    } else {
        4 - valid_length % 4
    };
    let byte_length = (valid_length + placeholders) / 4 * 3;
    let Some(byte_length) = byte_length.checked_sub(placeholders) else {
        let negative = i64::try_from(placeholders - byte_length).unwrap_or(i64::MAX);
        return Err(Base64JsError::InvalidTypedArrayLength(-negative));
    };

    let lookup = |index: usize| -> u32 { units.get(index).map_or(0, |unit| reverse(*unit)) };
    let mut output = vec![0_u8; byte_length];
    let mut written = 0;
    // A typed array ignores writes past its end.
    let mut push = |byte: u32| {
        if let Some(slot) = output.get_mut(written) {
            *slot = byte.to_le_bytes()[0];
        }
        written += 1;
    };
    let full_length = if placeholders > 0 {
        valid_length.saturating_sub(4)
    } else {
        valid_length
    };
    let mut index = 0;
    while index < full_length {
        let triple = (lookup(index) << 18)
            | (lookup(index + 1) << 12)
            | (lookup(index + 2) << 6)
            | lookup(index + 3);
        push(triple >> 16);
        push(triple >> 8);
        push(triple);
        index += 4;
    }
    if placeholders == 2 {
        push((lookup(index) << 2) | (lookup(index + 1) >> 4));
    }
    if placeholders == 1 {
        let pair = (lookup(index) << 10) | (lookup(index + 1) << 4) | (lookup(index + 2) >> 2);
        push(pair >> 8);
        push(pair);
    }
    Ok(output)
}

/// `revLookup[code]`, where an unmapped code reads as `undefined` and so
/// contributes zero bits.
fn reverse(unit: u16) -> u32 {
    match u8::try_from(unit) {
        Ok(byte @ b'A'..=b'Z') => u32::from(byte - b'A'),
        Ok(byte @ b'a'..=b'z') => u32::from(byte - b'a') + 26,
        Ok(byte @ b'0'..=b'9') => u32::from(byte - b'0') + 52,
        Ok(b'+' | b'-') => 62,
        Ok(b'/' | b'_') => 63,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_round_trip() {
        for bytes in [&b""[..], b"f", b"fo", b"foo", b"\x00\xff\x10\x80"] {
            let encoded = array_buffer_to_base64(bytes);
            assert_eq!(base64_to_array_buffer(&encoded).unwrap(), bytes);
        }
    }

    #[test]
    fn normalization_accepts_url_alphabet_whitespace_and_missing_padding() {
        assert_eq!(base64_to_array_buffer(" -_8\n").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64_to_array_buffer("Zm9v\u{a0}").unwrap(), b"foo");
        assert_eq!(base64_to_array_buffer("Zg").unwrap(), b"f");
        assert_eq!(base64_to_array_buffer("Zm8").unwrap(), b"fo");
    }

    #[test]
    fn malformed_input_decodes_like_base64_js() {
        // A single trailing character pads to three placeholders: no bytes.
        assert_eq!(base64_to_array_buffer("Zm9vY").unwrap(), b"foo");
        // Characters outside the alphabet contribute zero bits.
        assert_eq!(base64_to_array_buffer("!!!!").unwrap(), [0, 0, 0]);
        assert_eq!(base64_to_array_buffer("{\"a\"").unwrap(), [0, 0x06, 0x80]);
        // Text after the first `=` is ignored, and four placeholders leave
        // two zero bytes the loop never writes.
        assert_eq!(base64_to_array_buffer("Zm9v====").unwrap(), [0, 0]);
        assert_eq!(base64_to_array_buffer("Zg==Zm9v").unwrap(), b"f");
        assert_eq!(base64_to_array_buffer("").unwrap(), b"");
    }

    #[test]
    fn leading_padding_is_a_range_error() {
        let error = base64_to_array_buffer(" =").unwrap_err();
        assert_eq!(error, Base64JsError::InvalidTypedArrayLength(-1));
        assert_eq!(error.to_string(), "Invalid typed array length: -1");
        assert_eq!(
            to_byte_array(&utf16("abc")).unwrap_err().to_string(),
            "Invalid string. Length must be a multiple of 4"
        );
    }
}
