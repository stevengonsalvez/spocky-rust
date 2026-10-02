//! `TimelineAssembler` from `providers/claude/agent.ts`: assistant and
//! reasoning text from partial stream events and whole assistant messages,
//! emitted once per new suffix.

use std::collections::{HashMap, HashSet};

use spocky_contracts::js_value::{JsObject, JsValue, js_text_from_utf16, js_text_utf16};

use crate::transcript::{
    INTERRUPT_TOOL_USE_PLACEHOLDER, is_transcript_noise_text, read_trimmed_string,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FragmentKind {
    Assistant,
    Reasoning,
}

/// One message's text, in UTF-16 units so lengths match `String.length`.
struct MessageState {
    id: String,
    assistant: Vec<u16>,
    reasoning: Vec<u16>,
    emitted_assistant: usize,
    emitted_reasoning: usize,
}

/// `TimelineAssembler`.
#[derive(Default)]
pub struct TimelineAssembler {
    messages: HashMap<String, MessageState>,
    finalized: HashSet<String>,
    active_by_run: HashMap<String, String>,
    synthetic_counter: u64,
}

fn units(text: &str) -> Vec<u16> {
    js_text_utf16(text).collect()
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `extractFragments(content)`.
fn extract_fragments(content: Option<&JsValue>) -> Vec<(FragmentKind, String)> {
    if let Some(JsValue::String(value)) = content {
        return if value.is_empty() {
            Vec::new()
        } else {
            vec![(FragmentKind::Assistant, value.clone())]
        };
    }
    let blocks: Vec<&JsValue> = match content {
        Some(JsValue::Array(items)) => items.iter().collect(),
        Some(other) => vec![other],
        None => Vec::new(),
    };
    let mut fragments = Vec::new();
    for block in blocks {
        let Some(kind) = block.get("type").and_then(JsValue::as_str) else {
            continue;
        };
        if matches!(kind, "text" | "text_delta")
            && let Some(value) = block.get("text").and_then(JsValue::as_str)
            && !value.is_empty()
        {
            fragments.push((FragmentKind::Assistant, value.to_owned()));
        }
        if matches!(kind, "thinking" | "thinking_delta")
            && let Some(value) = block.get("thinking").and_then(JsValue::as_str)
            && !value.is_empty()
        {
            fragments.push((FragmentKind::Reasoning, value.to_owned()));
        }
    }
    fragments
}

impl TimelineAssembler {
    /// `consume({ message, runId, messageIdHint })`.
    pub fn consume(
        &mut self,
        message: &JsValue,
        run_id: Option<&str>,
        hint: Option<&str>,
    ) -> Vec<JsValue> {
        match message.get("type").and_then(JsValue::as_str) {
            Some("assistant") => self.consume_assistant(message, run_id, hint),
            Some("stream_event") => self.consume_stream_event(message, run_id, hint),
            _ => Vec::new(),
        }
    }

    fn consume_assistant(
        &mut self,
        message: &JsValue,
        run_id: Option<&str>,
        hint: Option<&str>,
    ) -> Vec<JsValue> {
        let container = message.get("message");
        let from_message = read_trimmed_string(message.get("message_id"))
            .or_else(|| read_trimmed_string(container.and_then(|inner| inner.get("id"))));
        let Some(message_id) = from_message
            .or_else(|| hint.map(str::to_owned))
            .or_else(|| self.resolve_message_id(run_id, true, None))
        else {
            return Vec::new();
        };
        if self.finalized.contains(&message_id) {
            return Vec::new();
        }
        self.ensure_state(&message_id, run_id);
        let fragments = extract_fragments(container.and_then(|inner| inner.get("content")));
        self.apply_absolute(&message_id, &fragments)
    }

    fn consume_stream_event(
        &mut self,
        message: &JsValue,
        run_id: Option<&str>,
        hint: Option<&str>,
    ) -> Vec<JsValue> {
        let empty = JsValue::Object(JsObject::new());
        let event = message
            .get("event")
            .filter(|event| event.is_object())
            .unwrap_or(&empty);
        let event_type = read_trimmed_string(event.get("type"));
        let event_message_id = read_trimmed_string(event.get("message_id"))
            .or_else(|| {
                read_trimmed_string(
                    event
                        .get("message")
                        .filter(|inner| inner.is_object())
                        .and_then(|inner| inner.get("id")),
                )
            })
            .or_else(|| hint.map(str::to_owned));
        match event_type.as_deref() {
            Some("message_start") => {
                if let Some(id) = self.resolve_message_id(run_id, true, event_message_id) {
                    self.ensure_state(&id, run_id);
                }
                Vec::new()
            }
            Some("message_stop") => {
                match self.resolve_message_id(run_id, false, event_message_id) {
                    Some(id) => self.finalize(&id, run_id),
                    None => Vec::new(),
                }
            }
            Some("content_block_start") => {
                self.consume_delta(event.get("content_block"), run_id, event_message_id)
            }
            Some("content_block_delta") => {
                self.consume_delta(event.get("delta"), run_id, event_message_id)
            }
            _ => Vec::new(),
        }
    }

    fn consume_delta(
        &mut self,
        content: Option<&JsValue>,
        run_id: Option<&str>,
        hint: Option<String>,
    ) -> Vec<JsValue> {
        let fragments = extract_fragments(content);
        if fragments.is_empty() {
            return Vec::new();
        }
        let Some(id) = self.resolve_message_id(run_id, true, hint) else {
            return Vec::new();
        };
        self.ensure_state(&id, run_id);
        if let Some(state) = self.messages.get_mut(&id) {
            for (kind, fragment) in &fragments {
                match kind {
                    FragmentKind::Assistant => state.assistant.extend(units(fragment)),
                    FragmentKind::Reasoning => state.reasoning.extend(units(fragment)),
                }
            }
        }
        self.emit_new_content(&id)
    }

    fn apply_absolute(&mut self, id: &str, fragments: &[(FragmentKind, String)]) -> Vec<JsValue> {
        let joined = |kind: FragmentKind| -> Vec<u16> {
            fragments
                .iter()
                .filter(|(fragment_kind, _)| *fragment_kind == kind)
                .flat_map(|(_, fragment)| units(fragment))
                .collect()
        };
        let assistant = joined(FragmentKind::Assistant);
        let reasoning = joined(FragmentKind::Reasoning);
        if let Some(state) = self.messages.get_mut(id) {
            if !assistant.is_empty() {
                if !assistant.starts_with(&state.assistant) {
                    state.emitted_assistant = 0;
                }
                state.assistant = assistant;
            }
            if !reasoning.is_empty() {
                if !reasoning.starts_with(&state.reasoning) {
                    state.emitted_reasoning = 0;
                }
                state.reasoning = reasoning;
            }
        }
        self.emit_new_content(id)
    }

    fn finalize(&mut self, id: &str, run_id: Option<&str>) -> Vec<JsValue> {
        if !self.messages.contains_key(id) {
            return Vec::new();
        }
        let items = self.emit_new_content(id);
        if let Some(run) = run_id
            && self.active_by_run.get(run).map(String::as_str) == Some(id)
        {
            self.active_by_run.remove(run);
        }
        self.finalized.insert(id.to_owned());
        self.messages.remove(id);
        items
    }

    fn emit_new_content(&mut self, id: &str) -> Vec<JsValue> {
        let Some(state) = self.messages.get_mut(id) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        let next_assistant = js_text_from_utf16(
            state
                .assistant
                .get(state.emitted_assistant..)
                .unwrap_or_default(),
        );
        if !next_assistant.is_empty()
            && next_assistant != INTERRUPT_TOOL_USE_PLACEHOLDER
            && !is_transcript_noise_text(&next_assistant)
        {
            state.emitted_assistant = state.assistant.len();
            let mut item = JsObject::new();
            item.insert("type", text("assistant_message"));
            item.insert("text", JsValue::String(next_assistant));
            item.insert("messageId", text(&state.id));
            items.push(JsValue::Object(item));
        }
        let next_reasoning = js_text_from_utf16(
            state
                .reasoning
                .get(state.emitted_reasoning..)
                .unwrap_or_default(),
        );
        if !next_reasoning.is_empty() {
            state.emitted_reasoning = state.reasoning.len();
            let mut item = JsObject::new();
            item.insert("type", text("reasoning"));
            item.insert("text", JsValue::String(next_reasoning));
            items.push(JsValue::Object(item));
        }
        items
    }

    fn ensure_state(&mut self, id: &str, run_id: Option<&str>) {
        if let Some(run) = run_id {
            self.active_by_run.insert(run.to_owned(), id.to_owned());
        }
        self.messages
            .entry(id.to_owned())
            .or_insert_with(|| MessageState {
                id: id.to_owned(),
                assistant: Vec::new(),
                reasoning: Vec::new(),
                emitted_assistant: 0,
                emitted_reasoning: 0,
            });
    }

    fn resolve_message_id(
        &mut self,
        run_id: Option<&str>,
        create: bool,
        message_id: Option<String>,
    ) -> Option<String> {
        if let Some(id) = message_id.filter(|id| !id.is_empty()) {
            return Some(id);
        }
        if let Some(active) = run_id
            .filter(|run| !run.is_empty())
            .and_then(|run| self.active_by_run.get(run))
        {
            return Some(active.clone());
        }
        if !create {
            return None;
        }
        self.synthetic_counter += 1;
        let synthetic = format!("synthetic-message-{}", self.synthetic_counter);
        if let Some(run) = run_id.filter(|run| !run.is_empty()) {
            self.active_by_run.insert(run.to_owned(), synthetic.clone());
        }
        Some(synthetic)
    }
}
