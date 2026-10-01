//! JavaScript string semantics the pinned relay channel relies on.
//!
//! The pinned `encrypted-channel.ts` and `base64.ts` operate on JavaScript
//! strings: UTF-16 code units that may hold lone surrogates. These helpers
//! reproduce the exact behavior of `String.prototype.trim`, the regular
//! expression `\s` class, `TextDecoder` (lossy and fatal, both stripping a
//! leading byte order mark), and `JSON.stringify` string quoting.

/// A JavaScript string as UTF-16 code units. It may hold lone surrogates.
pub type JsString = Vec<u16>;

/// Encodes Rust text as JavaScript UTF-16 code units.
#[must_use]
pub fn utf16(text: &str) -> JsString {
    text.encode_utf16().collect()
}

/// Returns true for the ECMAScript `WhiteSpace` and `LineTerminator` code
/// units, the set both `String.prototype.trim` and `\s` use.
#[must_use]
pub const fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009..=0x000d
            | 0x0020
            | 0x00a0
            | 0x1680
            | 0x2000..=0x200a
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

/// `String.prototype.trim`.
#[must_use]
pub fn trim(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|unit| !is_js_whitespace(*unit))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|unit| !is_js_whitespace(*unit))
        .map_or(start, |index| index + 1);
    &units[start..end]
}

/// `text.replace(/\s+/g, " ")`.
#[must_use]
pub fn collapse_whitespace(units: &[u16]) -> JsString {
    let mut output = Vec::with_capacity(units.len());
    let mut in_run = false;
    for unit in units {
        if is_js_whitespace(*unit) {
            if !in_run {
                output.push(u16::from(b' '));
                in_run = true;
            }
        } else {
            output.push(*unit);
            in_run = false;
        }
    }
    output
}

/// `JSON.stringify` applied to a string: quotes and escapes it exactly as
/// the well-formed `JSON.stringify` of ES2019 does, including lone
/// surrogates as lowercase `\uXXXX` escapes.
#[must_use]
pub fn json_quote(units: &[u16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(units.len() + 2);
    output.push('"');
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok('"') => output.push_str("\\\""),
            Ok('\\') => output.push_str("\\\\"),
            Ok('\u{8}') => output.push_str("\\b"),
            Ok('\u{c}') => output.push_str("\\f"),
            Ok('\n') => output.push_str("\\n"),
            Ok('\r') => output.push_str("\\r"),
            Ok('\t') => output.push_str("\\t"),
            Ok(character) if u32::from(character) < 0x20 => {
                output.push_str("\\u00");
                let code = u32::from(character);
                output.push(char::from(HEX[(code >> 4) as usize]));
                output.push(char::from(HEX[(code & 0xf) as usize]));
            }
            Ok(character) => output.push(character),
            Err(error) => {
                let unit = error.unpaired_surrogate();
                output.push_str("\\u");
                for shift in [12, 8, 4, 0] {
                    output.push(char::from(HEX[usize::from((unit >> shift) & 0xf)]));
                }
            }
        }
    }
    output.push('"');
    output
}

/// `new TextDecoder().decode(bytes)`: lossy UTF-8 with the WHATWG
/// replacement behavior, after removing one leading byte order mark.
#[must_use]
pub fn decode_utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(strip_bom(bytes)).into_owned()
}

/// `new TextDecoder("utf-8", { fatal: true }).decode(bytes)`, which also
/// removes one leading byte order mark.
///
/// # Errors
///
/// Returns an error when the bytes are not valid UTF-8.
pub fn decode_utf8_fatal(bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
    std::str::from_utf8(strip_bom(bytes)).map(str::to_owned)
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_uses_the_ecmascript_whitespace_set() {
        let text = utf16("\u{feff}\u{a0}\t x \u{2028}\u{3000}");
        assert_eq!(trim(&text), utf16("x").as_slice());
        // U+0085 is Unicode whitespace but not ECMAScript whitespace.
        let next_line = utf16("\u{85}x\u{85}");
        assert_eq!(trim(&next_line), next_line.as_slice());
        assert!(trim(&utf16(" \n\r ")).is_empty());
    }

    #[test]
    fn collapse_replaces_each_whitespace_run_with_one_space() {
        assert_eq!(
            collapse_whitespace(&utf16("a \t\n b\u{a0}\u{a0}c ")),
            utf16("a b c ")
        );
    }

    #[test]
    fn json_quote_matches_well_formed_json_stringify() {
        assert_eq!(json_quote(&utf16("a\"b\\c")), r#""a\"b\\c""#);
        assert_eq!(
            json_quote(&utf16("\u{8}\u{c}\n\r\t\u{1}\u{1f}\u{7f}")),
            "\"\\b\\f\\n\\r\\t\\u0001\\u001f\u{7f}\""
        );
        assert_eq!(json_quote(&[0xd83d, 0xde00]), "\"\u{1f600}\"");
        assert_eq!(json_quote(&[0x61, 0xd83d]), "\"a\\ud83d\"");
        assert_eq!(json_quote(&[0xdc00, 0x62]), "\"\\udc00b\"");
        assert_eq!(json_quote(&utf16("\u{2028}é")), "\"\u{2028}é\"");
    }

    #[test]
    fn text_decoder_strips_one_leading_bom() {
        assert_eq!(decode_utf8_lossy(&[0xef, 0xbb, 0xbf, 0x41]), "A");
        assert_eq!(
            decode_utf8_lossy(&[0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf]),
            "\u{feff}"
        );
        assert_eq!(decode_utf8_fatal(&[0xef, 0xbb, 0xbf, 0x41]).unwrap(), "A");
        assert_eq!(decode_utf8_lossy(&[0x41, 0xff, 0x42]), "A\u{fffd}B");
        assert!(decode_utf8_fatal(&[0xff]).is_err());
    }
}
