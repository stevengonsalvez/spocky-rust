//! `tool-call-mapper.ts`: Claude tool uses and results to `tool_call`
//! timeline items.

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::json::PROTO_KEY;
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::AgentError;

use crate::tool_call_detail::derive_claude_tool_detail;

/// `MapperParams`. `None` stands for an absent or `undefined` member.
#[derive(Debug, Clone, Copy, Default)]
pub struct MapperParams<'a> {
    /// `callId` (`string | null`).
    pub call_id: Option<&'a str>,
    pub name: &'a str,
    pub input: Option<&'a JsValue>,
    pub output: Option<&'a JsValue>,
    pub metadata: Option<&'a JsObject>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    Completed,
    Failed,
    Canceled,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Clone, Copy)]
enum ToolKind {
    Shell,
    Read,
    Write,
    Edit,
    Search,
    Fetch,
    Speak,
    Unknown,
}

fn resolve_tool_kind(name: &str) -> ToolKind {
    match name {
        "Bash" | "bash" | "shell" | "exec_command" => ToolKind::Shell,
        "Read" | "read" | "read_file" | "view_file" => ToolKind::Read,
        "Write" | "write" | "write_file" | "create_file" => ToolKind::Write,
        "Edit" | "MultiEdit" | "multi_edit" | "edit" | "apply_patch" | "apply_diff"
        | "str_replace_editor" => ToolKind::Edit,
        "WebSearch" | "web_search" | "search" | "Grep" | "grep" | "Glob" | "glob" => {
            ToolKind::Search
        }
        "WebFetch" | "web_fetch" | "WebFetchTool" | "web_fetch_tool" | "webfetch" => {
            ToolKind::Fetch
        }
        _ if is_speak_tool_name(name) => ToolKind::Speak,
        _ => ToolKind::Unknown,
    }
}

/// `isSpeakToolName(name)` from `@getpaseo/protocol/tool-name-normalization`:
/// the last `/[a-z0-9]+/g` token of the lowercased, trimmed name is `speak`.
#[must_use]
pub fn is_speak_tool_name(name: &str) -> bool {
    let normalized = js_trim(name).to_lowercase();
    normalized
        .split(|character: char| !(character.is_ascii_lowercase() || character.is_ascii_digit()))
        .rfind(|token| !token.is_empty())
        == Some("speak")
}

fn resolve_detail_name(kind: ToolKind, name: &str) -> &str {
    match kind {
        ToolKind::Shell => "shell",
        ToolKind::Read => "read_file",
        ToolKind::Write => "write_file",
        ToolKind::Edit => "apply_patch",
        ToolKind::Speak => "speak",
        ToolKind::Search | ToolKind::Fetch | ToolKind::Unknown => name,
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn map_tool_call(
    params: &MapperParams<'_>,
    status: Status,
    error: Option<&JsValue>,
) -> Result<Option<JsValue>, AgentError> {
    // `ClaudeRawToolCallSchema`: a non-empty name.
    if params.name.is_empty() {
        return Ok(None);
    }
    let Some(call_id) = params
        .call_id
        .filter(|call_id| !js_trim(call_id).is_empty())
    else {
        return Ok(None);
    };
    let trimmed_name = js_trim(params.name);
    let nullish = |value: Option<&JsValue>| {
        value
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .cloned()
    };
    if trimmed_name == "ExitPlanMode"
        && let Some(plan) = params
            .input
            .and_then(JsValue::as_object)
            .and_then(|input| input.get("plan"))
            .and_then(JsValue::as_str)
    {
        let mut metadata = JsObject::new();
        if let Some(raw) = params.metadata {
            for (key, value) in raw.iter() {
                if key != PROTO_KEY {
                    metadata.insert(key, value.clone());
                }
            }
        }
        if matches!(status, Status::Completed | Status::Failed) {
            metadata.insert("approved", JsValue::Bool(status == Status::Completed));
        }
        let mut detail = JsObject::new();
        detail.insert("type", text("plan"));
        detail.insert("text", text(plan));
        let mut item = JsObject::new();
        item.insert("type", text("tool_call"));
        item.insert("callId", text(call_id));
        item.insert(
            "name",
            text(if status == Status::Running {
                trimmed_name
            } else {
                "plan_approval"
            }),
        );
        let shown = if status == Status::Failed {
            Status::Completed
        } else {
            status
        };
        item.insert("status", text(shown.as_str()));
        item.insert("error", JsValue::Null);
        item.insert("detail", JsValue::Object(detail));
        item.insert("metadata", JsValue::Object(metadata));
        return Ok(Some(JsValue::Object(item)));
    }
    let kind = resolve_tool_kind(trimmed_name);
    let name = if matches!(kind, ToolKind::Speak) {
        "speak"
    } else {
        trimmed_name
    };
    let input = nullish(params.input).unwrap_or(JsValue::Null);
    let output = nullish(params.output).unwrap_or(JsValue::Null);
    let detail =
        derive_claude_tool_detail(resolve_detail_name(kind, name), Some(&input), Some(&output))?;
    let mut item = JsObject::new();
    item.insert("type", text("tool_call"));
    item.insert("callId", text(call_id));
    item.insert("name", text(name));
    item.insert("detail", detail);
    item.insert("status", text(status.as_str()));
    if status == Status::Failed {
        let error = nullish(error).unwrap_or_else(|| {
            let mut fallback = JsObject::new();
            fallback.insert("message", text("Tool call failed"));
            JsValue::Object(fallback)
        });
        item.insert("error", error);
    } else {
        item.insert("error", JsValue::Null);
    }
    if let Some(metadata) = params.metadata {
        // `z.record(z.string(), z.unknown())` drops an own `__proto__` key.
        let mut kept = JsObject::new();
        for (key, value) in metadata.iter() {
            if key != PROTO_KEY {
                kept.insert(key, value.clone());
            }
        }
        item.insert("metadata", JsValue::Object(kept));
    }
    Ok(Some(JsValue::Object(item)))
}

/// `mapClaudeRunningToolCall(params)`.
///
/// # Errors
///
/// A throw from [`derive_claude_tool_detail`].
pub fn map_running(params: &MapperParams<'_>) -> Result<Option<JsValue>, AgentError> {
    map_tool_call(params, Status::Running, None)
}

/// `mapClaudeCompletedToolCall(params)`.
///
/// # Errors
///
/// A throw from [`derive_claude_tool_detail`].
pub fn map_completed(params: &MapperParams<'_>) -> Result<Option<JsValue>, AgentError> {
    map_tool_call(params, Status::Completed, None)
}

/// `mapClaudeFailedToolCall({ ...params, error })`.
///
/// # Errors
///
/// A throw from [`derive_claude_tool_detail`].
pub fn map_failed(
    params: &MapperParams<'_>,
    error: Option<&JsValue>,
) -> Result<Option<JsValue>, AgentError> {
    map_tool_call(params, Status::Failed, error)
}

/// `mapClaudeCanceledToolCall(params)`.
///
/// # Errors
///
/// A throw from [`derive_claude_tool_detail`].
pub fn map_canceled(params: &MapperParams<'_>) -> Result<Option<JsValue>, AgentError> {
    map_tool_call(params, Status::Canceled, None)
}
