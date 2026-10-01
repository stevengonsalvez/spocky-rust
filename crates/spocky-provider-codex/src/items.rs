//! Codex thread items to Paseo timeline items, plus token usage.
//!
//! Port of `threadItemToTimeline`, `toAgentUsage`, the plan helpers, and
//! `codexAsyncQuestionToTimeline` from pinned Paseo. Timeline items are JSON
//! objects whose key order equals the order of Paseo's object literals, since
//! that order reaches the wire unchanged.
//!
//! Slice scope: `userMessage`, `agentMessage` (including async questions),
//! `reasoning`, `plan`, and `contextCompaction` map here, and tool items go
//! through [`crate::tools`]. Tool types that module has not ported and image
//! items (`imageView`, `imageGeneration`) report `ThreadItemMapping::Unported`
//! so a caller can never mistake them for items Paseo drops.

use serde_json::{Map, Value, json};

use spocky_contracts::text::{is_js_whitespace, js_trim};

/// Result of mapping one Codex thread item.
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadItemMapping {
    /// Paseo renders this timeline item.
    Item(Value),
    /// Paseo renders nothing for this item.
    Skip,
    /// Paseo renders this item through a mapper not yet ported to Spocky.
    Unported { item_type: String },
}

pub const CODEX_TOOL_THREAD_ITEM_TYPES: [&str; 6] = [
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "webSearch",
    "collabAgentToolCall",
    "subAgentActivity",
];

/// `normalizeCodexThreadItemType`: `PascalCase` legacy names to `camelCase`.
#[must_use]
pub fn normalize_thread_item_type(raw: &str) -> &str {
    match raw {
        "UserMessage" => "userMessage",
        "AgentMessage" => "agentMessage",
        "Reasoning" => "reasoning",
        "Plan" => "plan",
        "CommandExecution" => "commandExecution",
        "FileChange" => "fileChange",
        "McpToolCall" => "mcpToolCall",
        "WebSearch" => "webSearch",
        "CollabAgentToolCall" => "collabAgentToolCall",
        "SubAgentActivity" => "subAgentActivity",
        "ImageView" => "imageView",
        "ImageGeneration" => "imageGeneration",
        other => other,
    }
}

/// The normalized `type` of a raw item, if it is a string.
#[must_use]
pub fn item_type(item: &Map<String, Value>) -> Option<&str> {
    item.get("type")
        .and_then(Value::as_str)
        .map(normalize_thread_item_type)
}

/// `nonEmptyString`.
#[must_use]
pub fn non_empty_string(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text),
        _ => None,
    }
}

/// `value ?? fallback` for JSON: missing and `null` are nullish.
fn nullish_or<'a>(value: Option<&'a Value>, fallback: Option<&'a Value>) -> Option<&'a Value> {
    match value {
        None | Some(Value::Null) => fallback,
        some => some,
    }
}

/// `threadItemToTimeline(item, { includeUserMessage, cwd })`.
#[must_use]
pub fn thread_item_to_timeline(item: &Value, include_user_message: bool) -> ThreadItemMapping {
    let Value::Object(record) = item else {
        return ThreadItemMapping::Skip;
    };
    let Some(normalized_type) = item_type(record) else {
        return ThreadItemMapping::Skip;
    };
    if normalized_type == "imageView" || normalized_type == "imageGeneration" {
        return ThreadItemMapping::Unported {
            item_type: normalized_type.to_owned(),
        };
    }
    if CODEX_TOOL_THREAD_ITEM_TYPES.contains(&normalized_type) {
        return match crate::tools::tool_call_from_thread_item(record, normalized_type) {
            crate::tools::ToolMapping::Item(item) => ThreadItemMapping::Item(item),
            crate::tools::ToolMapping::Skip => ThreadItemMapping::Skip,
            crate::tools::ToolMapping::Unported(_) => ThreadItemMapping::Unported {
                item_type: normalized_type.to_owned(),
            },
        };
    }
    let mapped = match normalized_type {
        "userMessage" => user_message_item(record, include_user_message),
        "agentMessage" => Some(agent_message_item(record)),
        "plan" => plan_item(record),
        "reasoning" => reasoning_item(record),
        "contextCompaction" => Some(json!({"type": "compaction", "status": "completed"})),
        _ => None,
    };
    mapped.map_or(ThreadItemMapping::Skip, ThreadItemMapping::Item)
}

fn user_message_item(record: &Map<String, Value>, include: bool) -> Option<Value> {
    if !include {
        return None;
    }
    let text = extract_user_text(record.get("content")).unwrap_or_default();
    let mut item = Map::new();
    item.insert("type".to_owned(), json!("user_message"));
    item.insert("text".to_owned(), Value::String(text));
    if let Some(message_id) = non_empty_string(record.get("id")) {
        item.insert("messageId".to_owned(), json!(message_id));
    }
    let client = nullish_or(
        nullish_or(record.get("clientId"), record.get("client_id")),
        record.get("clientUserMessageId"),
    );
    if let Some(client_message_id) = non_empty_string(client) {
        item.insert("clientMessageId".to_owned(), json!(client_message_id));
    }
    Some(Value::Object(item))
}

fn extract_user_text(content: Option<&Value>) -> Option<String> {
    let Some(Value::Array(entries)) = content else {
        return None;
    };
    let parts: Vec<&str> = entries
        .iter()
        .filter_map(Value::as_object)
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|entry| entry.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn agent_message_item(record: &Map<String, Value>) -> Value {
    if let Some(question) = async_question_to_timeline(record) {
        return question;
    }
    let mut item = Map::new();
    item.insert("type".to_owned(), json!("assistant_message"));
    let text = record.get("text").and_then(Value::as_str).unwrap_or("");
    item.insert("text".to_owned(), json!(text));
    if let Some(message_id) = non_empty_string(record.get("id")) {
        item.insert("messageId".to_owned(), json!(message_id));
    }
    Value::Object(item)
}

/// An async question parsed by Paseo's `ItemSchema` in `async-questions.ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct AsyncQuestionItem {
    pub id: String,
    pub questions: Vec<AsyncQuestion>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncQuestion {
    /// Trimmed by the zod schema.
    pub title: String,
    pub options: QuestionOptions,
}

/// `options: z.array(z.string().min(1)).nullish()` keeps missing and `null`
/// apart in the parsed record Paseo persists.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionOptions {
    Missing,
    Null,
    Labels(Vec<String>),
}

impl QuestionOptions {
    /// `question.options ?? []`.
    #[must_use]
    pub fn labels(&self) -> &[String] {
        match self {
            Self::Labels(labels) => labels,
            Self::Missing | Self::Null => &[],
        }
    }
}

/// `ItemSchema.safeParse(item)`.
#[must_use]
pub fn parse_async_question(record: &Map<String, Value>) -> Option<AsyncQuestionItem> {
    if record.get("type").and_then(Value::as_str) != Some("agentMessage")
        || record.get("delivery").and_then(Value::as_str) != Some("async")
    {
        return None;
    }
    let id = non_empty_string(record.get("id"))?.to_owned();
    let Some(Value::Array(raw_questions)) = record.get("questions") else {
        return None;
    };
    if raw_questions.is_empty() {
        return None;
    }
    let mut questions = Vec::with_capacity(raw_questions.len());
    for raw in raw_questions {
        let raw = raw.as_object()?;
        let title = js_trim(raw.get("title")?.as_str()?);
        if title.is_empty() {
            return None;
        }
        let options = match raw.get("options") {
            None => QuestionOptions::Missing,
            Some(Value::Null) => QuestionOptions::Null,
            Some(Value::Array(options)) => {
                let mut labels = Vec::with_capacity(options.len());
                for option in options {
                    match option.as_str() {
                        Some(label) if !label.is_empty() => labels.push(label.to_owned()),
                        _ => return None,
                    }
                }
                QuestionOptions::Labels(labels)
            }
            Some(_) => return None,
        };
        questions.push(AsyncQuestion {
            title: title.to_owned(),
            options,
        });
    }
    Some(AsyncQuestionItem { id, questions })
}

/// Async question resolution recorded by `CodexAsyncQuestions`.
#[derive(Debug, Clone, PartialEq)]
pub enum AsyncQuestionResolution {
    Dismissed,
    Answers(Vec<String>),
}

/// `toTimeline(record)` in `async-questions.ts`.
#[must_use]
pub fn async_question_timeline(
    question: &AsyncQuestionItem,
    resolution: Option<&AsyncQuestionResolution>,
) -> Value {
    let answers = match resolution {
        Some(AsyncQuestionResolution::Answers(answers)) => Some(answers),
        _ => None,
    };
    let blocks: Vec<String> = question
        .questions
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let second = match answers {
                Some(answers) => answers.get(index).cloned().unwrap_or_default(),
                None => entry.options.labels().join(", "),
            };
            [entry.title.clone(), second]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let mut text = blocks.join("\n\n");
    if resolution == Some(&AsyncQuestionResolution::Dismissed) {
        text.push_str("\n\nDismissed");
    }
    json!({
        "type": "tool_call",
        "callId": question.id,
        "name": "request_user_input_async",
        "status": "completed",
        "error": null,
        "detail": {
            "type": "plain_text",
            "icon": "brain",
            "text": text,
        },
    })
}

/// `codexAsyncQuestionToTimeline(item)`.
#[must_use]
pub fn async_question_to_timeline(record: &Map<String, Value>) -> Option<Value> {
    parse_async_question(record).map(|question| async_question_timeline(&question, None))
}

/// `toPermission(record)` in `async-questions.ts`.
#[must_use]
pub fn async_question_permission(question: &AsyncQuestionItem) -> Value {
    let questions: Vec<Value> = question
        .questions
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let options: Vec<Value> = entry
                .options
                .labels()
                .iter()
                .map(|label| json!({"label": label}))
                .collect();
            json!({
                "id": index.to_string(),
                "header": format!("Question {}", index + 1),
                "question": entry.title,
                "options": options,
                "isOther": true,
            })
        })
        .collect();
    json!({
        "id": format!("permission-{}", question.id),
        "provider": "codex",
        "name": "request_user_input_async",
        "kind": "question",
        "title": "Question",
        "input": {"questions": questions},
    })
}

/// One `CodexAsyncQuestions` record as `serialize()` persists it: the zod
/// output keeps only declared keys, in schema order.
#[must_use]
pub fn async_question_record(
    question: &AsyncQuestionItem,
    resolution: Option<&AsyncQuestionResolution>,
) -> Value {
    let questions: Vec<Value> = question
        .questions
        .iter()
        .map(|entry| {
            let mut record = Map::new();
            record.insert("title".to_owned(), json!(entry.title));
            match &entry.options {
                QuestionOptions::Missing => {}
                QuestionOptions::Null => {
                    record.insert("options".to_owned(), Value::Null);
                }
                QuestionOptions::Labels(labels) => {
                    record.insert("options".to_owned(), json!(labels));
                }
            }
            Value::Object(record)
        })
        .collect();
    let mut record = Map::new();
    record.insert(
        "item".to_owned(),
        json!({
            "type": "agentMessage",
            "id": question.id,
            "delivery": "async",
            "questions": questions,
        }),
    );
    match resolution {
        None => {}
        Some(AsyncQuestionResolution::Dismissed) => {
            record.insert("resolution".to_owned(), json!("dismissed"));
        }
        Some(AsyncQuestionResolution::Answers(answers)) => {
            record.insert("resolution".to_owned(), json!(answers));
        }
    }
    Value::Object(record)
}

fn reasoning_item(record: &Map<String, Value>) -> Option<Value> {
    let summary = js_join_array(record.get("summary"));
    let content = js_join_array(record.get("content"));
    let text = if summary.is_empty() { content } else { summary };
    if text.is_empty() {
        None
    } else {
        Some(json!({"type": "reasoning", "text": text}))
    }
}

/// `Array.isArray(value) ? value.join("\n") : ""`.
fn js_join_array(value: Option<&Value>) -> String {
    let Some(Value::Array(entries)) = value else {
        return String::new();
    };
    entries
        .iter()
        .map(js_array_element_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `Array.prototype.join` element conversion.
fn js_array_element_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => crate::notification::js_number_to_string(number),
        Value::String(text) => text.clone(),
        Value::Array(entries) => entries
            .iter()
            .map(js_array_element_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn plan_item(record: &Map<String, Value>) -> Option<Value> {
    let text = record.get("text").and_then(Value::as_str).unwrap_or("");
    let id = nullish_or(record.get("id"), record.get("itemId"));
    let call_id = non_empty_string(id).map_or_else(
        || format!("plan:{}", normalize_plan_markdown(text)),
        str::to_owned,
    );
    plan_tool_call(&call_id, text)
}

/// `normalizePlanMarkdown`.
#[must_use]
pub fn normalize_plan_markdown(text: &str) -> String {
    let joined = text
        .split('\n')
        .map(|line| line.trim_end_matches(is_js_whitespace))
        .collect::<Vec<_>>()
        .join("\n");
    js_trim(&joined).to_owned()
}

/// `mapCodexPlanToToolCall`.
#[must_use]
pub fn plan_tool_call(call_id: &str, text: &str) -> Option<Value> {
    let text = normalize_plan_markdown(text);
    if text.is_empty() {
        return None;
    }
    Some(json!({
        "type": "tool_call",
        "callId": call_id,
        "name": "plan",
        "status": "completed",
        "error": null,
        "detail": {"type": "plan", "text": text},
    }))
}

/// `mapCodexPlanUpdateToTodo`.
#[must_use]
pub fn plan_update_to_todo(plan: &[crate::notification::PlanEntry]) -> Value {
    let items: Vec<Value> = plan
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let text = js_trim(entry.step.as_deref()?);
            if text.is_empty() {
                return None;
            }
            let status = match entry.status.as_deref() {
                Some("completed") => "completed",
                Some("inProgress" | "in_progress") => "in_progress",
                _ => "pending",
            };
            Some(json!({
                "id": index.to_string(),
                "text": text,
                "status": status,
                "completed": status == "completed",
            }))
        })
        .collect();
    json!({"type": "todo", "items": items})
}

fn positive_finite(value: Option<&Value>) -> Option<Value> {
    let number = value?.as_f64()?;
    (number.is_finite() && number > 0.0)
        .then(|| value.cloned())
        .flatten()
}

/// `toAgentUsage(tokenUsage)`; `None` when the payload is not an object.
#[must_use]
pub fn to_agent_usage(token_usage: Option<&Value>) -> Option<Value> {
    let usage = token_usage?.as_object()?;
    let last = usage.get("last").and_then(Value::as_object);
    let last_field = |key: &str| last.and_then(|last| last.get(key));
    let number = |value: Option<&Value>| value.filter(|value| value.is_number()).cloned();
    let max_tokens = positive_finite(usage.get("model_context_window"))
        .or_else(|| positive_finite(usage.get("modelContextWindow")));
    let used_tokens = positive_finite(last_field("total_tokens"))
        .or_else(|| positive_finite(last_field("totalTokens")));
    let mut output = Map::new();
    for (key, value) in [
        ("inputTokens", number(last_field("inputTokens"))),
        ("cachedInputTokens", number(last_field("cachedInputTokens"))),
        ("outputTokens", number(last_field("outputTokens"))),
        ("contextWindowMaxTokens", max_tokens),
        ("contextWindowUsedTokens", used_tokens),
    ] {
        if let Some(value) = value {
            output.insert(key.to_owned(), value);
        }
    }
    Some(Value::Object(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(value: &Value) -> Value {
        match thread_item_to_timeline(value, true) {
            ThreadItemMapping::Item(item) => item,
            other => panic!("expected item, got {other:?}"),
        }
    }

    fn paseo_usage() -> Value {
        json!({
            "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000,
            "contextWindowMaxTokens": 200_000, "contextWindowUsedTokens": 50000
        })
    }

    // Paseo: "extracts context window usage from snake_case token payloads".
    #[test]
    fn usage_from_snake_case_payload() {
        let usage = to_agent_usage(Some(&json!({
            "model_context_window": 200_000,
            "last": {"total_tokens": 50000, "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}
        })));
        assert_eq!(usage, Some(paseo_usage()));
    }

    // Paseo: "extracts context window usage from camelCase token payloads".
    #[test]
    fn usage_from_camel_case_payload() {
        let usage = to_agent_usage(Some(&json!({
            "modelContextWindow": 200_000,
            "last": {"totalTokens": 50000, "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}
        })));
        assert_eq!(usage, Some(paseo_usage()));
    }

    // Paseo: "keeps existing usage behavior when context window fields are missing".
    #[test]
    fn usage_without_context_window_fields() {
        let usage = to_agent_usage(Some(&json!({
            "last": {"inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}
        })));
        assert_eq!(
            usage,
            Some(json!({"inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}))
        );
    }

    // Paseo: "excludes invalid context window values". JSON has no NaN, so the
    // non-number cases use null and numeric strings.
    #[test]
    fn usage_excludes_invalid_context_window_values() {
        let usage = to_agent_usage(Some(&json!({
            "model_context_window": null,
            "modelContextWindow": "200000",
            "last": {"total_tokens": null, "totalTokens": "50000", "inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}
        })));
        assert_eq!(
            usage,
            Some(json!({"inputTokens": 30000, "cachedInputTokens": 5000, "outputTokens": 15000}))
        );
    }

    #[test]
    fn usage_rejects_non_objects_and_keeps_empty_objects() {
        assert_eq!(to_agent_usage(Some(&json!("x"))), None);
        assert_eq!(to_agent_usage(Some(&json!({}))), Some(json!({})));
        assert_eq!(
            to_agent_usage(Some(
                &json!({"model_context_window": 0, "last": {"totalTokens": -1}})
            )),
            Some(json!({}))
        );
    }

    #[test]
    fn real_codex_user_message_with_null_client_id() {
        let mapped = item(&json!({
            "type": "userMessage", "id": "u1", "clientId": null,
            "content": [{"type": "text", "text": "Say hello", "text_elements": []}]
        }));
        assert_eq!(
            serde_json::to_string(&mapped).unwrap(),
            r#"{"type":"user_message","text":"Say hello","messageId":"u1"}"#
        );
        assert_eq!(
            thread_item_to_timeline(&json!({"type": "userMessage", "id": "u1"}), false),
            ThreadItemMapping::Skip
        );
    }

    #[test]
    fn user_message_client_id_falls_back_through_aliases() {
        let mapped = item(&json!({
            "type": "UserMessage", "id": "", "clientId": null, "client_id": "c-1",
            "content": [{"type": "text", "text": "a"}, {"type": "image"}, {"type": "text", "text": "b"}]
        }));
        assert_eq!(
            serde_json::to_string(&mapped).unwrap(),
            r#"{"type":"user_message","text":"a\nb","clientMessageId":"c-1"}"#
        );
    }

    #[test]
    fn real_codex_agent_message_maps_to_assistant_message() {
        let mapped = item(&json!({
            "type": "agentMessage", "id": "msg_1", "text": "Hello from stub.",
            "phase": null, "memoryCitation": null, "delivery": null, "questions": null
        }));
        assert_eq!(
            serde_json::to_string(&mapped).unwrap(),
            r#"{"type":"assistant_message","text":"Hello from stub.","messageId":"msg_1"}"#
        );
    }

    #[test]
    fn async_question_agent_message_maps_to_completed_tool_call() {
        let mapped = item(&json!({
            "type": "agentMessage", "id": "q1", "delivery": "async",
            "questions": [{"title": "  Pick one ", "options": ["A", "B"]}, {"title": "Why?"}]
        }));
        assert_eq!(
            mapped,
            json!({
                "type": "tool_call", "callId": "q1", "name": "request_user_input_async",
                "status": "completed", "error": null,
                "detail": {"type": "plain_text", "icon": "brain", "text": "Pick one\nA, B\n\nWhy?"}
            })
        );
    }

    #[test]
    fn reasoning_prefers_summary_then_content() {
        assert_eq!(
            item(&json!({"type": "reasoning", "summary": ["a", "b"], "content": ["c"]})),
            json!({"type": "reasoning", "text": "a\nb"})
        );
        assert_eq!(
            item(&json!({"type": "reasoning", "summary": [], "content": ["c"]})),
            json!({"type": "reasoning", "text": "c"})
        );
        assert_eq!(
            thread_item_to_timeline(&json!({"type": "reasoning", "summary": []}), true),
            ThreadItemMapping::Skip
        );
    }

    // Paseo: "maps Codex plan markdown to a synthetic plan tool call".
    #[test]
    fn plan_item_maps_to_plan_tool_call() {
        assert_eq!(
            item(&json!({"type": "plan", "id": "p1", "text": "- one  \n- two\n"})),
            json!({
                "type": "tool_call", "callId": "p1", "name": "plan", "status": "completed",
                "error": null, "detail": {"type": "plan", "text": "- one\n- two"}
            })
        );
    }

    // Paseo: "preserves checklist progress without creating a plan card".
    #[test]
    fn plan_update_maps_to_todo() {
        use crate::notification::PlanEntry;
        let entry = |step: &str, status: &str| PlanEntry {
            step: Some(step.to_owned()),
            status: Some(status.to_owned()),
        };
        assert_eq!(
            plan_update_to_todo(&[
                entry("Inspect", "completed"),
                entry("Implement", "inProgress"),
                entry("Verify", "pending"),
            ]),
            json!({"type": "todo", "items": [
                {"id": "0", "text": "Inspect", "status": "completed", "completed": true},
                {"id": "1", "text": "Implement", "status": "in_progress", "completed": false},
                {"id": "2", "text": "Verify", "status": "pending", "completed": false}
            ]})
        );
    }

    #[test]
    fn plan_update_skips_blank_steps_and_keeps_indexes() {
        use crate::notification::PlanEntry;
        assert_eq!(
            plan_update_to_todo(&[
                PlanEntry {
                    step: Some("  ".to_owned()),
                    status: None
                },
                PlanEntry {
                    step: Some(" Ship ".to_owned()),
                    status: None
                },
            ]),
            json!({"type": "todo", "items": [
                {"id": "1", "text": "Ship", "status": "pending", "completed": false}
            ]})
        );
    }

    #[test]
    fn plan_markdown_trims_only_javascript_whitespace() {
        // JS `\s` and `trim()` leave U+0085 (NEL) in place; Rust's
        // `char::is_whitespace` would strip it.
        assert_eq!(
            normalize_plan_markdown("a\u{85}\n b \u{3000}"),
            "a\u{85}\n b"
        );
    }

    #[test]
    fn compaction_and_unported_types() {
        assert_eq!(
            item(&json!({"type": "contextCompaction", "id": "c"})),
            json!({"type": "compaction", "status": "completed"})
        );
        assert_eq!(
            thread_item_to_timeline(&json!({"type": "FileChange", "id": "f"}), true),
            ThreadItemMapping::Unported {
                item_type: "fileChange".to_owned()
            }
        );
        assert_eq!(
            thread_item_to_timeline(&json!({"type": "CommandExecution"}), true),
            ThreadItemMapping::Skip
        );
        assert_eq!(
            thread_item_to_timeline(&json!({"type": "hookPrompt"}), true),
            ThreadItemMapping::Skip
        );
    }
}
