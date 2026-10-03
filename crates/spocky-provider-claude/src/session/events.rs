//! SDK messages to stream events: system, user, assistant, stream, and
//! result frames, task protocol events, and sidechain frames.

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::AgentError;

use super::options::provider_subagent;
use super::rewind::session_changed_notice;
use super::timeline::BlockOptions;
use super::{ClaudeSession, text};
use crate::model_manifest::normalize_claude_runtime_model_id;
use crate::sidechain_tracker::TrackerContext;
use crate::subagents::observation::{SubagentObservation, fold_subagent_observations};
use crate::task_notification::{map_system_record_to_tool_call, map_user_content_to_tool_call};
use crate::tool_call_mapper::{MapperParams, map_running};
use crate::transcript::{
    is_subagent_tool_name, is_synthetic_user_entry, is_transcript_noise_text,
    read_command_lifecycle, read_compaction_metadata, read_parent_tool_use_id,
};

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `{ type: "timeline", item, provider: "claude" }`.
fn timeline_item_first(item: JsValue) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", text("timeline"));
    event.insert("item", item);
    event.insert("provider", text("claude"));
    JsValue::Object(event)
}

/// `{ type: "timeline", provider: "claude", item }`.
fn timeline_provider_first(item: JsValue) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", text("timeline"));
    event.insert("provider", text("claude"));
    event.insert("item", item);
    JsValue::Object(event)
}

fn thread_started(session_id: &str) -> JsValue {
    let mut event = JsObject::new();
    event.insert("type", text("thread_started"));
    event.insert("provider", text("claude"));
    event.insert("sessionId", text(session_id));
    JsValue::Object(event)
}

/// `extractSessionIdRaw(...)` over a message record.
fn extract_session_id(message: &JsValue) -> String {
    let session = message.get("session").filter(|session| session.is_object());
    let raw = str_of(message, "session_id")
        .or_else(|| str_of(message, "sessionId"))
        .or_else(|| session.and_then(|session| str_of(session, "id")))
        .unwrap_or_default();
    spocky_contracts::text::js_trim(raw).to_owned()
}

/// A session id capture: the `thread_started` id and a change notice.
struct SessionCapture {
    thread_started_session_id: Option<String>,
    notice: Option<JsValue>,
}

/// `/\baborted\b/i` over a result's `errors`.
fn is_abort_error(message: &JsValue) -> bool {
    message
        .get("errors")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .any(|error| error.as_str().is_some_and(contains_aborted_word))
}

fn contains_aborted_word(error: &str) -> bool {
    contains_word_phrase(&error.to_lowercase(), "aborted")
}

/// `/\bphrase\b/i.test(text)` for a lowercase ASCII phrase and lowercased
/// text; `\w` is `[A-Za-z0-9_]` without the `u` flag.
pub(crate) fn contains_word_phrase(lower: &str, phrase: &str) -> bool {
    let word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    lower.match_indices(phrase).any(|(start, matched)| {
        let before = lower[..start].chars().next_back();
        let after = lower[start + matched.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

impl ClaudeSession {
    /// `isAbortError(message)`.
    pub(crate) fn is_abort_error(message: &JsValue) -> bool {
        is_abort_error(message)
    }

    /// `ClaudeTaskProtocolSource.isActive`.
    fn descriptor_owned_elsewhere(&self) -> bool {
        self.task_protocol_source.borrow().is_active()
    }

    /// `translateMessageToEvents(message, options)`.
    ///
    /// # Errors
    ///
    /// A throw from a mapper, or a message the baseline cannot read.
    pub(crate) fn translate_message_to_events(
        &self,
        message: &JsValue,
        options: BlockOptions,
    ) -> Result<Vec<JsValue>, AgentError> {
        if let Some(parent_tool_use_id) = read_parent_tool_use_id(message) {
            return self.translate_sidechain_frame_to_events(message, &parent_tool_use_id);
        }
        let mut events: Vec<JsValue> = Vec::new();
        // `appendTaskStateEvent`.
        let task_item = self.state.borrow_mut().task_state.observe(message);
        if let Some(item) = task_item {
            events.push(timeline_provider_first(item));
        }
        // Subagent identity and lifecycle come from the task protocol.
        let observations = self.task_protocol_source.borrow_mut().observe(message);
        for event in fold_subagent_observations(&observations) {
            events.push(provider_subagent(event));
        }
        for observation in &observations {
            let SubagentObservation::Declared { id, .. } = observation else {
                continue;
            };
            if !self
                .task_protocol_source
                .borrow()
                .needs_synthetic_parent_tool_card(id)
            {
                continue;
            }
            if let Some(card) = Self::build_subagent_tool_call_card(observation)? {
                events.push(card);
            }
        }
        if str_of(message, "type") != Some("system") {
            let capture = self.capture_session_id_from_message(message);
            if let Some(notice) = capture.notice {
                events.push(timeline_provider_first(notice));
            }
            if let Some(session_id) = capture.thread_started_session_id {
                events.push(thread_started(&session_id));
            }
        }
        self.forget_read_steer(message);
        match str_of(message, "type") {
            Some("system") => self.append_system_message_events(message, &mut events),
            Some("user") => {
                self.append_user_message_events(message, &mut events)?;
                self.append_sidechain_result_events(message, &mut events);
            }
            Some("assistant") => {
                let Some(inner) = message.get("message").filter(|inner| inner.is_object()) else {
                    return Err(AgentError {
                        name: "TypeError".to_owned(),
                        message: "Cannot read properties of undefined (reading 'content')"
                            .to_owned(),
                    });
                };
                let items = self.map_blocks_to_timeline(
                    inner.get("content").unwrap_or(&JsValue::Undefined),
                    BlockOptions {
                        user_text: false,
                        ..options
                    },
                )?;
                for item in items {
                    events.push(timeline_item_first(item));
                }
                self.append_sidechain_result_events(message, &mut events);
            }
            Some("stream_event") => {
                self.append_stream_event_events(message, &mut events, options)?;
            }
            Some("result") => self.append_result_events(message, &mut events),
            _ => {}
        }
        Ok(events)
    }

    /// `forgetReadSteer(message)`.
    fn forget_read_steer(&self, message: &JsValue) {
        let Some((command_uuid, state)) = read_command_lifecycle(message) else {
            return;
        };
        if state == "queued" {
            return;
        }
        let mut session = self.state.borrow_mut();
        session
            .queued_steer_uuids
            .retain(|uuid| *uuid != command_uuid);
        session
            .permission_clearing_steer_uuids
            .remove(&command_uuid);
    }

    /// `translateSidechainFrameToEvents(message, parentToolUseId)`.
    fn translate_sidechain_frame_to_events(
        &self,
        message: &JsValue,
        parent_tool_use_id: &str,
    ) -> Result<Vec<JsValue>, AgentError> {
        let canonical = self
            .task_protocol_source
            .borrow()
            .resolve_subagent_id(parent_tool_use_id);
        // Once a CLI announces its tasks it announces all of them: a frame for
        // one that was never declared is work the filter already rejected.
        if self.task_protocol_source.borrow().announces_tasks() && canonical.is_none() {
            return Ok(Vec::new());
        }
        let routed_id = canonical.unwrap_or_else(|| parent_tool_use_id.to_owned());
        let observations = self
            .task_protocol_source
            .borrow_mut()
            .observe_sidechain_frame(message, &routed_id);
        let mut events: Vec<JsValue> = fold_subagent_observations(&observations)
            .into_iter()
            .map(provider_subagent)
            .collect();
        let cache = std::rc::Rc::clone(&self.tool_use_cache);
        let lookup = move |id: &str| {
            cache
                .borrow()
                .iter()
                .find(|(entry_id, _)| entry_id == id)
                .and_then(|(_, entry)| entry.input.clone())
                .and_then(|input| input.as_object().cloned())
        };
        let needs_card = |id: &str| {
            self.task_protocol_source
                .borrow()
                .needs_synthetic_parent_tool_card(id)
        };
        let context = TrackerContext {
            get_tool_input: &lookup,
            is_descriptor_owned_elsewhere: self.descriptor_owned_elsewhere(),
            needs_synthetic_parent_tool_card: &needs_card,
        };
        let tracked = self
            .state
            .borrow_mut()
            .sidechain_tracker
            .handle_message(message, &routed_id, &context)?;
        events.extend(tracked);
        Ok(events)
    }

    /// `buildSubagentToolCallCard(declaration)`.
    fn build_subagent_tool_call_card(
        declaration: &SubagentObservation,
    ) -> Result<Option<JsValue>, AgentError> {
        let SubagentObservation::Declared {
            id,
            title,
            description,
            parent_subagent_id,
            ..
        } = declaration
        else {
            return Ok(None);
        };
        // The launching sidechain already owns and renders this tool call.
        if parent_subagent_id
            .as_ref()
            .is_some_and(|parent| !parent.is_empty())
        {
            return Ok(None);
        }
        let Some(tool_call) = map_running(&MapperParams {
            call_id: Some(id),
            name: "Task",
            input: Some(&JsValue::Null),
            output: Some(&JsValue::Null),
            metadata: None,
        })?
        else {
            return Ok(None);
        };
        let mut detail = JsObject::new();
        detail.insert("type", text("sub_agent"));
        if let Some(title) = title.as_ref().filter(|title| !title.is_empty()) {
            detail.insert("subAgentType", text(title));
        }
        if let Some(description) = description.as_ref().filter(|text| !text.is_empty()) {
            detail.insert("description", text(description));
        }
        detail.insert("log", text(""));
        detail.insert("actions", JsValue::Array(Vec::new()));
        let mut item = spocky_contracts::js::spread(Some(&tool_call));
        item.insert("detail", JsValue::Object(detail));
        Ok(Some(timeline_provider_first(JsValue::Object(item))))
    }

    /// `appendSidechainResultEvents(message, events)`.
    fn append_sidechain_result_events(&self, message: &JsValue, events: &mut Vec<JsValue>) {
        let content = message
            .get("message")
            .filter(|inner| inner.is_object())
            .and_then(|inner| inner.get("content"))
            .and_then(JsValue::as_array);
        let owned = self.descriptor_owned_elsewhere();
        for block in content.unwrap_or_default() {
            if !block.is_object() || str_of(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = str_of(block, "tool_use_id") else {
                continue;
            };
            let status = if spocky_contracts::js::truthy(block.get("is_error")) {
                "failed"
            } else {
                "completed"
            };
            events.extend(self.state.borrow_mut().sidechain_tracker.finish(
                tool_use_id,
                status,
                owned,
            ));
        }
    }

    /// `appendSystemMessageEvents(message, events)`.
    fn append_system_message_events(&self, message: &JsValue, events: &mut Vec<JsValue>) {
        match str_of(message, "subtype") {
            Some("init") => {
                let update = self.handle_system_message(message);
                if let Some(notice) = update.notice {
                    events.push(timeline_provider_first(notice));
                }
                if let Some(session_id) = update.thread_started_session_id {
                    events.push(thread_started(&session_id));
                }
            }
            Some("status") => {
                if str_of(message, "status") == Some("compacting") {
                    let mut state = self.state.borrow_mut();
                    state.compacting = true;
                    // Claude Code repeats this status every 30 seconds until the
                    // compaction finishes; only the first opens a marker.
                    if !state.compaction_marker_open {
                        state.compaction_marker_open = true;
                        let mut item = JsObject::new();
                        item.insert("type", text("compaction"));
                        item.insert("status", text("loading"));
                        events.push(timeline_item_first(JsValue::Object(item)));
                    }
                }
            }
            Some("compact_boundary") => {
                self.state.borrow_mut().compaction_marker_open = false;
                let metadata = read_compaction_metadata(message);
                events.push(timeline_item_first(
                    crate::transcript::completed_compaction_item(message),
                ));
                let post_tokens = metadata.and_then(|(_, _, post)| post);
                let usage_event = self
                    .state
                    .borrow_mut()
                    .context_usage
                    .build_compaction_usage_event(post_tokens);
                events.push(usage_event);
            }
            Some("task_notification") => self.append_task_notification_events(message, events),
            _ => {}
        }
    }

    /// `appendTaskNotificationEvents(message, events)`.
    fn append_task_notification_events(&self, message: &JsValue, events: &mut Vec<JsValue>) {
        let task_use_id = str_of(message, "tool_use_id").filter(|id| !id.is_empty());
        let cached_name = task_use_id
            .and_then(|id| self.cache_get(id))
            .map(|entry| entry.name);
        // The task protocol owns provider-subagent identity; the cache is only
        // a fallback for streams that do not announce tasks.
        if self
            .task_protocol_source
            .borrow()
            .is_declared_task(message.get("task_id"))
            || is_subagent_tool_name(cached_name.as_deref())
        {
            return;
        }
        let item = map_system_record_to_tool_call(message);
        let owner = self
            .task_protocol_source
            .borrow()
            .resolve_task_owner(message.get("task_id"), task_use_id);
        match (item, owner) {
            (Some(item), Some(owner)) => {
                let mut event = JsObject::new();
                event.insert("type", text("timeline"));
                event.insert("id", JsValue::String(owner));
                event.insert("item", item);
                events.push(provider_subagent(JsValue::Object(event)));
            }
            (Some(item), None) => events.push(timeline_item_first(item)),
            _ => {}
        }
    }

    /// `appendUserMessageEvents(message, events)`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper.
    pub(crate) fn append_user_message_events(
        &self,
        message: &JsValue,
        events: &mut Vec<JsValue>,
    ) -> Result<(), AgentError> {
        if is_synthetic_user_entry(message) {
            return Ok(());
        }
        {
            let mut state = self.state.borrow_mut();
            if state.compacting {
                state.compacting = false;
                return Ok(());
            }
        }
        let message_id = str_of(message, "uuid").filter(|id| !id.is_empty());
        if let Some(id) = message_id
            && self.state.borrow().emitted_user_message_ids.contains(id)
        {
            return Ok(());
        }
        self.remember_user_message_id(message_id);
        self.remember_emitted_user_message_id(message_id);
        let content = message
            .get("message")
            .filter(|inner| inner.is_object())
            .and_then(|inner| inner.get("content"));
        if let Some(notification) = map_user_content_to_tool_call(content, message_id) {
            self.append_user_task_notification_event(notification, events);
            return Ok(());
        }
        if let Some(JsValue::String(content)) = content
            && !content.is_empty()
        {
            if !is_transcript_noise_text(content) {
                let mut item = JsObject::new();
                item.insert("type", text("user_message"));
                item.insert("text", text(content));
                if let Some(id) = message_id {
                    item.insert("messageId", text(id));
                }
                events.push(timeline_item_first(JsValue::Object(item)));
            }
            return Ok(());
        }
        if let Some(JsValue::Array(content)) = content {
            self.append_user_content_array_events(content, message_id, events)?;
        }
        Ok(())
    }

    /// `appendUserTaskNotificationEvent(item, events)`.
    fn append_user_task_notification_event(&self, item: JsValue, events: &mut Vec<JsValue>) {
        let metadata = item.get("metadata");
        let task_id = metadata
            .and_then(|metadata| str_of(metadata, "taskId"))
            .map(str::to_owned);
        let tool_use_id = metadata
            .and_then(|metadata| str_of(metadata, "toolUseId"))
            .map(str::to_owned);
        if let Some(task_id) = task_id.as_deref().filter(|id| !id.is_empty()) {
            let task_value = JsValue::String(task_id.to_owned());
            if self
                .task_protocol_source
                .borrow()
                .is_declared_task(Some(&task_value))
            {
                return;
            }
            let owner = self
                .task_protocol_source
                .borrow()
                .resolve_task_owner(Some(&task_value), tool_use_id.as_deref());
            if let Some(owner) = owner {
                let mut event = JsObject::new();
                event.insert("type", text("timeline"));
                event.insert("id", JsValue::String(owner));
                event.insert("item", item);
                events.push(provider_subagent(JsValue::Object(event)));
                return;
            }
        }
        events.push(timeline_item_first(item));
    }

    /// `appendUserContentArrayEvents(content, messageId, events)`.
    fn append_user_content_array_events(
        &self,
        content: &[JsValue],
        message_id: Option<&str>,
        events: &mut Vec<JsValue>,
    ) -> Result<(), AgentError> {
        let items = self.map_blocks_to_timeline(
            &JsValue::Array(content.to_vec()),
            BlockOptions {
                user_text: true,
                ..BlockOptions::default()
            },
        )?;
        for item in items {
            if str_of(&item, "type") == Some("user_message")
                && let Some(id) = message_id
                && !spocky_contracts::js::truthy(item.get("messageId"))
            {
                let mut tagged = spocky_contracts::js::spread(Some(&item));
                tagged.insert("messageId", text(id));
                events.push(timeline_item_first(JsValue::Object(tagged)));
                continue;
            }
            events.push(timeline_item_first(item));
        }
        Ok(())
    }

    /// `appendStreamEventEvents(message, events, options)`.
    fn append_stream_event_events(
        &self,
        message: &JsValue,
        events: &mut Vec<JsValue>,
        options: BlockOptions,
    ) -> Result<(), AgentError> {
        let usage_event = self
            .state
            .borrow_mut()
            .context_usage
            .build_stream_usage_event(message.get("event"));
        if let Some(usage_event) = usage_event {
            events.push(usage_event);
        }
        let event = message.get("event").cloned().unwrap_or(JsValue::Undefined);
        for item in self.map_partial_event(&event, options)? {
            events.push(timeline_item_first(item));
        }
        Ok(())
    }

    /// `appendResultEvents(message, events)`.
    fn append_result_events(&self, message: &JsValue, events: &mut Vec<JsValue>) {
        let usage = self
            .state
            .borrow_mut()
            .context_usage
            .build_result_usage(message);
        let owned = self.descriptor_owned_elsewhere();
        if str_of(message, "subtype") == Some("success") {
            events.extend(
                self.state
                    .borrow_mut()
                    .sidechain_tracker
                    .finish_all("completed", owned),
            );
            // Built-in slash commands run client-side with no model turn:
            // output_tokens is 0 and the text is carried in `result`.
            let result_text = str_of(message, "result")
                .map(|result| spocky_contracts::text::js_trim(result).to_owned())
                .unwrap_or_default();
            let output_tokens = message
                .get("usage")
                .and_then(|usage| usage.get("output_tokens"))
                .and_then(JsValue::as_f64);
            let has_text = self.state.borrow().active_turn_has_assistant_text;
            if !result_text.is_empty() && output_tokens == Some(0.0) && !has_text {
                let mut item = JsObject::new();
                item.insert("type", text("assistant_message"));
                item.insert("text", JsValue::String(result_text));
                item.insert(
                    "messageId",
                    message.get("uuid").cloned().unwrap_or(JsValue::Undefined),
                );
                events.push(timeline_provider_first(JsValue::Object(item)));
            }
            let mut completed = JsObject::new();
            completed.insert("type", text("turn_completed"));
            completed.insert("provider", text("claude"));
            completed.insert("usage", usage.unwrap_or(JsValue::Undefined));
            events.push(JsValue::Object(completed));
            return;
        }
        let errors: Vec<String> = message
            .get("errors")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .map(|error| match error {
                JsValue::Undefined | JsValue::Null => String::new(),
                other => spocky_contracts::js::js_string(Some(other)),
            })
            .collect();
        let error_message = if errors.is_empty() {
            "Claude run failed".to_owned()
        } else {
            errors.join("\n")
        };
        events.extend(
            self.state
                .borrow_mut()
                .sidechain_tracker
                .finish_all("failed", owned),
        );
        events.push(self.build_turn_failed_event(&error_message));
    }

    /// `captureSessionIdFromMessage(message)`.
    fn capture_session_id_from_message(&self, message: &JsValue) -> SessionCapture {
        let session_id = extract_session_id(message);
        if session_id.is_empty() {
            return SessionCapture {
                thread_started_session_id: None,
                notice: None,
            };
        }
        let mut state = self.state.borrow_mut();
        match state.claude_session_id.clone() {
            None => {
                state.claude_session_id = Some(session_id.clone());
                state.pending_fresh_session_id = None;
                state.persistence = None;
                SessionCapture {
                    thread_started_session_id: Some(session_id),
                    notice: None,
                }
            }
            Some(existing) if existing == session_id => {
                state.pending_fresh_session_id = None;
                SessionCapture {
                    thread_started_session_id: None,
                    notice: None,
                }
            }
            Some(old) => {
                // The id changed mid-stream (a hook restarted Claude with a new
                // session): accept it and continue.
                state.claude_session_id = Some(session_id.clone());
                state.pending_fresh_session_id = None;
                state.persistence = None;
                SessionCapture {
                    thread_started_session_id: Some(session_id.clone()),
                    notice: Some(session_changed_notice(&old, &session_id)),
                }
            }
        }
    }

    /// `handleSystemMessage(message)` for an `init` frame.
    fn handle_system_message(&self, message: &JsValue) -> SessionCapture {
        let none = SessionCapture {
            thread_started_session_id: None,
            notice: None,
        };
        if str_of(message, "subtype") != Some("init") {
            return none;
        }
        let new_session_id = extract_session_id(message);
        if new_session_id.is_empty() {
            return none;
        }
        let mut state = self.state.borrow_mut();
        let existing = state.claude_session_id.clone();
        let mut capture = SessionCapture {
            thread_started_session_id: None,
            notice: None,
        };
        match existing {
            None => {
                state.claude_session_id = Some(new_session_id.clone());
                state.pending_fresh_session_id = None;
                capture.thread_started_session_id = Some(new_session_id);
            }
            Some(existing) if existing == new_session_id => {
                state.pending_fresh_session_id = None;
            }
            Some(existing) => {
                state.claude_session_id = Some(new_session_id.clone());
                state.pending_fresh_session_id = None;
                capture.notice = Some(session_changed_notice(&existing, &new_session_id));
                capture.thread_started_session_id = Some(new_session_id);
            }
        }
        state.available_modes = super::default_modes();
        // `message.permissionMode`; an absent one is `undefined` in the baseline.
        let mode = str_of(message, "permissionMode").map(str::to_owned);
        state.current_mode.clone_from(&mode);
        if mode.as_deref() != Some("plan") {
            state.plan_resume_mode = mode;
        }
        state.persistence = None;
        if let Some(model) = str_of(message, "model").filter(|model| !model.is_empty()) {
            let normalized = normalize_claude_runtime_model_id(Some(model));
            if let Some(normalized) = normalized {
                state.last_options_model = Some(normalized);
            } else if state.last_options_model.is_none() {
                state.last_options_model = state
                    .config
                    .get("model")
                    .and_then(JsValue::as_str)
                    .map(str::to_owned);
            }
            state.last_runtime_model = Some(model.to_owned());
            state.cached_runtime_info = None;
        }
        capture
    }

    /// `readMissingResumedConversationError(message)`.
    pub(crate) fn read_missing_resumed_conversation_error(
        &self,
        message: &JsValue,
    ) -> Option<String> {
        if str_of(message, "type") != Some("result")
            || str_of(message, "subtype") != Some("error_during_execution")
        {
            return None;
        }
        let session_id = self
            .state
            .borrow()
            .claude_session_id
            .clone()
            .filter(|id| !id.is_empty())?;
        for entry in message
            .get("errors")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            let Some(entry) = entry.as_str() else {
                continue;
            };
            let Some(rest) = entry.strip_prefix("No conversation found with session ID:") else {
                continue;
            };
            // `\s*(.+)$`: `.` stops at a line terminator, so only single lines match.
            let rest = rest.trim_start_matches(spocky_contracts::text::is_js_whitespace);
            if rest.is_empty() || rest.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
                continue;
            }
            if spocky_contracts::text::js_trim(rest) == session_id {
                return Some(spocky_contracts::text::js_trim(entry).to_owned());
            }
        }
        None
    }
}
