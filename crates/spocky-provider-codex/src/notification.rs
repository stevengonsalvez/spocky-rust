//! Codex app-server notification parsing.
//!
//! Port of the zod 4 `CodexNotificationSchema` union in pinned Paseo
//! `codex-app-server-agent.ts`. Each known method has a strict payload shape;
//! a payload that fails it becomes `InvalidPayload`, and any other method
//! becomes `UnknownMethod`. zod semantics kept: `.optional()` accepts a
//! missing key but rejects `null`, `.nullable().optional()` accepts both, and
//! a passthrough object outputs its declared keys first, then the remaining
//! keys in input order.

use serde_json::{Map, Value};

/// The `ParsedCodexNotification` union.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedNotification {
    ThreadStarted {
        thread_id: String,
    },
    TurnStarted {
        turn_id: String,
        thread_id: Option<String>,
    },
    TurnCompleted {
        status: String,
        error_message: Option<String>,
        thread_id: Option<String>,
    },
    PlanUpdated {
        plan: Vec<PlanEntry>,
        thread_id: Option<String>,
    },
    DiffUpdated {
        diff: String,
        thread_id: Option<String>,
    },
    TokenUsageUpdated {
        token_usage: Option<Value>,
        thread_id: Option<String>,
    },
    AgentMessageDelta {
        item_id: String,
        delta: String,
        thread_id: Option<String>,
    },
    ReasoningDelta {
        item_id: String,
        delta: String,
        thread_id: Option<String>,
    },
    ItemCompleted {
        source: ItemSource,
        thread_id: Option<String>,
        turn_id: Option<String>,
        item: Map<String, Value>,
    },
    ItemStarted {
        source: ItemSource,
        thread_id: Option<String>,
        turn_id: Option<String>,
        item: Map<String, Value>,
    },
    ExecCommandStarted {
        call_id: Option<String>,
        command: Value,
        cwd: Option<String>,
        thread_id: Option<String>,
    },
    ExecCommandCompleted {
        call_id: Option<String>,
        command: Value,
        cwd: Option<String>,
        output: Option<String>,
        exit_code: Option<Value>,
        success: Option<bool>,
        stderr: Option<String>,
        thread_id: Option<String>,
    },
    ExecCommandOutputDelta {
        call_id: Option<String>,
        stream: Option<String>,
        chunk: Option<String>,
        thread_id: Option<String>,
    },
    TerminalInteraction {
        source: ItemSource,
        call_id: Option<String>,
        process_id: Option<String>,
        stdin: Option<String>,
        thread_id: Option<String>,
    },
    PatchApplyStarted {
        call_id: Option<String>,
        changes: Value,
        thread_id: Option<String>,
    },
    PatchApplyCompleted {
        call_id: Option<String>,
        changes: Value,
        stdout: Option<String>,
        stderr: Option<String>,
        success: Option<bool>,
        thread_id: Option<String>,
    },
    FileChangeOutputDelta {
        item_id: String,
        delta: Option<String>,
        thread_id: Option<String>,
    },
    ThreadRolledBack {
        num_turns: u64,
        thread_id: Option<String>,
    },
    ContextCompacted {
        thread_id: String,
        turn_id: Option<String>,
    },
    InvalidPayload {
        method: String,
        params: Option<Value>,
    },
    UnknownMethod {
        method: String,
        params: Option<Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSource {
    Item,
    CodexEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub step: Option<String>,
    pub status: Option<String>,
}

impl ParsedNotification {
    /// `getCodexNotificationThreadId`: the variant's `threadId`, if it has one.
    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::ThreadStarted { thread_id } | Self::ContextCompacted { thread_id, .. } => {
                Some(thread_id)
            }
            Self::TurnStarted { thread_id, .. }
            | Self::TurnCompleted { thread_id, .. }
            | Self::PlanUpdated { thread_id, .. }
            | Self::DiffUpdated { thread_id, .. }
            | Self::TokenUsageUpdated { thread_id, .. }
            | Self::AgentMessageDelta { thread_id, .. }
            | Self::ReasoningDelta { thread_id, .. }
            | Self::ItemCompleted { thread_id, .. }
            | Self::ItemStarted { thread_id, .. }
            | Self::ExecCommandStarted { thread_id, .. }
            | Self::ExecCommandCompleted { thread_id, .. }
            | Self::ExecCommandOutputDelta { thread_id, .. }
            | Self::TerminalInteraction { thread_id, .. }
            | Self::PatchApplyStarted { thread_id, .. }
            | Self::PatchApplyCompleted { thread_id, .. }
            | Self::FileChangeOutputDelta { thread_id, .. }
            | Self::ThreadRolledBack { thread_id, .. } => thread_id.as_deref(),
            Self::InvalidPayload { .. } | Self::UnknownMethod { .. } => None,
        }
    }
}

/// Field read failure: the value is present but has the wrong type.
struct Mismatch;

type Field<T> = Result<T, Mismatch>;

fn object(value: Option<&Value>) -> Field<&Map<String, Value>> {
    match value {
        Some(Value::Object(map)) => Ok(map),
        _ => Err(Mismatch),
    }
}

fn required_string(map: &Map<String, Value>, key: &str) -> Field<String> {
    match map.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(Mismatch),
    }
}

/// `z.string().optional()`.
fn optional_string(map: &Map<String, Value>, key: &str) -> Field<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Mismatch),
    }
}

/// `z.string().nullable().optional()`.
fn nullable_string(map: &Map<String, Value>, key: &str) -> Field<Option<String>> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Mismatch),
    }
}

/// `z.number().nullable().optional()`.
fn nullable_number(map: &Map<String, Value>, key: &str) -> Field<Option<Value>> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(number @ Value::Number(_)) => Ok(Some(number.clone())),
        Some(_) => Err(Mismatch),
    }
}

fn optional_bool(map: &Map<String, Value>, key: &str) -> Field<Option<bool>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(Mismatch),
    }
}

/// `z.number().int().nonnegative().optional()`.
fn optional_count(map: &Map<String, Value>, key: &str) -> Field<Option<u64>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|float| float.fract() == 0.0 && *float >= 0.0)
                    .and_then(|float| format!("{float:.0}").parse().ok())
            })
            .map(Some)
            .ok_or(Mismatch),
        Some(_) => Err(Mismatch),
    }
}

/// `z.union([z.string(), z.number()]).optional()`, then `String(number)`.
fn optional_process_id(map: &Map<String, Value>, key: &str) -> Field<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(Value::Number(number)) => Ok(Some(js_number_to_string(number))),
        Some(_) => Err(Mismatch),
    }
}

/// `String(n)` for JSON numbers in the safe integer range and plain decimals.
#[must_use]
pub fn js_number_to_string(number: &serde_json::Number) -> String {
    if let Some(integer) = number.as_i64() {
        return integer.to_string();
    }
    if let Some(integer) = number.as_u64() {
        return integer.to_string();
    }
    let float = number.as_f64().unwrap_or(f64::NAN);
    if float.fract() == 0.0 && float.abs() < 1e21 {
        return format!("{float:.0}");
    }
    float.to_string()
}

/// A passthrough object with the declared keys reordered first, as zod 4
/// builds its output.
fn passthrough(map: &Map<String, Value>, declared: &[&str]) -> Map<String, Value> {
    let mut output = Map::new();
    for key in declared {
        if let Some(value) = map.get(*key) {
            output.insert((*key).to_owned(), value.clone());
        }
    }
    for (key, value) in map {
        if !declared.contains(&key.as_str()) {
            output.insert(key.clone(), value.clone());
        }
    }
    output
}

const ITEM_KEYS: &[&str] = &["id", "type"];

fn item_object(map: &Map<String, Value>) -> Field<Map<String, Value>> {
    let item = object(map.get("item"))?;
    optional_string(item, "id")?;
    optional_string(item, "type")?;
    Ok(passthrough(item, ITEM_KEYS))
}

/// Codex event wrapper fields (`threadId`/`thread_id` at both levels).
struct CodexEvent<'a> {
    params: &'a Map<String, Value>,
    msg: &'a Map<String, Value>,
}

impl<'a> CodexEvent<'a> {
    fn parse(params: Option<&'a Value>, msg_type: &[&str]) -> Field<Self> {
        let params = object(params)?;
        optional_string(params, "threadId")?;
        optional_string(params, "thread_id")?;
        let msg = object(params.get("msg"))?;
        optional_string(msg, "threadId")?;
        optional_string(msg, "thread_id")?;
        match msg.get("type") {
            Some(Value::String(kind)) if msg_type.contains(&kind.as_str()) => {}
            _ => return Err(Mismatch),
        }
        Ok(Self { params, msg })
    }

    fn with_turn_fields(self) -> Field<Self> {
        optional_string(self.params, "turnId")?;
        optional_string(self.params, "turn_id")?;
        optional_string(self.msg, "turnId")?;
        optional_string(self.msg, "turn_id")?;
        Ok(self)
    }

    fn first(&self, keys: [&str; 2]) -> Option<String> {
        for map in [self.params, self.msg] {
            for key in keys {
                if let Some(Value::String(text)) = map.get(key) {
                    return Some(text.clone());
                }
            }
        }
        None
    }

    fn thread_id(&self) -> Option<String> {
        self.first(["threadId", "thread_id"])
    }

    fn turn_id(&self) -> Option<String> {
        self.first(["turnId", "turn_id"])
    }

    fn string(&self, key: &str) -> Field<Option<String>> {
        optional_string(self.msg, key)
    }
}

/// Parses one notification exactly as the zod union would.
#[must_use]
pub fn parse_notification(method: &str, params: Option<&Value>) -> ParsedNotification {
    let parsed = match method {
        "thread/started" => thread_started(params),
        "turn/started" => turn_started(params),
        "turn/completed" => turn_completed(params),
        "turn/plan/updated" => plan_updated(params),
        "turn/diff/updated" => diff_updated(params),
        "thread/tokenUsage/updated" => token_usage_updated(params),
        "thread/compacted" => context_compacted(params),
        "item/agentMessage/delta" => text_delta(params, true),
        "item/reasoning/summaryTextDelta" => text_delta(params, false),
        "item/completed" => item_lifecycle(params, true),
        "item/started" => item_lifecycle(params, false),
        "codex/event/item_started" | "codex/event/item_completed" => {
            codex_item_lifecycle(params, method == "codex/event/item_completed")
        }
        "codex/event/exec_command_begin" => exec_command_begin(params),
        "codex/event/exec_command_end" => exec_command_end(params),
        "codex/event/exec_command_output_delta" => exec_command_output_delta(params),
        "codex/event/terminal_interaction" => codex_terminal_interaction(params),
        "item/commandExecution/terminalInteraction" => item_terminal_interaction(params),
        "codex/event/patch_apply_begin" => patch_apply_begin(params),
        "codex/event/patch_apply_end" => patch_apply_end(params),
        "item/fileChange/outputDelta" => file_change_output_delta(params),
        "codex/event/turn_diff" => turn_diff(params),
        "codex/event/turn_aborted" => turn_aborted(params),
        "codex/event/task_complete" => task_complete(params),
        "codex/event/thread_rolled_back" => thread_rolled_back(params),
        _ => {
            return ParsedNotification::UnknownMethod {
                method: method.to_owned(),
                params: params.cloned(),
            };
        }
    };
    parsed.unwrap_or_else(|Mismatch| ParsedNotification::InvalidPayload {
        method: method.to_owned(),
        params: params.cloned(),
    })
}

fn thread_started(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread = object(params.get("thread"))?;
    Ok(ParsedNotification::ThreadStarted {
        thread_id: required_string(thread, "id")?,
    })
}

fn turn_started(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let turn = object(params.get("turn"))?;
    Ok(ParsedNotification::TurnStarted {
        turn_id: required_string(turn, "id")?,
        thread_id,
    })
}

fn turn_completed(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let turn = object(params.get("turn"))?;
    optional_string(turn, "id")?;
    let status = required_string(turn, "status")?;
    let error_message = match turn.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::Object(error)) => optional_string(error, "message")?,
        Some(_) => return Err(Mismatch),
    };
    Ok(ParsedNotification::TurnCompleted {
        status,
        error_message,
        thread_id,
    })
}

fn plan_updated(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let Some(Value::Array(entries)) = params.get("plan") else {
        return Err(Mismatch);
    };
    let mut plan = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = object(Some(entry))?;
        plan.push(PlanEntry {
            step: optional_string(entry, "step")?,
            status: optional_string(entry, "status")?,
        });
    }
    Ok(ParsedNotification::PlanUpdated { plan, thread_id })
}

fn diff_updated(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    Ok(ParsedNotification::DiffUpdated {
        diff: required_string(params, "diff")?,
        thread_id,
    })
}

fn token_usage_updated(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    Ok(ParsedNotification::TokenUsageUpdated {
        token_usage: params.get("tokenUsage").cloned(),
        thread_id,
    })
}

fn context_compacted(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = required_string(params, "threadId")?;
    Ok(ParsedNotification::ContextCompacted {
        thread_id,
        turn_id: optional_string(params, "turnId")?,
    })
}

fn text_delta(params: Option<&Value>, agent_message: bool) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let item_id = required_string(params, "itemId")?;
    let delta = required_string(params, "delta")?;
    Ok(if agent_message {
        ParsedNotification::AgentMessageDelta {
            item_id,
            delta,
            thread_id,
        }
    } else {
        ParsedNotification::ReasoningDelta {
            item_id,
            delta,
            thread_id,
        }
    })
}

fn item_lifecycle(params: Option<&Value>, completed: bool) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let turn_id = optional_string(params, "turnId")?;
    let item = item_object(params)?;
    Ok(lifecycle(
        ItemSource::Item,
        completed,
        thread_id,
        turn_id,
        item,
    ))
}

fn lifecycle(
    source: ItemSource,
    completed: bool,
    thread_id: Option<String>,
    turn_id: Option<String>,
    item: Map<String, Value>,
) -> ParsedNotification {
    if completed {
        ParsedNotification::ItemCompleted {
            source,
            thread_id,
            turn_id,
            item,
        }
    } else {
        ParsedNotification::ItemStarted {
            source,
            thread_id,
            turn_id,
            item,
        }
    }
}

fn codex_item_lifecycle(params: Option<&Value>, completed: bool) -> Field<ParsedNotification> {
    let kind = if completed {
        "item_completed"
    } else {
        "item_started"
    };
    let event = CodexEvent::parse(params, &[kind])?.with_turn_fields()?;
    let item = item_object(event.msg)?;
    Ok(lifecycle(
        ItemSource::CodexEvent,
        completed,
        event.thread_id(),
        event.turn_id(),
        item,
    ))
}

fn exec_command_begin(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["exec_command_begin"])?;
    Ok(ParsedNotification::ExecCommandStarted {
        call_id: event.string("call_id")?,
        command: event.msg.get("command").cloned().unwrap_or(Value::Null),
        cwd: event.string("cwd")?,
        thread_id: event.thread_id(),
    })
}

fn exec_command_end(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["exec_command_end"])?;
    let msg = event.msg;
    let call_id = event.string("call_id")?;
    let cwd = event.string("cwd")?;
    let stdout = optional_string(msg, "stdout")?;
    let stderr = optional_string(msg, "stderr")?;
    let aggregated_snake = nullable_string(msg, "aggregated_output")?;
    let aggregated_camel = nullable_string(msg, "aggregatedOutput")?;
    let formatted = optional_string(msg, "formatted_output")?;
    let exit_snake = nullable_number(msg, "exit_code")?;
    let exit_camel = nullable_number(msg, "exitCode")?;
    let success = optional_bool(msg, "success")?;
    Ok(ParsedNotification::ExecCommandCompleted {
        call_id,
        command: msg.get("command").cloned().unwrap_or(Value::Null),
        cwd,
        output: aggregated_snake
            .or(aggregated_camel)
            .or(formatted)
            .or(stdout),
        exit_code: exit_snake.or(exit_camel),
        success,
        stderr,
        thread_id: event.thread_id(),
    })
}

fn exec_command_output_delta(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["exec_command_output_delta"])?;
    let call_id = event.string("call_id")?;
    let stream = event.string("stream")?;
    let chunk = event.string("chunk")?;
    let delta = event.string("delta")?;
    Ok(ParsedNotification::ExecCommandOutputDelta {
        call_id,
        stream,
        chunk: chunk.or(delta),
        thread_id: event.thread_id(),
    })
}

fn codex_terminal_interaction(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["terminal_interaction"])?;
    Ok(ParsedNotification::TerminalInteraction {
        source: ItemSource::CodexEvent,
        call_id: event.string("call_id")?,
        process_id: optional_process_id(event.msg, "process_id")?,
        stdin: event.string("stdin")?,
        thread_id: event.thread_id(),
    })
}

fn item_terminal_interaction(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let call_id = optional_string(params, "itemId")?;
    let process_id = optional_process_id(params, "processId")?;
    let stdin = optional_string(params, "stdin")?;
    Ok(ParsedNotification::TerminalInteraction {
        source: ItemSource::Item,
        call_id,
        process_id,
        stdin,
        thread_id,
    })
}

fn patch_apply_begin(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["patch_apply_begin"])?;
    Ok(ParsedNotification::PatchApplyStarted {
        call_id: event.string("call_id")?,
        changes: event.msg.get("changes").cloned().unwrap_or(Value::Null),
        thread_id: event.thread_id(),
    })
}

fn patch_apply_end(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["patch_apply_end"])?;
    Ok(ParsedNotification::PatchApplyCompleted {
        call_id: event.string("call_id")?,
        changes: event.msg.get("changes").cloned().unwrap_or(Value::Null),
        stdout: event.string("stdout")?,
        stderr: event.string("stderr")?,
        success: optional_bool(event.msg, "success")?,
        thread_id: event.thread_id(),
    })
}

fn file_change_output_delta(params: Option<&Value>) -> Field<ParsedNotification> {
    let params = object(params)?;
    let thread_id = optional_string(params, "threadId")?;
    let item_id = required_string(params, "itemId")?;
    let delta = optional_string(params, "delta")?;
    let chunk = optional_string(params, "chunk")?;
    Ok(ParsedNotification::FileChangeOutputDelta {
        item_id,
        delta: delta.or(chunk),
        thread_id,
    })
}

fn turn_diff(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["turn_diff"])?;
    let unified = event.string("unified_diff")?;
    let diff = event.string("diff")?;
    Ok(ParsedNotification::DiffUpdated {
        diff: unified.or(diff).unwrap_or_default(),
        thread_id: event.thread_id(),
    })
}

fn turn_aborted(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["turn_aborted"])?;
    event.string("reason")?;
    Ok(ParsedNotification::TurnCompleted {
        status: "interrupted".to_owned(),
        error_message: None,
        thread_id: event.thread_id(),
    })
}

fn task_complete(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["task_complete"])?;
    Ok(ParsedNotification::TurnCompleted {
        status: "completed".to_owned(),
        error_message: None,
        thread_id: event.thread_id(),
    })
}

fn thread_rolled_back(params: Option<&Value>) -> Field<ParsedNotification> {
    let event = CodexEvent::parse(params, &["thread_rolled_back"])?;
    let snake = optional_count(event.msg, "num_turns")?;
    let camel = optional_count(event.msg, "numTurns")?;
    Ok(ParsedNotification::ThreadRolledBack {
        num_turns: snake.or(camel).unwrap_or(0),
        thread_id: event.thread_id(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(method: &str, params: &Value) -> ParsedNotification {
        parse_notification(method, Some(params))
    }

    #[test]
    fn optional_string_rejects_null_like_zod() {
        let parsed = parse(
            "turn/started",
            &json!({"threadId": null, "turn": {"id": "t1"}}),
        );
        assert!(matches!(parsed, ParsedNotification::InvalidPayload { .. }));
    }

    #[test]
    fn missing_params_is_an_invalid_payload_for_known_methods() {
        let parsed = parse_notification("turn/completed", None);
        assert_eq!(
            parsed,
            ParsedNotification::InvalidPayload {
                method: "turn/completed".to_owned(),
                params: None,
            }
        );
    }

    #[test]
    fn unknown_methods_keep_their_params() {
        let parsed = parse("account/rateLimits/updated", &json!({"a": 1}));
        assert_eq!(
            parsed,
            ParsedNotification::UnknownMethod {
                method: "account/rateLimits/updated".to_owned(),
                params: Some(json!({"a": 1})),
            }
        );
    }

    #[test]
    fn turn_completed_accepts_null_error_and_reads_message() {
        assert_eq!(
            parse(
                "turn/completed",
                &json!({"threadId": "th", "turn": {"id": "t", "status": "failed", "error": {"message": "boom"}}})
            ),
            ParsedNotification::TurnCompleted {
                status: "failed".to_owned(),
                error_message: Some("boom".to_owned()),
                thread_id: Some("th".to_owned()),
            }
        );
        assert!(matches!(
            parse(
                "turn/completed",
                &json!({"turn": {"status": "completed", "error": null}})
            ),
            ParsedNotification::TurnCompleted {
                error_message: None,
                thread_id: None,
                ..
            }
        ));
    }

    #[test]
    fn item_passthrough_puts_declared_keys_first() {
        let parsed = parse(
            "item/completed",
            &json!({"item": {"text": "Hi", "type": "agentMessage", "id": "m1"}, "threadId": "th", "turnId": "tu"}),
        );
        let ParsedNotification::ItemCompleted { item, source, .. } = parsed else {
            panic!("expected item_completed");
        };
        assert_eq!(source, ItemSource::Item);
        let keys: Vec<&str> = item.keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "type", "text"]);
    }

    #[test]
    fn codex_event_thread_id_prefers_outer_camel_then_snake_then_msg() {
        let parsed = parse(
            "codex/event/task_complete",
            &json!({"thread_id": "outer", "msg": {"type": "task_complete", "threadId": "inner"}}),
        );
        assert_eq!(parsed.thread_id(), Some("outer"));
        let parsed = parse(
            "codex/event/turn_aborted",
            &json!({"msg": {"type": "turn_aborted", "thread_id": "inner"}}),
        );
        assert_eq!(
            parsed,
            ParsedNotification::TurnCompleted {
                status: "interrupted".to_owned(),
                error_message: None,
                thread_id: Some("inner".to_owned()),
            }
        );
    }

    #[test]
    fn codex_event_with_wrong_msg_type_is_invalid() {
        assert!(matches!(
            parse(
                "codex/event/task_complete",
                &json!({"msg": {"type": "other"}})
            ),
            ParsedNotification::InvalidPayload { .. }
        ));
    }

    #[test]
    fn exec_command_end_output_precedence() {
        let parsed = parse(
            "codex/event/exec_command_end",
            &json!({"msg": {"type": "exec_command_end", "call_id": "c", "stdout": "so", "formatted_output": "fo", "aggregatedOutput": null, "exit_code": null, "exitCode": 2}}),
        );
        let ParsedNotification::ExecCommandCompleted {
            output,
            exit_code,
            command,
            ..
        } = parsed
        else {
            panic!("expected exec_command_completed");
        };
        assert_eq!(output.as_deref(), Some("fo"));
        assert_eq!(exit_code, Some(json!(2)));
        assert_eq!(command, Value::Null);
    }

    #[test]
    fn terminal_interaction_stringifies_numeric_process_ids() {
        let parsed = parse(
            "item/commandExecution/terminalInteraction",
            &json!({"itemId": "i", "processId": 42, "stdin": "ls\n"}),
        );
        assert!(matches!(
            parsed,
            ParsedNotification::TerminalInteraction { process_id: Some(ref id), source: ItemSource::Item, .. } if id == "42"
        ));
    }

    #[test]
    fn thread_rolled_back_requires_nonnegative_integers() {
        assert!(matches!(
            parse(
                "codex/event/thread_rolled_back",
                &json!({"msg": {"type": "thread_rolled_back", "numTurns": 2}})
            ),
            ParsedNotification::ThreadRolledBack { num_turns: 2, .. }
        ));
        assert!(matches!(
            parse(
                "codex/event/thread_rolled_back",
                &json!({"msg": {"type": "thread_rolled_back", "num_turns": -1}})
            ),
            ParsedNotification::InvalidPayload { .. }
        ));
    }
}
