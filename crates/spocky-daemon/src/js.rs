//! JavaScript string semantics the pinned daemon relies on.

/// `WhiteSpace` and `LineTerminator` code points, the set
/// `String.prototype.trim` strips. It differs from `char::is_whitespace`
/// at U+0085 and U+FEFF.
#[must_use]
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | '\u{20}' | '\u{a0}' | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`.
#[must_use]
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_whitespace)
}

/// `String.prototype.trimStart`.
#[must_use]
pub fn trim_start(text: &str) -> &str {
    text.trim_start_matches(is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_the_javascript_whitespace_set() {
        assert_eq!(trim("\u{feff} a b\u{a0}\n"), "a b");
        assert_eq!(trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
        assert_eq!(trim_start("  a "), "a ");
    }
}
