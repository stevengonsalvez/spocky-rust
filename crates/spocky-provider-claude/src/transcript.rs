//! Pure helpers from `providers/claude/agent.ts`: transcript noise and
//! user prompt text, event identifiers, tool result text, usage readers,
//! compaction metadata, permission request shaping, MCP config, and
//! persisted session metadata.

use spocky_contracts::js::truthy;
use spocky_contracts::js_value::{JsObject, JsValue, js_text_utf16, stringify};
use spocky_contracts::text::js_trim;

use crate::provider_image::ProviderImageOutput;

pub const INTERRUPT_TOOL_USE_PLACEHOLDER: &str = "[Request interrupted by user for tool use]";
const NO_RESPONSE_REQUESTED_PLACEHOLDER: &str = "No response requested.";

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `toObjectRecord(value)`: an object that is not an array.
#[must_use]
pub fn record(value: Option<&JsValue>) -> Option<&JsObject> {
    value?.as_object()
}

/// `readTrimmedString(value)`.
#[must_use]
pub fn read_trimmed_string(value: Option<&JsValue>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `readNonEmptyString(value)`: the untrimmed string when its trim is not
/// empty.
#[must_use]
pub fn read_non_empty_string(value: Option<&JsValue>) -> Option<String> {
    let value = value?.as_str()?;
    (!js_trim(value).is_empty()).then(|| value.to_owned())
}

/// `normalizeClaudeTranscriptText(value)`.
fn normalize_transcript_text(value: Option<&JsValue>) -> Option<String> {
    read_trimmed_string(value)
}

/// `INTERRUPT_PLACEHOLDER_PATTERN.test(text)`:
/// `/^\[Request interrupted by user(?:[^\]]*)\]$/`.
fn is_interrupt_placeholder(text: &str) -> bool {
    text.strip_prefix("[Request interrupted by user")
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|middle| !middle.contains(']'))
}

/// `LOCAL_COMMAND_STDOUT_PATTERN.test(text)` on trimmed text.
fn is_local_command_stdout(text: &str) -> bool {
    let trimmed = js_trim(text);
    trimmed.starts_with("<local-command-stdout>")
        && trimmed.ends_with("</local-command-stdout>")
        && trimmed.len() >= "<local-command-stdout></local-command-stdout>".len()
}

/// `isClaudeTranscriptNoiseText(value)`.
#[must_use]
pub fn is_transcript_noise_text(value: &str) -> bool {
    let Some(normalized) = normalize_transcript_text(Some(&text(value))) else {
        return false;
    };
    is_interrupt_placeholder(&normalized)
        || normalized == NO_RESPONSE_REQUESTED_PLACEHOLDER
        || is_local_command_stdout(&normalized)
}

/// `collectClaudeTextContentParts(content)`.
fn collect_text_content_parts(content: Option<&JsValue>) -> Vec<String> {
    match content {
        Some(JsValue::String(_)) => normalize_transcript_text(content).into_iter().collect(),
        Some(JsValue::Array(blocks)) => blocks
            .iter()
            .filter_map(JsValue::as_object)
            .filter_map(|block| {
                normalize_transcript_text(block.get("text"))
                    .or_else(|| normalize_transcript_text(block.get("input")))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `isClaudeTranscriptNoiseContent(content)`.
#[must_use]
pub fn is_transcript_noise_content(content: Option<&JsValue>) -> bool {
    let parts = collect_text_content_parts(content);
    !parts.is_empty() && parts.iter().all(|part| is_transcript_noise_text(part))
}

/// The first lazy `<tag>...</tag>` capture.
fn capture_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

/// `readClaudeCommandPromptName(text)`.
fn read_command_prompt_name(text: &str) -> Option<String> {
    let slash = |name: &str| {
        if name.starts_with('/') {
            name.to_owned()
        } else {
            format!("/{name}")
        }
    };
    if let Some(name) = capture_tag(text, "command-name")
        .map(js_trim)
        .filter(|name| !name.is_empty())
    {
        return Some(slash(name));
    }
    capture_tag(text, "command-message")
        .map(js_trim)
        .filter(|message| !message.is_empty())
        .map(slash)
}

/// `normalizeClaudeUserPromptText(text)`.
#[must_use]
pub fn normalize_user_prompt_text(text: &str) -> Option<String> {
    let normalized = js_trim(text);
    if capture_tag(normalized, "command-message").is_none() {
        return (!normalized.is_empty()).then(|| normalized.to_owned());
    }
    let command = read_command_prompt_name(normalized)?;
    if let Some(args) = capture_tag(normalized, "command-args")
        .map(js_trim)
        .filter(|args| !args.is_empty())
    {
        return Some(format!("{command} {args}"));
    }
    Some(command)
}

fn user_text_part(value: &str) -> Option<String> {
    let trimmed = js_trim(value);
    if trimmed.is_empty() || is_transcript_noise_text(trimmed) {
        return None;
    }
    normalize_user_prompt_text(trimmed)
}

/// `extractUserMessageText(content)`.
#[must_use]
pub fn extract_user_message_text(content: Option<&JsValue>) -> Option<String> {
    match content? {
        JsValue::String(value) => user_text_part(value),
        JsValue::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks.iter().filter_map(JsValue::as_object) {
                if let Some(value) = block
                    .get("text")
                    .and_then(JsValue::as_str)
                    .filter(|value| !js_trim(value).is_empty())
                {
                    parts.extend(user_text_part(value));
                    continue;
                }
                if let Some(value) = block
                    .get("input")
                    .and_then(JsValue::as_str)
                    .filter(|value| !js_trim(value).is_empty())
                {
                    parts.extend(user_text_part(value));
                }
            }
            let combined = parts.join("\n\n");
            let combined = js_trim(&combined);
            (!combined.is_empty()).then(|| combined.to_owned())
        }
        _ => None,
    }
}

/// `extractClaudeUserText(message)` for importable session previews.
#[must_use]
pub fn extract_claude_user_text(message: Option<&JsValue>) -> Option<String> {
    let message = record(message)?;
    if let Some(content) = message.get("content").and_then(JsValue::as_str) {
        return user_text_part(content);
    }
    if let Some(value) = message.get("text").and_then(JsValue::as_str) {
        return user_text_part(value);
    }
    for block in message
        .get("content")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_object)
    {
        if let Some(value) = block.get("text").and_then(JsValue::as_str) {
            let trimmed = js_trim(value);
            if !trimmed.is_empty() && !is_transcript_noise_text(trimmed) {
                return normalize_user_prompt_text(trimmed);
            }
        }
    }
    None
}

/// `isSyntheticUserEntry(entry)`.
#[must_use]
pub fn is_synthetic_user_entry(entry: &JsValue) -> bool {
    let Some(candidate) = entry.as_object() else {
        return false;
    };
    candidate.get("isSynthetic") == Some(&JsValue::Bool(true))
        || candidate.get("isMeta") == Some(&JsValue::Bool(true))
        || truthy(candidate.get("toolUseResult"))
}

/// `isToolResultUserEntry(entry)`.
#[must_use]
pub fn is_tool_result_user_entry(entry: &JsValue) -> bool {
    record(entry.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(JsValue::as_array)
        .is_some_and(|content| {
            content
                .iter()
                .any(|block| block.get("type").and_then(JsValue::as_str) == Some("tool_result"))
        })
}

/// `isSyntheticHistoryUserEntry(entry)`.
#[must_use]
pub fn is_synthetic_history_user_entry(entry: &JsValue) -> bool {
    is_synthetic_user_entry(entry) && !is_tool_result_user_entry(entry)
}

fn first_trimmed(sources: &[Option<&JsValue>]) -> Option<String> {
    sources
        .iter()
        .find_map(|source| read_trimmed_string(*source))
}

/// `readTranscriptUuid(message)`.
#[must_use]
pub fn read_transcript_uuid(message: &JsValue) -> Option<String> {
    let kind = read_trimmed_string(message.get("type"));
    if !matches!(kind.as_deref(), Some("user" | "assistant")) {
        return None;
    }
    read_trimmed_string(message.get("uuid"))
}

/// `readEventIdentifiers(message).messageId`.
#[must_use]
pub fn read_event_message_id(message: &JsValue) -> Option<String> {
    let kind = read_trimmed_string(message.get("type"));
    let event = record(message.get("event"));
    let event_message = event.and_then(|event| record(event.get("message")));
    let container = record(message.get("message"));
    let uuid = matches!(kind.as_deref(), Some("user" | "assistant" | "system"))
        .then(|| message.get("uuid"))
        .flatten();
    first_trimmed(&[
        message.get("message_id"),
        event.and_then(|event| event.get("message_id")),
        event_message.and_then(|inner| inner.get("id")),
        event_message.and_then(|inner| inner.get("message_id")),
        container.and_then(|inner| inner.get("id")),
        container.and_then(|inner| inner.get("message_id")),
        uuid,
    ])
}

/// `readClaudeParentToolUseId(message)`.
#[must_use]
pub fn read_parent_tool_use_id(message: &JsValue) -> Option<String> {
    message
        .get("parent_tool_use_id")
        .and_then(JsValue::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// `isClaudeSubagentToolName(name)`.
#[must_use]
pub fn is_subagent_tool_name(name: Option<&str>) -> bool {
    matches!(name, Some("Task" | "Agent" | "Workflow"))
}

/// `readCompactionMetadata(source)`: `(trigger, preTokens, postTokens)`.
#[must_use]
pub fn read_compaction_metadata(
    source: &JsValue,
) -> Option<(Option<String>, Option<f64>, Option<f64>)> {
    let source = source.as_object()?;
    for key in ["compact_metadata", "compactMetadata", "compactionMetadata"] {
        let Some(metadata) = record(source.get(key)) else {
            continue;
        };
        let present = |value: &&JsValue| !matches!(value, JsValue::Undefined | JsValue::Null);
        let pre = metadata
            .get("preTokens")
            .filter(present)
            .or_else(|| metadata.get("pre_tokens"));
        let post = metadata
            .get("postTokens")
            .filter(present)
            .or_else(|| metadata.get("post_tokens"));
        return Some((
            metadata
                .get("trigger")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            pre.and_then(JsValue::as_f64),
            post.and_then(JsValue::as_f64),
        ));
    }
    None
}

/// The completed `compaction` item.
#[must_use]
pub fn completed_compaction_item(source: &JsValue) -> JsValue {
    let metadata = read_compaction_metadata(source);
    let mut item = JsObject::new();
    item.insert("type", text("compaction"));
    item.insert("status", text("completed"));
    let manual = metadata
        .as_ref()
        .and_then(|(trigger, _, _)| trigger.as_deref())
        == Some("manual");
    item.insert("trigger", text(if manual { "manual" } else { "auto" }));
    item.insert(
        "preTokens",
        metadata
            .and_then(|(_, pre, _)| pre)
            .map_or(JsValue::Undefined, JsValue::Number),
    );
    JsValue::Object(item)
}

fn utf16_key(value: &str) -> Vec<u16> {
    js_text_utf16(value).collect()
}

/// `normalizeForDeterministicString(value)`.
fn normalize_deterministic(value: &JsValue) -> JsValue {
    match value {
        JsValue::Undefined => text("[undefined]"),
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(normalize_deterministic).collect())
        }
        JsValue::Object(object) => {
            let mut keys: Vec<&str> = object.iter().map(|(key, _)| key).collect();
            keys.sort_by_key(|key| utf16_key(key));
            let mut normalized = JsObject::new();
            for key in keys {
                normalized.insert(
                    key,
                    normalize_deterministic(object.get(key).unwrap_or(&JsValue::Undefined)),
                );
            }
            JsValue::Object(normalized)
        }
        other => other.clone(),
    }
}

/// `deterministicStringify(value)`; `None` input is `undefined`.
#[must_use]
pub fn deterministic_stringify(value: Option<&JsValue>) -> String {
    let Some(value) = value.filter(|value| !matches!(value, JsValue::Undefined)) else {
        return String::new();
    };
    let normalized = normalize_deterministic(value);
    match &normalized {
        JsValue::String(text) => text.clone(),
        other => stringify(other),
    }
}

fn is_text_block(value: &JsValue) -> bool {
    value.get("type").and_then(JsValue::as_str) == Some("text")
        && value.get("text").is_some_and(JsValue::is_string)
}

/// `coerceToolResultContentToString(content)`.
#[must_use]
pub fn coerce_tool_result_content_to_string(content: Option<&JsValue>) -> String {
    match content {
        Some(JsValue::String(value)) => value.clone(),
        Some(JsValue::Array(blocks)) if blocks.iter().all(is_text_block) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(JsValue::as_str))
            .collect(),
        other => deterministic_stringify(other),
    }
}

/// `splitClaudeToolResultImages(content)`: the images and the content with
/// each image replaced by an `[image]` text block.
#[must_use]
pub fn split_tool_result_images(
    content: Option<&JsValue>,
) -> (Vec<ProviderImageOutput>, Option<JsValue>) {
    let Some(JsValue::Array(blocks)) = content else {
        return (Vec::new(), content.cloned());
    };
    let mut images = Vec::new();
    let replaced = blocks
        .iter()
        .map(|block| {
            let source = record(block.get("source"));
            let image = (block.get("type").and_then(JsValue::as_str) == Some("image"))
                .then_some(source)
                .flatten()
                .filter(|source| source.get("type").and_then(JsValue::as_str) == Some("base64"))
                .and_then(|source| {
                    let data = source.get("data")?.as_str()?;
                    Some(ProviderImageOutput {
                        data: data.to_owned(),
                        mime_type: source
                            .get("media_type")
                            .and_then(JsValue::as_str)
                            .map(str::to_owned),
                    })
                });
            match image {
                Some(image) if block.is_object() => {
                    images.push(image);
                    let mut placeholder = JsObject::new();
                    placeholder.insert("type", text("text"));
                    placeholder.insert("text", text("[image]"));
                    JsValue::Object(placeholder)
                }
                _ => block.clone(),
            }
        })
        .collect();
    (images, Some(JsValue::Array(replaced)))
}

fn finite_number(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_f64)
        .filter(|number| number.is_finite())
}

/// `extractContextWindowSize(modelUsage)`.
#[must_use]
pub fn extract_context_window_size(model_usage: Option<&JsValue>) -> Option<f64> {
    let usage = record(model_usage)?;
    let mut max: Option<f64> = None;
    for (_, value) in usage.iter() {
        let Some(window) = record(Some(value))
            .and_then(|value| finite_number(value.get("contextWindow")))
            .filter(|window| *window > 0.0)
        else {
            continue;
        };
        max = Some(max.unwrap_or(0.0).max(window));
    }
    max
}

/// `readStreamRequestInputTokens(event)`.
#[must_use]
pub fn read_stream_request_input_tokens(event: &JsObject) -> Option<f64> {
    let usage = record(record(event.get("message"))?.get("usage"))?;
    let input = finite_number(usage.get("input_tokens"))?;
    let creation = finite_number(usage.get("cache_creation_input_tokens")).unwrap_or(0.0);
    let read = finite_number(usage.get("cache_read_input_tokens")).unwrap_or(0.0);
    if input < 0.0 {
        return None;
    }
    Some(input + creation + read)
}

/// `readStreamRequestOutputTokens(event)`.
#[must_use]
pub fn read_stream_request_output_tokens(event: &JsObject) -> Option<f64> {
    finite_number(record(event.get("usage"))?.get("output_tokens")).filter(|tokens| *tokens >= 0.0)
}

/// `readUsageTokenTotal(usage)`.
fn read_usage_token_total(usage: &JsObject) -> Option<f64> {
    let total: f64 = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .iter()
    .map(|key| finite_number(usage.get(key)).unwrap_or(0.0))
    .sum();
    (total > 0.0).then_some(total)
}

/// `readActiveUsageTokens(usage)`: the last object in `usage.iterations`.
#[must_use]
pub fn read_active_usage_tokens(usage: Option<&JsValue>) -> Option<f64> {
    let iterations = record(usage)?.get("iterations")?.as_array()?;
    let active = iterations.iter().rev().find_map(JsValue::as_object)?;
    read_usage_token_total(active)
}

/// `readLegacyResultUsageTokens(usage)`.
#[must_use]
pub fn read_legacy_result_usage_tokens(usage: Option<&JsValue>) -> Option<f64> {
    read_usage_token_total(record(usage)?)
}

/// `readClaudeCommandLifecycle(message)`: `(commandUuid, state)`.
#[must_use]
pub fn read_command_lifecycle(message: &JsValue) -> Option<(String, String)> {
    if message.get("type").and_then(JsValue::as_str) != Some("command_lifecycle") {
        return None;
    }
    let uuid = message.get("command_uuid")?.as_str()?;
    let state = spocky_contracts::js::js_string(message.get("state"));
    matches!(state.as_str(), "queued" | "started" | "completed").then(|| (uuid.to_owned(), state))
}

/// `isMetadata(value)`: any object, arrays included.
#[must_use]
pub fn is_metadata(value: Option<&JsValue>) -> bool {
    matches!(value, Some(JsValue::Object(_) | JsValue::Array(_)))
}

/// `normalizeClaudeAskUserQuestionRequestInput(toolName, input)`.
#[must_use]
pub fn normalize_ask_user_question_request_input(tool_name: &str, input: &JsValue) -> JsValue {
    let Some(questions) = input.get("questions").and_then(JsValue::as_array) else {
        return input.clone();
    };
    if tool_name != "AskUserQuestion" {
        return input.clone();
    }
    let mut normalized = spocky_contracts::js::spread(Some(input));
    normalized.insert(
        "questions",
        JsValue::Array(
            questions
                .iter()
                .map(|item| {
                    if !is_metadata(Some(item)) {
                        return item.clone();
                    }
                    let mut question = spocky_contracts::js::spread(Some(item));
                    question.insert("allowOther", JsValue::Bool(true));
                    JsValue::Object(question)
                })
                .collect(),
        ),
    );
    JsValue::Object(normalized)
}

/// `stripClaudeAskUserQuestionUiMetadata(input)`.
fn strip_ask_user_question_ui_metadata(input: JsObject) -> JsObject {
    let Some(questions) = input.get("questions").and_then(JsValue::as_array) else {
        return input;
    };
    let questions: Vec<JsValue> = questions
        .iter()
        .map(|item| match item {
            JsValue::Object(question) if question.get("allowOther").is_some() => {
                let mut stripped = JsObject::new();
                for (key, value) in question.iter() {
                    if key != "allowOther" {
                        stripped.insert(key, value.clone());
                    }
                }
                JsValue::Object(stripped)
            }
            other => other.clone(),
        })
        .collect();
    let mut stripped = input;
    stripped.insert("questions", JsValue::Array(questions));
    stripped
}

/// `normalizeClaudeAskUserQuestionUpdatedInput(updatedInput, fallbackInput)`.
#[must_use]
pub fn normalize_ask_user_question_updated_input(
    updated: Option<&JsValue>,
    fallback: Option<&JsValue>,
) -> JsValue {
    let fallback = fallback.filter(|value| is_metadata(Some(value)));
    let base = updated.filter(|value| is_metadata(Some(value)));
    let mut merged = spocky_contracts::js::spread(fallback);
    spocky_contracts::js::spread_into(&mut merged, base);
    let merged = strip_ask_user_question_ui_metadata(merged);
    let questions = base
        .and_then(|base| base.get("questions"))
        .filter(|questions| questions.as_array().is_some())
        .or_else(|| {
            fallback
                .and_then(|fallback| fallback.get("questions"))
                .filter(|questions| questions.as_array().is_some())
        });
    let answers = base
        .and_then(|base| base.get("answers"))
        .filter(|answers| is_metadata(Some(answers)));
    let (Some(questions), Some(answers)) = (questions, answers) else {
        return JsValue::Object(merged);
    };
    let mut normalized_answers = JsObject::new();
    for question in questions.as_array().unwrap_or_default() {
        if !is_metadata(Some(question)) {
            continue;
        }
        let Some(question_text) = read_non_empty_string(question.get("question")) else {
            continue;
        };
        let header = read_non_empty_string(question.get("header"));
        let answer = read_non_empty_string(answers.get(&question_text))
            .or_else(|| header.and_then(|header| read_non_empty_string(answers.get(&header))));
        if let Some(answer) = answer {
            normalized_answers.insert(question_text, JsValue::String(answer));
        }
    }
    if normalized_answers.is_empty() {
        return JsValue::Object(merged);
    }
    let mut result = merged;
    result.insert("answers", JsValue::Object(normalized_answers));
    JsValue::Object(result)
}

/// `resolvePermissionKind(toolName, input)`.
#[must_use]
pub fn resolve_permission_kind(tool_name: &str, input: &JsValue) -> &'static str {
    if tool_name == "ExitPlanMode" {
        return "plan";
    }
    if tool_name == "AskUserQuestion"
        && input.get("questions").and_then(JsValue::as_array).is_some()
    {
        return "question";
    }
    "tool"
}

/// `buildClaudeQuestionPermissionSummary(toolName, input)`: `(title,
/// description)`.
#[must_use]
pub fn question_permission_summary(
    tool_name: &str,
    input: &JsValue,
) -> (Option<String>, Option<String>) {
    let Some(questions) = input.get("questions").and_then(JsValue::as_array) else {
        return (None, None);
    };
    if tool_name != "AskUserQuestion" {
        return (None, None);
    }
    let question = questions.iter().find(|item| is_metadata(Some(item)));
    let title = question
        .and_then(|question| question.get("question"))
        .and_then(JsValue::as_str)
        .map(js_trim)
        .unwrap_or_default();
    if title.is_empty() {
        return (None, None);
    }
    let labels: Vec<String> = question
        .and_then(|question| question.get("options"))
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .map(|option| match option {
            JsValue::String(label) => js_trim(label).to_owned(),
            other if is_metadata(Some(other)) => other
                .get("label")
                .and_then(JsValue::as_str)
                .map(js_trim)
                .unwrap_or_default()
                .to_owned(),
            _ => String::new(),
        })
        .filter(|label| !label.is_empty())
        .collect();
    if labels.is_empty() {
        (Some(title.to_owned()), None)
    } else {
        (Some(title.to_owned()), Some(labels.join(" / ")))
    }
}

/// `isPermissionUpdate(value)`.
#[must_use]
pub fn is_permission_update(value: &JsValue) -> bool {
    if !is_metadata(Some(value)) {
        return false;
    }
    matches!(
        value.get("type").and_then(JsValue::as_str),
        Some("addRules" | "replaceRules" | "removeRules")
    ) && value.get("rules").and_then(JsValue::as_array).is_some()
        && value.get("behavior").is_some_and(JsValue::is_string)
        && value.get("destination").is_some_and(JsValue::is_string)
}

/// `isMcpServerConfig(value)`.
fn is_mcp_server_config(value: &JsValue) -> bool {
    if !is_metadata(Some(value)) {
        return false;
    }
    match value.get("type").and_then(JsValue::as_str) {
        Some("stdio") => value.get("command").is_some_and(JsValue::is_string),
        Some("http" | "sse") => value.get("url").is_some_and(JsValue::is_string),
        _ => false,
    }
}

/// `isMcpServersRecord(value)`.
#[must_use]
pub fn is_mcp_servers_record(value: Option<&JsValue>) -> bool {
    match value {
        Some(JsValue::Object(servers)) => servers
            .iter()
            .all(|(_, config)| is_mcp_server_config(config)),
        Some(JsValue::Array(servers)) => servers.iter().all(is_mcp_server_config),
        _ => false,
    }
}

/// `toClaudeSdkMcpConfig(config)`.
#[must_use]
pub fn to_claude_sdk_mcp_config(config: &JsValue) -> JsValue {
    let member = |key: &str| config.get(key).cloned().unwrap_or(JsValue::Undefined);
    let mut sdk = JsObject::new();
    match config.get("type").and_then(JsValue::as_str) {
        Some("stdio") => {
            sdk.insert("type", text("stdio"));
            sdk.insert("command", member("command"));
            sdk.insert("args", member("args"));
            sdk.insert("env", member("env"));
        }
        Some(kind @ ("http" | "sse")) => {
            sdk.insert("type", text(kind));
            sdk.insert("url", member("url"));
            sdk.insert("headers", member("headers"));
        }
        _ => return JsValue::Undefined,
    }
    if config.get("alwaysLoad") == Some(&JsValue::Bool(true)) {
        sdk.insert("alwaysLoad", JsValue::Bool(true));
    }
    JsValue::Object(sdk)
}
