//! `@getpaseo/protocol/workspace-labels`: the colour set, a label definition
//! and the two name functions every identity comparison goes through.

use spocky_contracts::text::is_js_whitespace;

/// `WORKSPACE_LABEL_COLORS`, in the protocol's order.
pub const WORKSPACE_LABEL_COLORS: [&str; 10] = [
    "violet", "sky", "emerald", "orange", "pink", "indigo", "teal", "red", "amber", "blue",
];

/// `WorkspaceLabelColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLabelColor {
    Violet,
    Sky,
    Emerald,
    Orange,
    Pink,
    Indigo,
    Teal,
    Red,
    Amber,
    Blue,
}

impl WorkspaceLabelColor {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Violet => "violet",
            Self::Sky => "sky",
            Self::Emerald => "emerald",
            Self::Orange => "orange",
            Self::Pink => "pink",
            Self::Indigo => "indigo",
            Self::Teal => "teal",
            Self::Red => "red",
            Self::Amber => "amber",
            Self::Blue => "blue",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "violet" => Self::Violet,
            "sky" => Self::Sky,
            "emerald" => Self::Emerald,
            "orange" => Self::Orange,
            "pink" => Self::Pink,
            "indigo" => Self::Indigo,
            "teal" => Self::Teal,
            "red" => Self::Red,
            "amber" => Self::Amber,
            "blue" => Self::Blue,
            _ => return None,
        })
    }
}

/// `WorkspaceLabelDefinition`. The name is JavaScript text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLabelDefinition {
    pub name: String,
    pub color: WorkspaceLabelColor,
}

/// `name.replace(/\s+/g, " ").trim()`.
#[must_use]
pub fn normalize_workspace_label_name(name: &str) -> String {
    let mut collapsed = String::with_capacity(name.len());
    let mut in_whitespace = false;
    for character in name.chars() {
        if is_js_whitespace(character) {
            if !in_whitespace {
                collapsed.push(' ');
            }
            in_whitespace = true;
        } else {
            collapsed.push(character);
            in_whitespace = false;
        }
    }
    collapsed.trim_matches(' ').to_owned()
}

/// `normalizeWorkspaceLabelName(name).toLowerCase()`.
#[must_use]
pub fn workspace_label_key(name: &str) -> String {
    normalize_workspace_label_name(name).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip_in_protocol_order() {
        for name in WORKSPACE_LABEL_COLORS {
            assert_eq!(
                WorkspaceLabelColor::parse(name).map(WorkspaceLabelColor::as_str),
                Some(name)
            );
        }
        assert_eq!(WorkspaceLabelColor::parse("Red"), None);
    }

    #[test]
    fn normalization_collapses_and_trims_script_whitespace() {
        assert_eq!(
            normalize_workspace_label_name("\u{a0} Needs \t\n  review\u{3000}"),
            "Needs review"
        );
        // U+0085 is not ECMAScript whitespace.
        assert_eq!(normalize_workspace_label_name("a\u{85}b"), "a\u{85}b");
        assert_eq!(workspace_label_key("  NEEDS   Review "), "needs review");
    }
}
