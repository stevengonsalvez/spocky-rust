//! `sidechain-tracker.ts`: maps frames from inside a subagent (sidechain)
//! to the child's timeline, the parent Task card, and, for Claude releases
//! without the task protocol, descriptor upserts.
//!
//! Action summaries use `buildToolCallDisplayModel(...).summary` from
//! `@getpaseo/protocol/tool-call-display`; only the summary is ported, since
//! the tracker reads nothing else.

use std::collections::{HashMap, HashSet};

use spocky_contracts::js_value::{JsObject, JsValue, js_text_from_utf16, js_text_utf16};
use spocky_contracts::text::{is_js_whitespace, js_trim};
use spocky_session::agent_sdk::AgentError;

use crate::tool_call_mapper::{MapperParams, map_completed, map_failed, map_running};

const MAX_SUB_AGENT_LOG_ENTRIES: usize = 200;
const MAX_SUB_AGENT_SUMMARY_CHARS: usize = 160;

#[derive(Debug, Clone)]
struct ActionEntry {
    tool_name: String,
    input: JsValue,
    summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ActivityState {
    name: Option<String>,
    sub_agent_type: Option<String>,
    description: Option<String>,
    actions: Vec<ActionEntry>,
    action_keys: Vec<String>,
    action_index_by_key: HashMap<String, usize>,
    completed_action_keys: HashSet<String>,
}

struct ActionCandidate {
    key: String,
    tool_name: String,
    input: JsValue,
}

fn read_trimmed(value: Option<&JsValue>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn is_content_chunk(value: &JsValue) -> bool {
    value.get("type").is_some_and(JsValue::is_string)
}

fn chunk_type(value: &JsValue) -> &str {
    value
        .get("type")
        .and_then(JsValue::as_str)
        .unwrap_or_default()
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn is_tool_use_type(kind: &str) -> bool {
    matches!(kind, "tool_use" | "mcp_tool_use" | "server_tool_use")
}

/// `normalizeSubAgentText(value)`: whitespace runs collapse to one space,
/// then the text is cut to 160 code units plus `...`.
fn normalize_sub_agent_text(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    let mut normalized = String::with_capacity(trimmed.len());
    let mut in_space = false;
    for character in trimmed.chars() {
        if is_js_whitespace(character) {
            if !in_space {
                normalized.push(' ');
            }
            in_space = true;
        } else {
            normalized.push(character);
            in_space = false;
        }
    }
    let units: Vec<u16> = js_text_utf16(&normalized).collect();
    if units.len() <= MAX_SUB_AGENT_SUMMARY_CHARS {
        return Some(normalized);
    }
    Some(format!(
        "{}...",
        js_text_from_utf16(&units[..MAX_SUB_AGENT_SUMMARY_CHARS])
    ))
}

/// `buildToolCallDisplayModel(item).summary` for a mapped tool call.
fn display_summary(item: &JsValue) -> Option<String> {
    let detail = item.get("detail")?;
    let detail_type = detail.get("type").and_then(JsValue::as_str);
    let read = |key: &str| {
        detail
            .get(key)
            .and_then(JsValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let lower_name = js_trim(
        item.get("name")
            .and_then(JsValue::as_str)
            .unwrap_or_default(),
    )
    .to_lowercase();
    let override_summary = if detail_type == Some("unknown") && lower_name == "task" {
        item.get("metadata")
            .and_then(|metadata| metadata.get("subAgentActivity"))
            .and_then(JsValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    } else if lower_name == "terminal" && detail_type == Some("plain_text") {
        read("label")
    } else {
        None
    };
    let canonical = match detail_type {
        Some("shell") => detail
            .get("command")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        Some("read" | "edit" | "write") => detail
            .get("filePath")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        Some("search") => detail
            .get("query")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        Some("fetch") => detail
            .get("url")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        Some("sub_agent") => read("description"),
        Some("plain_text") => detail
            .get("label")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        _ => None,
    };
    override_summary
        .or(canonical)
        .filter(|summary| !summary.is_empty())
}

/// Reads a parent Task call's input by its `tool_use` id.
pub type TaskInputLookup<'a> = &'a dyn Fn(&str) -> Option<JsObject>;

/// `ClaudeSidechainTracker`; the callbacks are passed per call.
#[derive(Debug, Default)]
pub struct ClaudeSidechainTracker {
    /// `activeSidechains`, in insertion order.
    active: Vec<(String, ActivityState)>,
}

/// The callbacks the baseline gives the tracker's constructor.
pub struct TrackerContext<'a> {
    pub get_tool_input: TaskInputLookup<'a>,
    pub is_descriptor_owned_elsewhere: bool,
    pub needs_synthetic_parent_tool_card: &'a dyn Fn(&str) -> bool,
}

fn upsert_event(id: &str, state: &ActivityState, status: &str) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", text("upsert"));
    event.insert("id", text(id));
    event.insert(
        "title",
        text(
            state
                .name
                .as_deref()
                .or(state.sub_agent_type.as_deref())
                .unwrap_or("Claude subagent"),
        ),
    );
    event.insert(
        "description",
        state.description.as_deref().map_or(JsValue::Null, text),
    );
    event.insert("status", text(status));
    event.insert("toolCallId", text(id));
    provider_subagent(JsValue::Object(event))
}

fn provider_subagent(event: JsValue) -> JsValue {
    let mut wrapped = JsObject::new();
    wrapped.insert("type", text("provider_subagent"));
    wrapped.insert("provider", text("claude"));
    wrapped.insert("event", event);
    JsValue::Object(wrapped)
}

impl ClaudeSidechainTracker {
    fn state_mut(&mut self, id: &str) -> &mut ActivityState {
        let position =
            if let Some(position) = self.active.iter().position(|(existing, _)| existing == id) {
                position
            } else {
                self.active.push((id.to_owned(), ActivityState::default()));
                self.active.len() - 1
            };
        &mut self.active[position].1
    }

    /// `handleMessage(message, parentToolUseId)`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper.
    pub fn handle_message(
        &mut self,
        message: &JsValue,
        parent_tool_use_id: &str,
        context: &TrackerContext<'_>,
    ) -> Result<Vec<JsValue>, AgentError> {
        let task_input = (context.get_tool_input)(parent_tool_use_id);
        let state = self.state_mut(parent_tool_use_id);
        let context_updated = update_context_from_task_input(state, task_input.as_ref());
        let candidates = extract_action_candidates(message);
        let mut child_items = extract_timeline_items(message);
        child_items.extend(extract_tool_results(message, state)?);
        let mut action_updated = false;
        for action in candidates {
            if state.completed_action_keys.contains(&action.key) {
                continue;
            }
            if append_action(state, &action)? {
                action_updated = true;
                if let Some(tool_call) = map_running(&MapperParams {
                    call_id: Some(&action.key),
                    name: &action.tool_name,
                    input: Some(&action.input),
                    output: Some(&JsValue::Null),
                    metadata: None,
                })? {
                    child_items.push(tool_call);
                }
            }
        }
        if !context_updated && !action_updated && child_items.is_empty() {
            return Ok(Vec::new());
        }
        let Some(tool_call) = map_running(&MapperParams {
            call_id: Some(parent_tool_use_id),
            name: "Task",
            input: Some(&JsValue::Null),
            output: Some(&JsValue::Null),
            metadata: None,
        })?
        else {
            return Ok(Vec::new());
        };
        let mut detail = JsObject::new();
        detail.insert("type", text("sub_agent"));
        if let Some(sub_agent_type) = &state.sub_agent_type {
            detail.insert("subAgentType", text(sub_agent_type));
        }
        if let Some(description) = &state.description {
            detail.insert("description", text(description));
        }
        detail.insert(
            "log",
            text(
                &state
                    .actions
                    .iter()
                    .map(|action| match &action.summary {
                        Some(summary) => format!("[{}] {summary}", action.tool_name),
                        None => format!("[{}]", action.tool_name),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        );
        detail.insert("actions", JsValue::Array(Vec::new()));
        let state = state.clone();
        let mut events = Vec::new();
        if !context.is_descriptor_owned_elsewhere {
            events.push(upsert_event(parent_tool_use_id, &state, "running"));
        }
        for item in child_items {
            let mut event = JsObject::new();
            event.insert("type", text("timeline"));
            event.insert("id", text(parent_tool_use_id));
            event.insert("item", item);
            events.push(provider_subagent(JsValue::Object(event)));
        }
        if (context.needs_synthetic_parent_tool_card)(parent_tool_use_id) {
            let mut card = tool_call.as_object().cloned().unwrap_or_default();
            card.insert("detail", JsValue::Object(detail));
            let mut event = JsObject::new();
            event.insert("type", text("timeline"));
            event.insert("item", JsValue::Object(card));
            event.insert("provider", text("claude"));
            events.push(JsValue::Object(event));
        }
        Ok(events)
    }

    /// `finishAll(status)`.
    pub fn finish_all(&mut self, status: &str, descriptor_owned_elsewhere: bool) -> Vec<JsValue> {
        let active = std::mem::take(&mut self.active);
        if descriptor_owned_elsewhere {
            return Vec::new();
        }
        active
            .iter()
            .map(|(id, state)| upsert_event(id, state, status))
            .collect()
    }

    /// `finish(id, status)`.
    pub fn finish(
        &mut self,
        id: &str,
        status: &str,
        descriptor_owned_elsewhere: bool,
    ) -> Vec<JsValue> {
        let Some(position) = self.active.iter().position(|(existing, _)| existing == id) else {
            return Vec::new();
        };
        let (_, state) = self.active.remove(position);
        if descriptor_owned_elsewhere {
            return Vec::new();
        }
        vec![upsert_event(id, &state, status)]
    }

    /// `clear()`.
    pub fn clear(&mut self) {
        self.active.clear();
    }
}

fn update_context_from_task_input(state: &mut ActivityState, input: Option<&JsObject>) -> bool {
    let read = |key: &str| {
        normalize_sub_agent_text(
            input
                .and_then(|input| input.get(key))
                .and_then(JsValue::as_str),
        )
    };
    let mut changed = false;
    for (next, slot) in [
        (read("name"), &mut state.name),
        (read("subagent_type"), &mut state.sub_agent_type),
        (read("description"), &mut state.description),
    ] {
        if let Some(next) = next
            && slot.as_ref() != Some(&next)
        {
            *slot = Some(next);
            changed = true;
        }
    }
    changed
}

fn extract_timeline_items(message: &JsValue) -> Vec<JsValue> {
    if message.get("type").and_then(JsValue::as_str) != Some("assistant") {
        return Vec::new();
    }
    let inner = message.get("message");
    let Some(content) = inner
        .and_then(|inner| inner.get("content"))
        .and_then(JsValue::as_array)
    else {
        return Vec::new();
    };
    let message_id = read_trimmed(inner.and_then(|inner| inner.get("id")));
    let mut items = Vec::new();
    for block in content.iter().filter(|block| is_content_chunk(block)) {
        match chunk_type(block) {
            "text" => {
                if let Some(body) = read_trimmed(block.get("text")) {
                    let mut item = JsObject::new();
                    item.insert("type", text("assistant_message"));
                    item.insert("text", text(&body));
                    if let Some(id) = &message_id {
                        item.insert("messageId", text(id));
                    }
                    items.push(JsValue::Object(item));
                }
            }
            "thinking" => {
                if let Some(body) = read_trimmed(block.get("thinking")) {
                    let mut item = JsObject::new();
                    item.insert("type", text("reasoning"));
                    item.insert("text", text(&body));
                    items.push(JsValue::Object(item));
                }
            }
            _ => {}
        }
    }
    items
}

fn extract_tool_results(
    message: &JsValue,
    state: &mut ActivityState,
) -> Result<Vec<JsValue>, AgentError> {
    let Some(content) = message
        .get("message")
        .and_then(|inner| inner.get("content"))
        .and_then(JsValue::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    for block in content {
        if !is_content_chunk(block) || !chunk_type(block).ends_with("tool_result") {
            continue;
        }
        let Some(call_id) = read_trimmed(block.get("tool_use_id")) else {
            continue;
        };
        if state.completed_action_keys.contains(&call_id) {
            continue;
        }
        let action = state
            .action_index_by_key
            .get(&call_id)
            .and_then(|index| state.actions.get(*index));
        let Some(tool_name) = action
            .map(|action| action.tool_name.clone())
            .or_else(|| read_trimmed(block.get("tool_name")))
        else {
            continue;
        };
        let input = action
            .map(|action| action.input.clone())
            .filter(|input| !matches!(input, JsValue::Undefined | JsValue::Null))
            .unwrap_or(JsValue::Null);
        let output = block
            .get("content")
            .filter(|content| !matches!(content, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or(JsValue::Null);
        let params = MapperParams {
            call_id: Some(&call_id),
            name: &tool_name,
            input: Some(&input),
            output: Some(&output),
            metadata: None,
        };
        let tool_call = if spocky_contracts::js::truthy(block.get("is_error")) {
            map_failed(&params, Some(block))?
        } else {
            map_completed(&params)?
        };
        if let Some(tool_call) = tool_call {
            state.completed_action_keys.insert(call_id);
            items.push(tool_call);
        }
    }
    Ok(items)
}

fn extract_action_candidates(message: &JsValue) -> Vec<ActionCandidate> {
    let nullish_input = |block: &JsValue| {
        block
            .get("input")
            .filter(|input| !matches!(input, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or(JsValue::Null)
    };
    match message.get("type").and_then(JsValue::as_str) {
        Some("assistant") => {
            let Some(content) = message
                .get("message")
                .and_then(|inner| inner.get("content"))
                .and_then(JsValue::as_array)
            else {
                return Vec::new();
            };
            let mut actions = Vec::new();
            for block in content {
                if !is_content_chunk(block) || !is_tool_use_type(chunk_type(block)) {
                    continue;
                }
                let Some(name) = block.get("name").and_then(JsValue::as_str) else {
                    continue;
                };
                let key = read_trimmed(block.get("id"))
                    .unwrap_or_else(|| format!("assistant:{name}:{}", actions.len()));
                actions.push(ActionCandidate {
                    key,
                    tool_name: name.to_owned(),
                    input: nullish_input(block),
                });
            }
            actions
        }
        Some("stream_event") => {
            let Some(event) = message.get("event") else {
                return Vec::new();
            };
            if event.get("type").and_then(JsValue::as_str) != Some("content_block_start") {
                return Vec::new();
            }
            let Some(block) = event
                .get("content_block")
                .filter(|block| is_content_chunk(block))
            else {
                return Vec::new();
            };
            if !is_tool_use_type(chunk_type(block)) {
                return Vec::new();
            }
            let Some(name) = block.get("name").and_then(JsValue::as_str) else {
                return Vec::new();
            };
            let index = event.get("index").and_then(JsValue::as_f64).map_or_else(
                || "0".to_owned(),
                |index| spocky_contracts::js::js_string(Some(&JsValue::Number(index))),
            );
            let key =
                read_trimmed(block.get("id")).unwrap_or_else(|| format!("stream:{name}:{index}"));
            vec![ActionCandidate {
                key,
                tool_name: name.to_owned(),
                input: nullish_input(block),
            }]
        }
        Some("tool_progress") => {
            let Some(tool_name) = read_trimmed(message.get("tool_name")) else {
                return Vec::new();
            };
            let key = read_trimmed(message.get("tool_use_id"))
                .unwrap_or_else(|| format!("progress:{tool_name}"));
            vec![ActionCandidate {
                key,
                tool_name,
                input: JsValue::Null,
            }]
        }
        _ => Vec::new(),
    }
}

fn derive_action_summary(tool_name: &str, input: &JsValue) -> Result<Option<String>, AgentError> {
    let call_id = format!("sub-agent-summary-{tool_name}");
    let Some(item) = map_running(&MapperParams {
        call_id: Some(&call_id),
        name: tool_name,
        input: Some(input),
        output: Some(&JsValue::Null),
        metadata: None,
    })?
    else {
        return Ok(None);
    };
    Ok(normalize_sub_agent_text(display_summary(&item).as_deref()))
}

fn append_action(
    state: &mut ActivityState,
    candidate: &ActionCandidate,
) -> Result<bool, AgentError> {
    let Some(tool_name) = read_trimmed(Some(&JsValue::String(candidate.tool_name.clone()))) else {
        return Ok(false);
    };
    let summary = derive_action_summary(&tool_name, &candidate.input)?;
    if let Some(index) = state.action_index_by_key.get(&candidate.key).copied() {
        let Some(existing) = state.actions.get(index) else {
            return Ok(false);
        };
        let next_summary = existing.summary.clone().or(summary);
        if existing.tool_name == tool_name && existing.summary == next_summary {
            return Ok(false);
        }
        let input = if matches!(existing.input, JsValue::Undefined | JsValue::Null) {
            candidate.input.clone()
        } else {
            existing.input.clone()
        };
        state.actions[index] = ActionEntry {
            tool_name,
            input,
            summary: next_summary,
        };
        return Ok(true);
    }
    state.actions.push(ActionEntry {
        tool_name,
        input: candidate.input.clone(),
        summary,
    });
    state.action_keys.push(candidate.key.clone());
    while state.actions.len() > MAX_SUB_AGENT_LOG_ENTRIES {
        state.actions.remove(0);
        let removed = state.action_keys.remove(0);
        if !removed.is_empty() {
            state.completed_action_keys.remove(&removed);
        }
    }
    state.action_index_by_key.clear();
    for (index, key) in state.action_keys.iter().enumerate() {
        if !key.is_empty() {
            state.action_index_by_key.insert(key.clone(), index);
        }
    }
    Ok(true)
}
