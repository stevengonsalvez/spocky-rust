//! JavaScript string semantics the pinned daemon relies on.

/// `String.prototype.trim` and its whitespace set, from spocky-contracts: one
/// copy for the workspace.
pub use spocky_contracts::text::{is_js_whitespace as is_whitespace, js_trim as trim};

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
