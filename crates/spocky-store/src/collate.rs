//! `String.prototype.localeCompare` with the default ICU root collation, as
//! node 22 runs it, for ASCII text.
//!
//! The ASCII order below was read from node 22 (full ICU) by sorting every
//! ASCII character with `localeCompare`:
//!
//! - Controls U+0000..U+0008, U+000E..U+001F, and U+007F are fully ignorable.
//! - Primary order: tab, LF, VT, FF, CR, space, then
//!   ``_-,;:!?.'"()[]{}@*/\&#%`^+<=>|~$``, then digits, then letters with
//!   case ignored.
//! - When every primary weight ties, case decides left to right: lowercase
//!   sorts before uppercase. Otherwise the strings compare equal.

use std::cmp::Ordering;

// ponytail: non-ASCII characters compare by code point after all ASCII
// primaries; port ICU root weights if non-ASCII ids or names need ordering.

const PRIMARY_ORDER: &[u8] =
    b"\t\n\x0b\x0c\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

fn is_ignorable(character: char) -> bool {
    matches!(character, '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{7f}')
}

/// Primary weight: ASCII by the table, non-ASCII after every ASCII weight.
fn primary(character: char) -> u32 {
    if character.is_ascii() {
        let lower = character.to_ascii_lowercase();
        let position = PRIMARY_ORDER
            .iter()
            .position(|byte| char::from(*byte) == lower)
            .unwrap_or(PRIMARY_ORDER.len());
        u32::try_from(position).unwrap_or(u32::MAX)
    } else {
        0x100 + u32::from(character)
    }
}

fn tertiary(character: char) -> u8 {
    u8::from(character.is_ascii_uppercase())
}

/// `left.localeCompare(right)` as an ordering.
#[must_use]
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let keys = |text: &str| -> Vec<char> { text.chars().filter(|c| !is_ignorable(*c)).collect() };
    let (left, right) = (keys(left), keys(right));
    let primaries = |chars: &[char]| chars.iter().map(|c| primary(*c)).collect::<Vec<_>>();
    primaries(&left).cmp(&primaries(&right)).then_with(|| {
        let tertiaries = |chars: &[char]| chars.iter().map(|c| tertiary(*c)).collect::<Vec<_>>();
        tertiaries(&left).cmp(&tertiaries(&right))
    })
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::locale_compare;

    #[test]
    fn sorts_like_node_icu() {
        // node -e '[...].sort((a, b) => a.localeCompare(b))'
        let mut ids = vec![
            "prj_B", "prj_a", "prj_A", "prj_b", "prj_", "prj_0", "prj-a", "prjA", "a", "A", "aB",
            "Ab", "ab", "AB",
        ];
        ids.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(
            ids,
            vec![
                "a", "A", "ab", "aB", "Ab", "AB", "prj_", "prj_0", "prj_a", "prj_A", "prj_b",
                "prj_B", "prj-a", "prjA"
            ]
        );
    }

    #[test]
    fn ignorables_and_whitespace_match_node() {
        assert_eq!(locale_compare("ab", "aB"), Ordering::Less);
        assert_eq!(locale_compare("a\u{1}b", "ab"), Ordering::Equal);
        assert_eq!(locale_compare("a b", "ab"), Ordering::Less);
        assert_eq!(locale_compare("x\ty", "x y"), Ordering::Less);
        assert_eq!(
            locale_compare("prj_0123456789abcdef", "prj_0123456789abcdee"),
            Ordering::Greater
        );
    }
}
