//! JavaScript string semantics used by session logic: ECMAScript white space
//! (`String.prototype.trim`, `/\s/`) and UTF-16 slicing over JavaScript text
//! ([`spocky_store::js_value`] encoding, which keeps lone surrogates).

use spocky_store::js_value::{js_text_from_utf16, js_text_utf16};

/// ECMAScript `WhiteSpace` and `LineTerminator`: what `trim` removes and
/// `/\s/` matches. Unlike Rust's `char::is_whitespace`, it includes U+FEFF and
/// excludes U+0085.
#[must_use]
pub const fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `String.prototype.trim`.
#[must_use]
pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// `value.replace(/\s+/g, " ")`.
#[must_use]
pub fn collapse_js_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for character in value.chars() {
        if is_js_whitespace(character) {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(character);
            in_run = false;
        }
    }
    out
}

/// `value.slice(0, units)` in UTF-16 code units; a split pair leaves a lone
/// surrogate, kept as JavaScript text.
#[must_use]
pub fn slice_utf16(value: &str, units: usize) -> String {
    let code_units: Vec<u16> = js_text_utf16(value).take(units).collect();
    js_text_from_utf16(&code_units)
}

/// UTF-16 length, `value.length`.
#[must_use]
pub fn utf16_len(value: &str) -> usize {
    js_text_utf16(value).count()
}

#[cfg(test)]
mod tests {
    use super::{collapse_js_whitespace, js_trim, slice_utf16, utf16_len};

    #[test]
    fn whitespace_matches_ecmascript() {
        // node: " \u0085x\u0085 ".trim().length === 3, "﻿x﻿".trim() === "x"
        assert_eq!(js_trim(" \u{85}x\u{85} ").len(), "\u{85}x\u{85}".len());
        assert_eq!(js_trim("\u{feff}x\u{feff}"), "x");
        assert_eq!(js_trim("\u{180e}x"), "\u{180e}x");
        assert_eq!(collapse_js_whitespace("a \t\u{a0}b\u{85}c"), "a b\u{85}c");
    }

    #[test]
    fn utf16_slicing_keeps_lone_surrogates() {
        let text = "ab😀cd";
        assert_eq!(utf16_len(text), 6);
        assert_eq!(slice_utf16(text, 4), "ab😀");
        let split = slice_utf16(text, 3);
        assert_eq!(utf16_len(&split), 3);
        assert_ne!(split, "ab😀");
        assert_eq!(slice_utf16(text, 100), text);
    }
}
