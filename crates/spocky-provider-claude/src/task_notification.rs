//! `task-notification-tool-call.ts`: Claude background task notifications
//! (system records, queued operations, and `<task-notification>` user
//! content) to synthetic `task_notification` tool calls.

use std::fmt::Write as _;

use sha1::{Digest, Sha1};
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::js_trim;

const TASK_NOTIFICATION_MARKER: &str = "<task-notification>";

/// `TaskNotificationEnvelope`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Envelope {
    message_id: Option<String>,
    task_id: Option<String>,
    tool_use_id: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    output_file: Option<String>,
    raw_text: Option<String>,
}

/// `toNonEmptyString(value)`: a trimmed non-empty string.
fn non_empty(value: Option<&JsValue>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn non_empty_str(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `TaskNotificationUserContentSchema` then `collectUserContentParts`:
/// `None` when the content does not parse.
fn extract_user_content_text(content: Option<&JsValue>) -> Option<String> {
    let parts = match content? {
        JsValue::String(text) => non_empty_str(Some(text)).into_iter().collect(),
        JsValue::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                let block = block.as_object()?;
                let text = block
                    .get("text")
                    .filter(|v| !matches!(v, JsValue::Undefined));
                let input = block
                    .get("input")
                    .filter(|v| !matches!(v, JsValue::Undefined));
                if text.is_some_and(|v| !v.is_string()) || input.is_some_and(|v| !v.is_string()) {
                    return None;
                }
                parts.extend(non_empty(text));
                parts.extend(non_empty(input));
            }
            parts
        }
        _ => return None,
    };
    let parts: Vec<String> = parts;
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// `readTaskNotificationTagValue({ text, tagName })`: the text between the
/// first `<tag>` and the first `</tag>` after it (ASCII case-insensitive),
/// trimmed.
fn read_tag(text: &str, tag: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    non_empty_str(Some(&text[start..end]))
}

fn read_tool_use_id_tag(text: &str) -> Option<String> {
    read_tag(text, "tool-use-id").or_else(|| read_tag(text, "tool_use_id"))
}

fn read_output_file_tag(text: &str) -> Option<String> {
    read_tag(text, "output-file").or_else(|| read_tag(text, "output_file"))
}

fn parse_from_user_content(
    content: Option<&JsValue>,
    message_id: Option<&str>,
) -> Option<Envelope> {
    let raw_text = extract_user_content_text(content)?;
    if !raw_text.contains(TASK_NOTIFICATION_MARKER) {
        return None;
    }
    Some(Envelope {
        message_id: non_empty_str(message_id),
        task_id: read_tag(&raw_text, "task-id"),
        tool_use_id: read_tool_use_id_tag(&raw_text),
        status: read_tag(&raw_text, "status"),
        summary: read_tag(&raw_text, "summary"),
        output_file: read_output_file_tag(&raw_text),
        raw_text: Some(raw_text),
    })
}

const HISTORY_STRING_FIELDS: [&str; 10] = [
    "type",
    "subtype",
    "uuid",
    "message_id",
    "task_id",
    "tool_use_id",
    "status",
    "summary",
    "output_file",
    "content",
];

/// `TaskNotificationHistoryRecordSchema`: the record when it parses.
fn history_record(value: &JsValue) -> Option<&JsObject> {
    let record = value.as_object()?;
    for key in HISTORY_STRING_FIELDS {
        if record
            .get(key)
            .is_some_and(|value| !matches!(value, JsValue::Undefined) && !value.is_string())
        {
            return None;
        }
    }
    if record
        .get("message")
        .is_some_and(|value| !matches!(value, JsValue::Undefined) && !value.is_object())
    {
        return None;
    }
    Some(record)
}

fn str_field<'a>(record: &'a JsObject, key: &str) -> Option<&'a str> {
    record.get(key).and_then(JsValue::as_str)
}

fn parse_from_system_record(value: &JsValue) -> Option<Envelope> {
    let record = history_record(value)?;
    let kind = str_field(record, "type");
    let is_system =
        kind == Some("system") && str_field(record, "subtype") == Some("task_notification");
    let is_queued =
        kind == Some("queue-operation") && is_task_notification_user_content(record.get("content"));
    if !is_system && !is_queued {
        return None;
    }
    let raw_text = non_empty(record.get("content"));
    let from_raw = |read: &dyn Fn(&str) -> Option<String>| raw_text.as_deref().and_then(read);
    Some(Envelope {
        message_id: non_empty(record.get("uuid")).or_else(|| non_empty(record.get("message_id"))),
        task_id: non_empty(record.get("task_id"))
            .or_else(|| from_raw(&|text| read_tag(text, "task-id"))),
        tool_use_id: non_empty(record.get("tool_use_id"))
            .or_else(|| from_raw(&read_tool_use_id_tag)),
        status: non_empty(record.get("status"))
            .or_else(|| from_raw(&|text| read_tag(text, "status"))),
        summary: non_empty(record.get("summary"))
            .or_else(|| from_raw(&|text| read_tag(text, "summary"))),
        output_file: non_empty(record.get("output_file"))
            .or_else(|| from_raw(&read_output_file_tag)),
        raw_text,
    })
}

/// `normalizeTaskNotificationCallIdSegment(segment)`: runs outside
/// `[a-zA-Z0-9._:-]` become `_`.
fn normalize_segment(segment: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut in_run = false;
    for character in js_trim(segment).chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-') {
            normalized.push(character);
            in_run = false;
        } else if !in_run {
            normalized.push('_');
            in_run = true;
        }
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn build_call_id(envelope: &Envelope) -> String {
    if let Some(segment) = envelope.message_id.as_deref().and_then(normalize_segment) {
        return format!("task_notification_{segment}");
    }
    if let Some(segment) = envelope.task_id.as_deref().and_then(normalize_segment) {
        return format!("task_notification_{segment}");
    }
    let seed = [
        &envelope.status,
        &envelope.summary,
        &envelope.output_file,
        &envelope.raw_text,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect::<Vec<_>>()
    .join("|");
    let seed = if seed.is_empty() {
        "task_notification".to_owned()
    } else {
        seed
    };
    // `createHash("sha1").update(seed)` hashes the UTF-8 encoding, with lone
    // surrogates written as U+FFFD.
    let bytes: Vec<u8> = String::from_utf16_lossy(
        &spocky_contracts::js_value::js_text_utf16(&seed).collect::<Vec<_>>(),
    )
    .into_bytes();
    let digest = Sha1::digest(&bytes);
    let hex = digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!("task_notification_{}", &hex[..12])
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn to_tool_call(envelope: &Envelope) -> JsValue {
    let normalized_status = envelope.status.as_deref().map(str::to_lowercase);
    let label = match (&envelope.summary, &envelope.status) {
        (Some(summary), _) => summary.clone(),
        (None, Some(status)) => format!("Background task {}", status.to_lowercase()),
        (None, None) => "Background task notification".to_owned(),
    };
    let mut detail = JsObject::new();
    detail.insert("type", text("plain_text"));
    detail.insert("label", text(&label));
    detail.insert("icon", text("wrench"));
    if let Some(detail_text) = envelope.raw_text.as_ref().or(envelope.summary.as_ref()) {
        detail.insert("text", text(detail_text));
    }
    let mut metadata = JsObject::new();
    metadata.insert("synthetic", JsValue::Bool(true));
    metadata.insert("source", text("claude_task_notification"));
    for (key, value) in [
        ("taskId", &envelope.task_id),
        ("toolUseId", &envelope.tool_use_id),
        ("status", &envelope.status),
        ("outputFile", &envelope.output_file),
    ] {
        if let Some(value) = value {
            metadata.insert(key, text(value));
        }
    }
    let mut item = JsObject::new();
    item.insert("type", text("tool_call"));
    item.insert("callId", text(&build_call_id(envelope)));
    item.insert("name", text("task_notification"));
    item.insert("detail", JsValue::Object(detail));
    item.insert("metadata", JsValue::Object(metadata));
    match normalized_status.as_deref() {
        Some("failed" | "error") => {
            let mut error = JsObject::new();
            error.insert(
                "message",
                text(
                    envelope
                        .summary
                        .as_deref()
                        .unwrap_or("Background task failed"),
                ),
            );
            item.insert("status", text("failed"));
            item.insert("error", JsValue::Object(error));
        }
        Some("canceled" | "cancelled") => {
            item.insert("status", text("canceled"));
            item.insert("error", JsValue::Null);
        }
        _ => {
            item.insert("status", text("completed"));
            item.insert("error", JsValue::Null);
        }
    }
    JsValue::Object(item)
}

/// `isTaskNotificationUserContent(content)`.
#[must_use]
pub fn is_task_notification_user_content(content: Option<&JsValue>) -> bool {
    extract_user_content_text(content).is_some_and(|text| text.contains(TASK_NOTIFICATION_MARKER))
}

/// `mapTaskNotificationUserContentToToolCall({ content, messageId })`.
#[must_use]
pub fn map_user_content_to_tool_call(
    content: Option<&JsValue>,
    message_id: Option<&str>,
) -> Option<JsValue> {
    parse_from_user_content(content, message_id).map(|envelope| to_tool_call(&envelope))
}

/// `mapTaskNotificationSystemRecordToToolCall(record)`.
#[must_use]
pub fn map_system_record_to_tool_call(record: &JsValue) -> Option<JsValue> {
    parse_from_system_record(record).map(|envelope| to_tool_call(&envelope))
}

/// `readTaskNotificationToolUseIdFromHistoryRecord(record)`.
#[must_use]
pub fn read_tool_use_id_from_history_record(value: &JsValue) -> Option<String> {
    let record = history_record(value)?;
    if str_field(record, "type") == Some("user")
        && let Some(message) = record
            .get("message")
            .filter(|value| !matches!(value, JsValue::Undefined))
    {
        let message_id = str_field(record, "uuid").or_else(|| str_field(record, "message_id"));
        return parse_from_user_content(message.get("content"), message_id)
            .and_then(|envelope| envelope.tool_use_id);
    }
    parse_from_system_record(value).and_then(|envelope| envelope.tool_use_id)
}
