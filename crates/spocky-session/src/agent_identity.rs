//! Agent titles and identifier lookup from pinned Paseo
//! `agent/create-agent-title.ts` and `session.ts` `resolveAgentIdentifier`.

use crate::text::{collapse_js_whitespace, js_trim, slice_utf16};

/// `MAX_EXPLICIT_AGENT_TITLE_CHARS` from `protocol/agent-title-limits.ts`.
pub const MAX_EXPLICIT_AGENT_TITLE_CHARS: usize = 200;
/// `MAX_INITIAL_AGENT_TITLE_CHARS`: `Math.min(60, MAX_EXPLICIT_AGENT_TITLE_CHARS)`.
pub const MAX_INITIAL_AGENT_TITLE_CHARS: usize = 60;

/// `deriveInitialAgentTitle`: the first non-empty line, white space
/// collapsed, cut to 60 UTF-16 units, trimmed.
#[must_use]
pub fn derive_initial_agent_title(prompt: &str) -> Option<String> {
    let first_line = prompt
        .split('\n')
        .map(|line| js_trim(line.strip_suffix('\r').unwrap_or(line)))
        .find(|line| !line.is_empty())?;
    let collapsed = collapse_js_whitespace(first_line);
    let normalized = js_trim(&collapsed);
    if normalized.is_empty() {
        return None;
    }
    let clamped = slice_utf16(normalized, MAX_INITIAL_AGENT_TITLE_CHARS);
    let clamped = js_trim(&clamped);
    (!clamped.is_empty()).then(|| clamped.to_owned())
}

/// `resolveCreateAgentTitles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAgentTitles {
    pub explicit_title: Option<String>,
    pub provisional_title: Option<String>,
}

#[must_use]
pub fn resolve_create_agent_titles(
    config_title: Option<&str>,
    initial_prompt: Option<&str>,
) -> CreateAgentTitles {
    let explicit_title = config_title
        .map(js_trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    let trimmed_prompt = initial_prompt
        .map(js_trim)
        .filter(|prompt| !prompt.is_empty());
    let provisional_title = explicit_title
        .clone()
        .or_else(|| trimmed_prompt.and_then(derive_initial_agent_title));
    CreateAgentTitles {
        explicit_title,
        provisional_title,
    }
}

/// A stored agent as identifier lookup sees it.
#[derive(Debug, Clone, Copy)]
pub struct StoredAgentRef<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub internal: bool,
}

fn preview(ids: &[&str]) -> String {
    let shown: Vec<String> = ids.iter().take(5).map(|id| slice_utf16(id, 8)).collect();
    let more = if ids.len() > 5 { ", …" } else { "" };
    format!("{}{more}", shown.join(", "))
}

/// `resolveAgentIdentifier`: exact id, then a unique id prefix, then a unique
/// stored title. `stored` is in storage order and `live` in manager order.
///
/// # Errors
///
/// Returns the baseline message for an empty, ambiguous, or unknown identifier.
pub fn resolve_agent_identifier(
    identifier: &str,
    stored: &[StoredAgentRef<'_>],
    live: &[&str],
) -> Result<String, String> {
    let trimmed = js_trim(identifier);
    if trimmed.is_empty() {
        return Err("Agent identifier cannot be empty".to_owned());
    }
    let visible: Vec<&StoredAgentRef<'_>> =
        stored.iter().filter(|record| !record.internal).collect();
    let mut known: Vec<&str> = Vec::new();
    for id in visible
        .iter()
        .map(|record| record.id)
        .chain(live.iter().copied())
    {
        if !known.contains(&id) {
            known.push(id);
        }
    }
    if known.contains(&trimmed) {
        return Ok(trimmed.to_owned());
    }
    let prefix_matches: Vec<&str> = known
        .iter()
        .copied()
        .filter(|id| id.starts_with(trimmed))
        .collect();
    match prefix_matches.as_slice() {
        [only] => return Ok((*only).to_owned()),
        [] => {}
        many => {
            return Err(format!(
                "Agent identifier \"{trimmed}\" is ambiguous ({})",
                preview(many)
            ));
        }
    }
    let title_matches: Vec<&str> = visible
        .iter()
        .filter(|record| record.title == Some(trimmed))
        .map(|record| record.id)
        .collect();
    match title_matches.as_slice() {
        [only] => Ok((*only).to_owned()),
        [] => Err(format!("Agent not found: {trimmed}")),
        many => Err(format!(
            "Agent title \"{trimmed}\" is ambiguous ({})",
            preview(many)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StoredAgentRef, derive_initial_agent_title, resolve_agent_identifier,
        resolve_create_agent_titles,
    };
    use spocky_contracts::text::js_length;

    #[test]
    fn titles_match_baseline_rules() {
        assert_eq!(
            derive_initial_agent_title("\n  \r\n  Fix   the\tbug  \nsecond").as_deref(),
            Some("Fix the bug")
        );
        let long = format!("{}😀tail", "a".repeat(59));
        // slice(0, 60) splits the emoji, leaving its high surrogate.
        let title = derive_initial_agent_title(&long).expect("title");
        assert_eq!(js_length(&title), 60);
        assert_eq!(derive_initial_agent_title(" \n \t "), None);
        let titles = resolve_create_agent_titles(Some("  Named  "), Some("prompt"));
        assert_eq!(titles.explicit_title.as_deref(), Some("Named"));
        assert_eq!(titles.provisional_title.as_deref(), Some("Named"));
        let titles = resolve_create_agent_titles(Some("  "), Some("  Run tests  "));
        assert_eq!(titles.explicit_title, None);
        assert_eq!(titles.provisional_title.as_deref(), Some("Run tests"));
    }

    #[test]
    fn identifier_resolution_matches_baseline_messages() {
        let stored = [
            StoredAgentRef {
                id: "aaaa1111-0000",
                title: Some("Alpha"),
                internal: false,
            },
            StoredAgentRef {
                id: "aaaa2222-0000",
                title: Some("Alpha"),
                internal: false,
            },
            StoredAgentRef {
                id: "bbbb1111-0000",
                title: Some("Beta"),
                internal: false,
            },
            StoredAgentRef {
                id: "cccc1111-0000",
                title: Some("Hidden"),
                internal: true,
            },
        ];
        let live = ["dddd1111-0000"];
        let resolve = |identifier: &str| resolve_agent_identifier(identifier, &stored, &live);
        assert_eq!(
            resolve("  ").unwrap_err(),
            "Agent identifier cannot be empty"
        );
        assert_eq!(resolve(" bbbb1111-0000 ").unwrap(), "bbbb1111-0000");
        assert_eq!(resolve("dddd").unwrap(), "dddd1111-0000");
        assert_eq!(
            resolve("aaaa").unwrap_err(),
            "Agent identifier \"aaaa\" is ambiguous (aaaa1111, aaaa2222)"
        );
        assert_eq!(resolve("Beta").unwrap(), "bbbb1111-0000");
        assert_eq!(
            resolve("Alpha").unwrap_err(),
            "Agent title \"Alpha\" is ambiguous (aaaa1111, aaaa2222)"
        );
        assert_eq!(resolve("Hidden").unwrap_err(), "Agent not found: Hidden");
        assert_eq!(resolve("cccc").unwrap_err(), "Agent not found: cccc");
    }
}
