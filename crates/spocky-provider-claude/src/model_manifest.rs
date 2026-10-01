//! `model-manifest.ts`: the curated Claude model catalog, thinking options,
//! version gates, and model id normalization.
//!
//! The baseline's regular expressions are matched by hand. Their `i` flag
//! is non-Unicode, so it folds ASCII letters only, and `\b` and `\d` are
//! ASCII.

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::{is_js_whitespace, js_trim};

/// `CLAUDE_DEFAULT_THINKING_OPTION_ID`.
pub const CLAUDE_DEFAULT_THINKING_OPTION_ID: &str = "high";
/// `CLAUDE_DISABLED_THINKING_OPTION_ID`.
pub const CLAUDE_DISABLED_THINKING_OPTION_ID: &str = "off";
/// `CLAUDE_ULTRACODE_THINKING_OPTION_ID`.
pub const CLAUDE_ULTRACODE_THINKING_OPTION_ID: &str = "ultracode";

const STANDARD: &[&str] = &["low", "medium", "high", "max"];
const XHIGH: &[&str] = &["low", "medium", "high", "xhigh", "max"];

fn effort_label(id: &str) -> &'static str {
    match id {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        _ => "Max",
    }
}

/// One `CLAUDE_MODEL_MANIFEST` entry.
#[derive(Debug, Clone, Copy)]
pub struct ManifestEntry {
    pub id: &'static str,
    pub aliases: Option<&'static [&'static str]>,
    pub label: &'static str,
    pub description: &'static str,
    pub default_priority: Option<f64>,
    pub minimum_claude_code_version: Option<&'static str>,
    pub context_window_max_tokens: Option<f64>,
    pub effort_levels: Option<&'static [&'static str]>,
    pub default_thinking_option_id: Option<&'static str>,
    pub supports_thinking_disabled: bool,
    pub supports_fast_mode: bool,
}

const fn entry(id: &'static str, label: &'static str, description: &'static str) -> ManifestEntry {
    ManifestEntry {
        id,
        aliases: None,
        label,
        description,
        default_priority: None,
        minimum_claude_code_version: None,
        context_window_max_tokens: None,
        effort_levels: None,
        default_thinking_option_id: None,
        supports_thinking_disabled: false,
        supports_fast_mode: false,
    }
}

const ONE_MILLION: Option<f64> = Some(1_000_000.0);
const TWO_HUNDRED_K: Option<f64> = Some(200_000.0);

/// `CLAUDE_MODEL_MANIFEST`, in order.
pub const CLAUDE_MODEL_MANIFEST: &[ManifestEntry] = &[
    ManifestEntry {
        default_priority: Some(3.0),
        minimum_claude_code_version: Some("2.1.280"),
        default_thinking_option_id: Some("medium"),
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        supports_fast_mode: true,
        ..entry("claude-opus-5-5", "Opus 5.5", "Opus 5.5 · Latest release")
    },
    ManifestEntry {
        default_priority: Some(2.0),
        minimum_claude_code_version: Some("2.1.219"),
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry("claude-opus-5", "Opus 5", "Opus 5 · Previous release")
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        ..entry(
            "claude-fable-5-1",
            "Fable 5.1",
            "Fable 5.1 · Most powerful model",
        )
    },
    ManifestEntry {
        aliases: Some(&["claude-fable-5[1m]"]),
        minimum_claude_code_version: Some("2.1.169"),
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        ..entry("claude-fable-5", "Fable 5", "Fable 5 · Previous release")
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry(
            "claude-opus-4-8[1m]",
            "Opus 4.8 1M",
            "Opus 4.8 with 1M context window",
        )
    },
    ManifestEntry {
        default_priority: Some(1.0),
        context_window_max_tokens: TWO_HUNDRED_K,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry("claude-opus-4-8", "Opus 4.8", "Opus 4.8 · Previous release")
    },
    ManifestEntry {
        minimum_claude_code_version: Some("2.1.284"),
        default_thinking_option_id: Some("medium"),
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        ..entry(
            "claude-sonnet-5-5",
            "Sonnet 5.5",
            "Sonnet 5.5 · Best for everyday tasks",
        )
    },
    ManifestEntry {
        context_window_max_tokens: TWO_HUNDRED_K,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        ..entry("claude-sonnet-5", "Sonnet 5", "Sonnet 5 · Previous release")
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        ..entry(
            "claude-sonnet-5[1m]",
            "Sonnet 5 1M",
            "Sonnet 5 with 1M context window",
        )
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry(
            "claude-opus-4-7[1m]",
            "Opus 4.7 1M",
            "Opus 4.7 with 1M context window",
        )
    },
    ManifestEntry {
        context_window_max_tokens: TWO_HUNDRED_K,
        effort_levels: Some(XHIGH),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry("claude-opus-4-7", "Opus 4.7", "Opus 4.7 · Previous release")
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(STANDARD),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry(
            "claude-opus-4-6[1m]",
            "Opus 4.6 1M",
            "Opus 4.6 with 1M context window",
        )
    },
    ManifestEntry {
        context_window_max_tokens: TWO_HUNDRED_K,
        effort_levels: Some(STANDARD),
        supports_thinking_disabled: true,
        supports_fast_mode: true,
        ..entry(
            "claude-opus-4-6",
            "Opus 4.6",
            "Opus 4.6 · Most capable for complex work",
        )
    },
    ManifestEntry {
        context_window_max_tokens: ONE_MILLION,
        effort_levels: Some(STANDARD),
        supports_thinking_disabled: true,
        ..entry(
            "claude-sonnet-4-6[1m]",
            "Sonnet 4.6 1M",
            "Sonnet 4.6 with 1M context window",
        )
    },
    ManifestEntry {
        context_window_max_tokens: TWO_HUNDRED_K,
        effort_levels: Some(STANDARD),
        supports_thinking_disabled: true,
        ..entry(
            "claude-sonnet-4-6",
            "Sonnet 4.6",
            "Sonnet 4.6 · Best for everyday tasks",
        )
    },
    ManifestEntry {
        context_window_max_tokens: TWO_HUNDRED_K,
        ..entry(
            "claude-haiku-4-5",
            "Haiku 4.5",
            "Haiku 4.5 · Fastest for quick answers",
        )
    },
];

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn default_thinking_option_id(model: &ManifestEntry) -> &'static str {
    model
        .default_thinking_option_id
        .unwrap_or(CLAUDE_DEFAULT_THINKING_OPTION_ID)
}

fn select_option(id: &str, label: &str) -> JsObject {
    let mut option = JsObject::new();
    option.insert("id", text(id));
    option.insert("label", text(label));
    option
}

fn build_thinking_options(model: &ManifestEntry) -> Option<Vec<JsValue>> {
    let levels = model.effort_levels?;
    let default = default_thinking_option_id(model);
    let mut options = Vec::new();
    if model.supports_thinking_disabled {
        options.push(JsValue::Object(select_option(
            CLAUDE_DISABLED_THINKING_OPTION_ID,
            "Off",
        )));
    }
    for id in levels {
        let mut option = select_option(id, effort_label(id));
        if *id == default {
            option.insert("isDefault", JsValue::Bool(true));
        }
        options.push(JsValue::Object(option));
    }
    if levels.contains(&"xhigh") {
        options.push(JsValue::Object(select_option(
            CLAUDE_ULTRACODE_THINKING_OPTION_ID,
            "Ultra Code",
        )));
    }
    Some(options)
}

/// `getClaudeManifestModels(claudeCodeVersion)`: `AgentModelDefinition[]`.
#[must_use]
pub fn get_claude_manifest_models(claude_code_version: Option<&str>) -> Vec<JsValue> {
    let available: Vec<&ManifestEntry> = CLAUDE_MODEL_MANIFEST
        .iter()
        .filter(|model| is_model_available(model, claude_code_version))
        .collect();
    let mut default_model: Option<&ManifestEntry> = None;
    for candidate in &available {
        if candidate.default_priority.unwrap_or(0.0)
            > default_model
                .and_then(|model| model.default_priority)
                .unwrap_or(0.0)
        {
            default_model = Some(candidate);
        }
    }
    let mut definitions = Vec::new();
    for model in available {
        let mut definition = JsObject::new();
        definition.insert("provider", text("claude"));
        definition.insert("id", text(model.id));
        definition.insert("label", text(model.label));
        definition.insert("description", text(model.description));
        if let Some(aliases) = model.aliases {
            definition.insert(
                "aliases",
                JsValue::Array(aliases.iter().map(|alias| text(alias)).collect()),
            );
        }
        if default_model.is_some_and(|default| std::ptr::eq(default, model)) {
            definition.insert("isDefault", JsValue::Bool(true));
        }
        if let Some(tokens) = model.context_window_max_tokens {
            definition.insert("contextWindowMaxTokens", JsValue::Number(tokens));
        }
        if let Some(options) = build_thinking_options(model) {
            definition.insert("thinkingOptions", JsValue::Array(options));
            definition.insert(
                "defaultThinkingOptionId",
                text(default_thinking_option_id(model)),
            );
        }
        definitions.push(JsValue::Object(definition.clone()));
        for alias in model.aliases.unwrap_or_default() {
            let mut legacy = definition.clone();
            legacy.insert("id", text(alias));
            legacy.insert("aliases", JsValue::Undefined);
            legacy.insert("isDefault", JsValue::Undefined);
            legacy.insert("isSelectable", JsValue::Bool(false));
            definitions.push(JsValue::Object(legacy));
        }
    }
    definitions
}

fn is_model_available(model: &ManifestEntry, claude_code_version: Option<&str>) -> bool {
    let (Some(minimum), Some(version)) = (model.minimum_claude_code_version, claude_code_version)
    else {
        return true;
    };
    compare_versions(version, minimum) >= 0.0
}

fn compare_versions(left: &str, right: &str) -> f64 {
    let (Some(left), Some(right)) = (
        parse_claude_code_version(left),
        parse_claude_code_version(right),
    ) else {
        return -1.0;
    };
    for index in 0..3 {
        let difference = left[index] - right[index];
        if difference != 0.0 {
            return difference;
        }
    }
    0.0
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn digit_run(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// `\b(\d+)\.(\d+)\.(\d+)` at `start`: the three numbers and the end index.
fn version_at(bytes: &[u8], start: usize) -> Option<([f64; 3], usize)> {
    if start > 0 && is_word(bytes[start - 1]) {
        return None;
    }
    let mut parts = [0.0; 3];
    let mut index = start;
    for (position, part) in parts.iter_mut().enumerate() {
        let run = digit_run(bytes, index);
        if run == 0 {
            return None;
        }
        let digits = std::str::from_utf8(&bytes[index..index + run]).ok()?;
        *part = digits.parse().ok()?;
        index += run;
        if position < 2 {
            if bytes.get(index) != Some(&b'.') {
                return None;
            }
            index += 1;
        }
    }
    Some((parts, index))
}

/// `\s+\(Claude Code\)` (ASCII case-insensitive) at `index`.
fn claude_code_suffix(text: &str, index: usize) -> bool {
    let rest = &text[index..];
    let trimmed = rest.trim_start_matches(is_js_whitespace);
    trimmed.len() < rest.len()
        && trimmed
            .get(..13)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case("(Claude Code)"))
}

/// `parseClaudeCodeVersion(value)`.
#[must_use]
pub fn parse_claude_code_version(value: &str) -> Option<[f64; 3]> {
    let bytes = value.as_bytes();
    let starts = || (0..bytes.len()).filter(|start| bytes[*start].is_ascii_digit());
    for start in starts() {
        if let Some((parts, end)) = version_at(bytes, start)
            && claude_code_suffix(value, end)
        {
            return Some(parts);
        }
    }
    for start in starts() {
        if let Some((parts, end)) = version_at(bytes, start)
            && bytes.get(end).is_none_or(|byte| !is_word(*byte))
        {
            return Some(parts);
        }
    }
    None
}

/// `resolveClaudeDisabledThinkingForModel(modelId)`: whether `off` is
/// supported and the fallback thinking option.
#[must_use]
pub fn resolve_claude_disabled_thinking_for_model(
    model_id: Option<&str>,
) -> (bool, Option<&'static str>) {
    let model = normalize_claude_manifest_model_id(model_id)
        .and_then(|id| CLAUDE_MODEL_MANIFEST.iter().find(|model| model.id == id));
    (
        model.is_some_and(|model| model.supports_thinking_disabled),
        model
            .filter(|model| model.effort_levels.is_some())
            .map(default_thinking_option_id),
    )
}

/// `isClaudeManifestModelId(modelId)`.
#[must_use]
pub fn is_claude_manifest_model_id(model_id: &str) -> bool {
    CLAUDE_MODEL_MANIFEST
        .iter()
        .any(|model| model.id == model_id)
}

/// `claudeManifestModelSupportsFastMode(modelId)`.
#[must_use]
pub fn claude_manifest_model_supports_fast_mode(model_id: Option<&str>) -> bool {
    normalize_claude_manifest_model_id(model_id).is_some_and(|id| {
        CLAUDE_MODEL_MANIFEST
            .iter()
            .any(|model| model.id == id && model.supports_fast_mode)
    })
}

const FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];

/// A family name, ASCII case-insensitive, at `index`.
fn family_at(text: &str, index: usize) -> Option<(&'static str, usize)> {
    let rest = text.get(index..)?;
    FAMILIES.iter().find_map(|family| {
        rest.get(..family.len())
            .filter(|candidate| candidate.eq_ignore_ascii_case(family))
            .map(|_| (*family, index + family.len()))
    })
}

fn is_separator(byte: u8) -> bool {
    matches!(byte, b'-' | b'_' | b' ')
}

fn separators(bytes: &[u8], index: usize) -> usize {
    bytes[index.min(bytes.len())..]
        .iter()
        .take_while(|byte| is_separator(**byte))
        .count()
}

fn one_million_at(text: &str, index: usize) -> bool {
    text.get(index..index + 4)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case("[1m]"))
}

/// `claude[-_ ]` (ASCII case-insensitive) at `index`.
fn claude_prefix_at(text: &str, index: usize) -> bool {
    text.get(index..index + 6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("claude"))
        && text
            .as_bytes()
            .get(index + 6)
            .is_some_and(|byte| is_separator(*byte))
}

struct ModelMatch {
    family: &'static str,
    major: String,
    minor: Option<String>,
    end: usize,
}

/// `(fable|opus|sonnet|haiku)[-_ ]+(\d+)` and, with `minor`, `[-.](\d+)`,
/// starting at `index`.
fn family_numbers_at(text: &str, index: usize, minor: bool) -> Option<ModelMatch> {
    let bytes = text.as_bytes();
    let (family, after_family) = family_at(text, index)?;
    let run = separators(bytes, after_family);
    if run == 0 {
        return None;
    }
    let major_start = after_family + run;
    let major_len = digit_run(bytes, major_start.min(bytes.len()));
    if major_len == 0 {
        return None;
    }
    let mut end = major_start + major_len;
    let major = text[major_start..end].to_owned();
    let minor = if minor {
        if !matches!(bytes.get(end), Some(b'-' | b'.')) {
            return None;
        }
        let minor_len = digit_run(bytes, end + 1);
        if minor_len == 0 {
            return None;
        }
        let value = text[end + 1..end + 1 + minor_len].to_owned();
        end += 1 + minor_len;
        Some(value)
    } else {
        None
    };
    Some(ModelMatch {
        family,
        major,
        minor,
        end,
    })
}

/// The anchored manifest pattern with an optional `claude[-_ ]` prefix and
/// the `(?:\[1m\])?(?:[-_ ]+\d{8})?(?:\[1m\])?$` tail.
fn anchored_match(text: &str, minor: bool) -> Option<ModelMatch> {
    let bytes = text.as_bytes();
    let start = if claude_prefix_at(text, 0) { 7 } else { 0 };
    let found = family_numbers_at(text, start, minor)?;
    let mut index = found.end;
    if one_million_at(text, index) {
        index += 4;
    }
    let run = separators(bytes, index);
    if run > 0 {
        let date_start = index + run;
        if digit_run(bytes, date_start.min(bytes.len())) < 8 {
            return None;
        }
        index = date_start + 8;
    }
    if one_million_at(text, index) {
        index += 4;
    }
    (index == text.len()).then_some(found)
}

/// The unanchored runtime pattern: the leftmost `claude[-_ ]` followed by
/// a family and numbers.
fn unanchored_match(text: &str, minor: bool) -> Option<ModelMatch> {
    (0..text.len())
        .filter(|index| text.is_char_boundary(*index) && claude_prefix_at(text, *index))
        .find_map(|index| family_numbers_at(text, index + 7, minor))
}

fn has_one_million(text: &str) -> bool {
    text.to_lowercase().contains("[1m]")
}

fn first_manifest_id(candidates: [String; 2]) -> Option<String> {
    candidates
        .into_iter()
        .find(|candidate| is_claude_manifest_model_id(candidate))
}

fn normalize_found(found: &ModelMatch, one_million: bool) -> Option<String> {
    let suffix = if one_million { "[1m]" } else { "" };
    let base = match &found.minor {
        Some(minor) => format!("claude-{}-{}-{minor}", found.family, found.major),
        None => format!("claude-{}-{}", found.family, found.major),
    };
    first_manifest_id([format!("{base}{suffix}"), base])
}

/// `normalizeClaudeManifestModelId(value)`.
#[must_use]
pub fn normalize_claude_manifest_model_id(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value.unwrap_or_default());
    if trimmed.is_empty() {
        return None;
    }
    if is_claude_manifest_model_id(trimmed) {
        return Some(trimmed.to_owned());
    }
    if let Some(found) = anchored_match(trimmed, false) {
        return normalize_found(&found, has_one_million(trimmed));
    }
    let found = anchored_match(trimmed, true)?;
    normalize_found(&found, has_one_million(trimmed))
}

/// `normalizeClaudeRuntimeModelId(value)` from the manifest.
#[must_use]
pub fn normalize_claude_runtime_model_id(value: Option<&str>) -> Option<String> {
    if let Some(normalized) = normalize_claude_manifest_model_id(value) {
        return Some(normalized);
    }
    let trimmed = js_trim(value.unwrap_or_default());
    if trimmed.is_empty() {
        return None;
    }
    if let Some(found) = unanchored_match(trimmed, true)
        && let Some(normalized) = normalize_found(&found, has_one_million(trimmed))
    {
        return Some(normalized);
    }
    let found = unanchored_match(trimmed, false)?;
    normalize_found(&found, has_one_million(trimmed))
}

/// `getClaudeCustomModelThinkingOptions()`.
#[must_use]
pub fn get_claude_custom_model_thinking_options() -> Vec<JsValue> {
    STANDARD
        .iter()
        .map(|id| {
            let mut option = select_option(id, effort_label(id));
            if *id == CLAUDE_DEFAULT_THINKING_OPTION_ID {
                option.insert("isDefault", JsValue::Bool(true));
            }
            JsValue::Object(option)
        })
        .collect()
}
