//! `subagents/presentation.ts`: the compact subtitle Claude exposes for a
//! provider subagent.

use spocky_contracts::js_value::{JsValue, js_number};
use spocky_contracts::text::{is_js_whitespace, js_trim};

use crate::models::find_claude_model;

/// `ClaudeSubagentPresentationFacts`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PresentationFacts {
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `usage.totalTokens`.
    pub total_tokens: Option<f64>,
}

impl PresentationFacts {
    /// `{ ...previous, ...patch }`: fields the patch sets replace.
    #[must_use]
    pub fn merged(&self, patch: &Self) -> Self {
        Self {
            title: patch.title.clone().or_else(|| self.title.clone()),
            model: patch.model.clone().or_else(|| self.model.clone()),
            effort: patch.effort.clone().or_else(|| self.effort.clone()),
            total_tokens: patch.total_tokens.or(self.total_tokens),
        }
    }
}

fn read_part(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn format_model(model: Option<&str>) -> Option<String> {
    let normalized = read_part(model)?;
    Some(
        find_claude_model(Some(&normalized))
            .and_then(|model| {
                model
                    .get("label")
                    .and_then(JsValue::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or(normalized),
    )
}

/// `part.charAt(0).toUpperCase() + part.slice(1)`: a first character
/// outside the BMP is a lone surrogate to `charAt` and stays unchanged.
fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let head = if first.len_utf16() == 2 {
        first.to_string()
    } else {
        first.to_uppercase().collect()
    };
    head + chars.as_str()
}

fn format_effort(effort: Option<&str>) -> Option<String> {
    let normalized = read_part(effort)?;
    if normalized == "xhigh" {
        return Some("Extra High".to_owned());
    }
    Some(
        normalized
            .split(|character: char| {
                character == '-' || character == '_' || is_js_whitespace(character)
            })
            .filter(|part| !part.is_empty())
            .map(capitalize)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// `Math.round(value)`: halves round toward +Infinity, and the fraction is
/// taken from `floor` so `0.49999999999999994` stays `0`.
fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

fn format_tokens(total_tokens: Option<f64>) -> Option<String> {
    let tokens = total_tokens.filter(|tokens| *tokens > 0.0)?;
    if tokens < 1000.0 {
        return Some(format!("{} tokens", js_number(js_round(tokens))));
    }
    Some(format!(
        "{}k tokens",
        js_number(js_round(tokens / 100.0) / 10.0)
    ))
}

/// `buildClaudeSubagentSubtitle(facts)`.
#[must_use]
pub fn build_claude_subagent_subtitle(facts: &PresentationFacts) -> Option<String> {
    let parts: Vec<String> = [
        read_part(facts.title.as_deref()),
        format_model(facts.model.as_deref()),
        format_effort(facts.effort.as_deref()),
        format_tokens(facts.total_tokens),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::js_round;

    // node: Math.round(0.49999999999999994) is 0, Math.round(-0.5) is -0,
    // Math.round(2.5) is 3, Math.round(-2.5) is -2.
    #[test]
    fn rounding_matches_math_round() {
        let rounded = |value: f64| js_round(value).to_string();
        assert_eq!(rounded(0.499_999_999_999_999_94), "0");
        assert_eq!(rounded(-0.5), "0");
        assert_eq!(rounded(2.5), "3");
        assert_eq!(rounded(-2.5), "-2");
        assert_eq!(rounded(1_999.5), "2000");
    }
}
