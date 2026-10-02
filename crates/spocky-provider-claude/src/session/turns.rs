//! Turn lifecycle: `startTurn`, `run`, steering, interrupts, autonomous
//! turns, terminal events, runtime exit, slash commands, and rewind.

use std::rc::Rc;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::{is_js_whitespace, js_trim};
use spocky_session::agent_sdk::{AgentError, AgentPromptInput, SteerResult};

use super::options::provider_subagent;
use super::{ClaudeSession, TurnState, text};
use crate::process::ChildExit;
use crate::prompt_attachments::render_prompt_attachment_as_text;
use crate::sdk_query::{ClaudeQuery, PromptInput};
use crate::subagents::observation::fold_subagent_observations;

const REWIND_COMMAND_NAME: &str = "rewind";

/// The CLDR root collation rank of an ASCII character: whitespace, then
/// punctuation, symbols, digits, and letters (case folded).
fn collation_primary(character: char) -> (u8, u32) {
    const PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%";
    const SYMBOLS: &str = "`^+<=>|~$";
    if character.is_whitespace() {
        return (0, u32::from(character));
    }
    if let Some(rank) = PUNCTUATION.find(character) {
        return (1, u32::try_from(rank).unwrap_or(u32::MAX));
    }
    if let Some(rank) = SYMBOLS.find(character) {
        return (2, u32::try_from(rank).unwrap_or(u32::MAX));
    }
    if character.is_ascii_digit() {
        return (3, u32::from(character));
    }
    if character.is_alphabetic() {
        let folded = character.to_lowercase().next().unwrap_or(character);
        return (4, u32::from(folded));
    }
    (5, u32::from(character))
}

/// `a.localeCompare(b)` for the command names Claude Code reports: ASCII
/// names order by the root collation (letters case-insensitively first, then
/// lowercase before uppercase). Other characters order by code point.
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    let primary = |text: &str| text.chars().map(collation_primary).collect::<Vec<_>>();
    primary(left).cmp(&primary(right)).then_with(|| {
        let tertiary = |text: &str| text.chars().map(char::is_uppercase).collect::<Vec<_>>();
        tertiary(left).cmp(&tertiary(right))
    })
}
const CLAUDE_ROOT_ONLY_COMMANDS: [&str; 10] = [
    "clear",
    "compact",
    "context",
    "debug",
    "extra-usage",
    "heapdump",
    "init",
    "loop",
    "schedule",
    "usage",
];
const STDERR_FLUSH_WAIT_MS: u64 = 150;
const STDERR_FLUSH_POLL_INTERVAL_MS: u64 = 10;

/// `SlashCommandInvocation`.
#[derive(Debug, Clone)]
pub(crate) struct SlashCommand {
    pub command_name: String,
    pub args: Option<String>,
}

/// `parseSlashCommandInput(text)`.
pub(crate) fn parse_slash_command_input(input: &str) -> Option<SlashCommand> {
    let trimmed = js_trim(input);
    let without_prefix = trimmed.strip_prefix('/')?;
    if without_prefix.is_empty() {
        return None;
    }
    let first_space = without_prefix.find(is_js_whitespace);
    let command_name = first_space.map_or(without_prefix, |index| &without_prefix[..index]);
    if command_name.is_empty() || command_name.contains('/') {
        return None;
    }
    let raw_args = first_space.map_or("", |index| {
        let rest = &without_prefix[index..];
        let after = rest
            .char_indices()
            .nth(1)
            .map_or("", |(offset, _)| &rest[offset..]);
        js_trim(after)
    });
    Some(SlashCommand {
        command_name: command_name.to_owned(),
        args: (!raw_args.is_empty()).then(|| raw_args.to_owned()),
    })
}

/// `classifyClaudeSlashCommand(name)`.
pub(crate) fn classify_slash_command(name: &str) -> &'static str {
    if CLAUDE_ROOT_ONLY_COMMANDS.contains(&name) {
        "command"
    } else {
        "skill"
    }
}

/// `/\bcode\s+(\d+)\b/i` on the error text.
fn exit_code_in(message: &str) -> Option<String> {
    let bytes = message.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    for start in 0..bytes.len() {
        if !message.is_char_boundary(start)
            || !message[start..]
                .get(..4)
                .is_some_and(|word| word.eq_ignore_ascii_case("code"))
            || (start > 0 && is_word(bytes[start - 1]))
        {
            continue;
        }
        let after = &message[start + 4..];
        let spaces = after.len() - after.trim_start_matches(is_js_whitespace).len();
        if spaces == 0 {
            continue;
        }
        let digits_text = &after[spaces..];
        let digits = digits_text.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        if digits_text
            .as_bytes()
            .get(digits)
            .is_some_and(|byte| is_word(*byte))
        {
            continue;
        }
        return Some(digits_text[..digits].to_owned());
    }
    None
}

fn is_image_mime_type(value: &str) -> bool {
    matches!(
        value,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

impl ClaudeSession {
    /// `buildTurnFailedEvent(errorMessage)`.
    pub(crate) fn build_turn_failed_event(&self, error_message: &str) -> JsValue {
        let trimmed = js_trim(error_message);
        let normalized = if trimmed.is_empty() {
            "Claude run failed"
        } else {
            trimmed
        };
        let mut event = JsObject::new();
        event.insert("type", text("turn_failed"));
        event.insert("provider", text("claude"));
        event.insert("error", text(normalized));
        if let Some(code) = exit_code_in(normalized) {
            event.insert("code", JsValue::String(code));
        }
        let diagnostic = js_trim(&self.state.borrow().recent_stderr).to_owned();
        if !diagnostic.is_empty() {
            event.insert("diagnostic", JsValue::String(diagnostic));
        }
        JsValue::Object(event)
    }

    /// `awaitRecentStderrAfterProcessExit(error)`.
    pub(crate) async fn await_recent_stderr_after_process_exit(&self, message: &str) {
        if !js_trim(&self.state.borrow().recent_stderr).is_empty() {
            return;
        }
        let lower = message.to_lowercase();
        if !lower.contains("process exited with code") && !lower.contains("terminated by signal") {
            return;
        }
        let started = tokio::time::Instant::now();
        loop {
            {
                let state = self.state.borrow();
                if state.closed || !js_trim(&state.recent_stderr).is_empty() {
                    return;
                }
            }
            if started.elapsed().as_millis() >= u128::from(STDERR_FLUSH_WAIT_MS) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                STDERR_FLUSH_POLL_INTERVAL_MS,
            ))
            .await;
        }
    }

    /// `resolveSlashCommandInvocation(prompt)`.
    pub(crate) fn resolve_slash_command(prompt: &AgentPromptInput) -> Option<SlashCommand> {
        let AgentPromptInput::Text(prompt) = prompt else {
            return None;
        };
        let parsed = parse_slash_command_input(prompt)?;
        (parsed.command_name == REWIND_COMMAND_NAME
            || CLAUDE_ROOT_ONLY_COMMANDS.contains(&parsed.command_name.as_str()))
        .then_some(parsed)
    }

    /// `toSdkUserMessage(prompt)`.
    pub(crate) fn to_sdk_user_message(
        &self,
        prompt: &AgentPromptInput,
    ) -> Result<JsValue, AgentError> {
        let mut content: Vec<JsValue> = Vec::new();
        let mut typed_slash_index: Option<usize> = None;
        let text_block = |value: &str| {
            let mut block = JsObject::new();
            block.insert("type", text("text"));
            block.insert("text", text(value));
            JsValue::Object(block)
        };
        match prompt {
            AgentPromptInput::Blocks(blocks) => {
                for chunk in blocks {
                    match chunk.get("type").and_then(JsValue::as_str) {
                        Some("text") => {
                            let value = spocky_contracts::js::js_string(chunk.get("text"));
                            if chunk.get("mimeType").is_none()
                                && parse_slash_command_input(&value).is_some()
                            {
                                typed_slash_index = Some(content.len());
                            }
                            content.push(text_block(&value));
                        }
                        Some("image") => {
                            let mime = spocky_contracts::js::js_string(chunk.get("mimeType"));
                            if is_image_mime_type(&mime) {
                                let mut source = JsObject::new();
                                source.insert("type", text("base64"));
                                source.insert("media_type", JsValue::String(mime));
                                source.insert(
                                    "data",
                                    chunk.get("data").cloned().unwrap_or(JsValue::Undefined),
                                );
                                let mut block = JsObject::new();
                                block.insert("type", text("image"));
                                block.insert("source", JsValue::Object(source));
                                content.push(JsValue::Object(block));
                            }
                        }
                        _ => content.push(text_block(&render_prompt_attachment_as_text(chunk)?)),
                    }
                }
            }
            AgentPromptInput::Text(prompt) => content.push(text_block(prompt)),
        }
        if let Some(index) = typed_slash_index.filter(|index| *index + 1 < content.len()) {
            let command = content.remove(index);
            content.push(command);
        }
        let message_id = uuid::Uuid::new_v4().to_string();
        self.remember_user_message_id(Some(&message_id));
        let mut message_body = JsObject::new();
        message_body.insert("role", text("user"));
        message_body.insert("content", JsValue::Array(content));
        let mut message = JsObject::new();
        message.insert("type", text("user"));
        message.insert("message", JsValue::Object(message_body));
        message.insert("parent_tool_use_id", JsValue::Null);
        message.insert("uuid", JsValue::String(message_id));
        message.insert(
            "session_id",
            JsValue::String(
                self.state
                    .borrow()
                    .claude_session_id
                    .clone()
                    .unwrap_or_default(),
            ),
        );
        Ok(JsValue::Object(message))
    }

    /// `startTurn(prompt, options)`.
    ///
    /// # Errors
    ///
    /// A closed session, an active foreground turn, or a prompt the
    /// baseline cannot render.
    pub async fn start_turn(
        self: &Rc<Self>,
        prompt: &AgentPromptInput,
        client_message_id: Option<String>,
    ) -> Result<String, AgentError> {
        {
            let state = self.state.borrow();
            if state.closed {
                return Err(AgentError::new("Claude session is closed"));
            }
            if state.active_foreground_turn_id.is_some() {
                return Err(AgentError::new("A foreground turn is already active"));
            }
        }
        let slash = Self::resolve_slash_command(prompt);
        if let Some(slash) = slash.filter(|slash| slash.command_name == REWIND_COMMAND_NAME) {
            let turn_id = self.create_turn_id("foreground");
            self.state.borrow_mut().active_foreground_turn_id = Some(turn_id.clone());
            self.transition_turn_state(TurnState::Foreground);
            let session = Rc::clone(self);
            tokio::task::spawn_local(async move { session.execute_rewind_turn(&slash).await });
            return Ok(turn_id);
        }
        if self.state.borrow().autonomous_turn.is_some() {
            self.complete_autonomous_turn();
        }
        let sdk_message = self.to_sdk_user_message(prompt)?;
        let uuid = sdk_message
            .get("uuid")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        self.remember_rewind_user_anchor(uuid.as_deref());
        let turn_id = self.create_turn_id("foreground");
        {
            let mut state = self.state.borrow_mut();
            state.active_foreground_turn_id = Some(turn_id.clone());
            state.foreground_has_visible_activity = false;
            state.active_turn_has_assistant_text = false;
            state.context_usage.begin_turn();
            state.turn_state = TurnState::Foreground;
            state.recent_stderr.clear();
            state.next_cancel_token += 1;
            let token = state.next_cancel_token;
            state.cancel_current_turn = Some(token);
        }
        let mut started = JsObject::new();
        started.insert("type", text("turn_started"));
        started.insert("provider", text("claude"));
        self.notify_subscribers(JsValue::Object(started));
        let outcome: Result<(), AgentError> = async {
            self.ensure_query().await?;
            let (query, input) = {
                let state = self.state.borrow();
                (state.query.clone(), state.input.clone())
            };
            let Some(input) = input else {
                return Err(AgentError::new(
                    "Claude session input stream not initialized",
                ));
            };
            {
                let mut state = self.state.borrow_mut();
                state.active_foreground_query = query;
                state.active_foreground_input = Some(Rc::clone(&input));
            }
            self.start_query_pump();
            input.push(sdk_message.clone());
            let session = Rc::clone(self);
            let turn = turn_id.clone();
            let message = sdk_message.clone();
            tokio::task::spawn_local(async move {
                crate::local::macrotask().await;
                if session.state.borrow().active_foreground_turn_id.as_deref()
                    == Some(turn.as_str())
                {
                    session.emit_submitted_user_message(
                        &message,
                        &turn,
                        client_message_id.as_deref(),
                    );
                }
            });
            Ok(())
        }
        .await;
        if let Err(error) = outcome {
            let failed = self.build_turn_failed_event(&error.message);
            self.finish_foreground_turn(failed)?;
        }
        Ok(turn_id)
    }

    /// The `requestCancel` closure a foreground turn installs.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper while canceled calls are flushed.
    pub(crate) fn request_cancel(self: &Rc<Self>, token: u64) -> Result<(), AgentError> {
        {
            let mut state = self.state.borrow_mut();
            if state.cancel_current_turn != Some(token) {
                return Ok(());
            }
            state.cancel_current_turn = None;
        }
        self.reject_all_pending_permissions("Permission request canceled");
        let mut canceled = JsObject::new();
        canceled.insert("type", text("turn_canceled"));
        canceled.insert("provider", text("claude"));
        canceled.insert("reason", text("Interrupted"));
        self.finish_foreground_turn(JsValue::Object(canceled))?;
        let session = Rc::clone(self);
        tokio::task::spawn_local(async move { session.interrupt_active_turn().await });
        Ok(())
    }

    /// `steerActiveTurn(prompt, options)`.
    ///
    /// # Errors
    ///
    /// A prompt the baseline cannot render.
    pub fn steer_active_turn(
        &self,
        prompt: &AgentPromptInput,
        expected_turn_id: &str,
        clear_pending_permissions: bool,
    ) -> Result<SteerResult, AgentError> {
        if Self::resolve_slash_command(prompt).is_some() {
            return Ok(SteerResult::Unavailable);
        }
        let (query, input) = {
            let state = self.state.borrow();
            let active = state
                .active_foreground_turn_id
                .clone()
                .or_else(|| state.autonomous_turn.clone());
            if state.compacting || active.as_deref() != Some(expected_turn_id) {
                return Ok(SteerResult::Unavailable);
            }
            let (Some(query), Some(input)) = (
                state.active_foreground_query.clone(),
                state.active_foreground_input.clone(),
            ) else {
                return Ok(SteerResult::Unavailable);
            };
            let current_query = state
                .query
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &query));
            let current_input = state
                .input
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &input));
            if !current_query || !current_input {
                return Ok(SteerResult::Unavailable);
            }
            (query, input)
        };
        let mut message = self.to_sdk_user_message(prompt)?;
        if let JsValue::Object(object) = &mut message {
            object.insert("priority", text("next"));
        }
        {
            let state = self.state.borrow();
            let active = state
                .active_foreground_turn_id
                .clone()
                .or_else(|| state.autonomous_turn.clone());
            let same = |slot: &Option<Rc<dyn ClaudeQuery>>| {
                slot.as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &query))
            };
            let same_input = |slot: &Option<Rc<PromptInput>>| {
                slot.as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &input))
            };
            if active.as_deref() != Some(expected_turn_id)
                || !same(&state.active_foreground_query)
                || !same_input(&state.active_foreground_input)
                || !same(&state.query)
                || !same_input(&state.input)
            {
                return Ok(SteerResult::Unavailable);
            }
        }
        self.enqueue_steer(&input, message, clear_pending_permissions)?;
        Ok(SteerResult::Accepted)
    }

    fn enqueue_steer(
        &self,
        input: &PromptInput,
        message: JsValue,
        clear: bool,
    ) -> Result<(), AgentError> {
        let uuid = message
            .get("uuid")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        if let Some(uuid) = &uuid {
            let mut state = self.state.borrow_mut();
            if !state.queued_steer_uuids.contains(uuid) {
                state.queued_steer_uuids.push(uuid.clone());
            }
            if clear {
                state.permission_clearing_steer_uuids.insert(uuid.clone());
            }
        }
        input.push(message);
        if clear && let Err(error) = self.deny_pending_permissions_superseded_by_steer() {
            if let Some(uuid) = &uuid {
                let mut state = self.state.borrow_mut();
                state.queued_steer_uuids.retain(|queued| queued != uuid);
                state.permission_clearing_steer_uuids.remove(uuid);
            }
            return Err(error);
        }
        Ok(())
    }

    /// `interrupt()`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper while pending calls are canceled.
    pub async fn interrupt(self: &Rc<Self>) -> Result<(), AgentError> {
        let cancel = self.state.borrow().cancel_current_turn;
        if let Some(token) = cancel {
            return self.request_cancel(token);
        }
        if self.state.borrow().autonomous_turn.is_some() {
            self.flush_pending_tool_calls()?;
            self.complete_autonomous_turn();
        }
        self.interrupt_active_turn().await;
        Ok(())
    }

    /// `interruptActiveTurn()`.
    pub(crate) async fn interrupt_active_turn(self: &Rc<Self>) {
        let Some(query) = self.state.borrow().query.clone() else {
            return;
        };
        self.state.borrow_mut().pending_interrupt_abort = true;
        self.discard_queued_steers(&query).await;
        Self::await_with_timeout(Some(query.interrupt())).await;
    }

    async fn discard_queued_steers(&self, query: &Rc<dyn ClaudeQuery>) {
        let uuids = {
            let mut state = self.state.borrow_mut();
            state.permission_clearing_steer_uuids.clear();
            std::mem::take(&mut state.queued_steer_uuids)
        };
        for uuid in uuids {
            if let Some(cancel) = query.cancel_async_message(&uuid) {
                let _ = cancel.await;
            } else {
                return;
            }
        }
    }

    /// `streamHistory()`: the replayed timeline, then subagent events.
    pub fn stream_history(&self) -> Vec<JsValue> {
        let mut state = self.state.borrow_mut();
        if !state.history_pending
            || (state.persisted_history.is_empty()
                && state.persisted_provider_subagent_events.is_empty())
        {
            return Vec::new();
        }
        let history = std::mem::take(&mut state.persisted_history);
        let subagents = std::mem::take(&mut state.persisted_provider_subagent_events);
        state.history_pending = false;
        let mut events: Vec<JsValue> = history
            .into_iter()
            .map(|(item, timestamp)| {
                let mut event = JsObject::new();
                event.insert("type", text("timeline"));
                event.insert("item", item);
                event.insert("provider", text("claude"));
                event.insert(
                    "timestamp",
                    timestamp.map_or(JsValue::Undefined, JsValue::String),
                );
                JsValue::Object(event)
            })
            .collect();
        events.extend(subagents);
        events
    }

    /// `finishForegroundTurn(event)`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper while pending calls are canceled.
    pub(crate) fn finish_foreground_turn(&self, event: JsValue) -> Result<(), AgentError> {
        if matches!(
            event.get("type").and_then(JsValue::as_str),
            Some("turn_failed" | "turn_canceled")
        ) {
            self.flush_pending_tool_calls()?;
        }
        self.notify_subscribers(event);
        {
            let mut state = self.state.borrow_mut();
            state.active_foreground_turn_id = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
            state.cancel_current_turn = None;
            state.active_turn_has_assistant_text = false;
            state.compaction_marker_open = false;
        }
        self.sync_turn_state();
        Ok(())
    }

    fn is_terminal(event: &JsValue) -> bool {
        matches!(
            event.get("type").and_then(JsValue::as_str),
            Some("turn_completed" | "turn_failed" | "turn_canceled")
        )
    }

    /// `dispatchEvents(events)`.
    pub(crate) fn dispatch_events(&self, events: Vec<JsValue>) {
        let mut terminal = false;
        for event in events {
            terminal |= Self::is_terminal(&event);
            self.notify_subscribers(event);
        }
        if !terminal {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.compaction_marker_open = false;
        if state.active_foreground_turn_id.is_some() {
            state.active_foreground_turn_id = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
            state.cancel_current_turn = None;
            state.active_turn_has_assistant_text = false;
        } else if state.autonomous_turn.is_some() {
            state.autonomous_turn = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
            state.active_turn_has_assistant_text = false;
        } else {
            return;
        }
        drop(state);
        self.sync_turn_state();
    }

    /// `startAutonomousTurn()`.
    pub(crate) fn start_autonomous_turn(&self) {
        if self.state.borrow().autonomous_turn.is_some() {
            return;
        }
        let id = self.create_turn_id("autonomous");
        {
            let mut state = self.state.borrow_mut();
            state.autonomous_turn = Some(id);
            let query = state.query.clone();
            let input = state.input.clone();
            state.active_foreground_query = query;
            state.active_foreground_input = input;
            state.active_turn_has_assistant_text = false;
            state.context_usage.begin_turn();
        }
        let mut started = JsObject::new();
        started.insert("type", text("turn_started"));
        started.insert("provider", text("claude"));
        self.notify_subscribers(JsValue::Object(started));
        self.sync_turn_state();
    }

    /// `completeAutonomousTurn()`.
    pub(crate) fn complete_autonomous_turn(&self) {
        if self.state.borrow().autonomous_turn.is_none() {
            return;
        }
        let mut completed = JsObject::new();
        completed.insert("type", text("turn_completed"));
        completed.insert("provider", text("claude"));
        self.notify_subscribers(JsValue::Object(completed));
        {
            let mut state = self.state.borrow_mut();
            state.autonomous_turn = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
            state.active_turn_has_assistant_text = false;
            state.compaction_marker_open = false;
        }
        self.sync_turn_state();
    }

    /// `failActiveTurns(errorMessage)`.
    ///
    /// # Errors
    ///
    /// A throw from the tool call mapper while pending calls are canceled.
    pub(crate) fn fail_active_turns(&self, error_message: &str) -> Result<(), AgentError> {
        let failure = self.build_turn_failed_event(error_message);
        self.flush_pending_tool_calls()?;
        let (foreground, autonomous) = {
            let state = self.state.borrow();
            (
                state.active_foreground_turn_id.is_some(),
                state.autonomous_turn.is_some(),
            )
        };
        if foreground {
            self.finish_foreground_turn(failure)?;
        } else if autonomous {
            self.dispatch_events(vec![failure]);
        }
        Ok(())
    }

    /// `handleRuntimeExit(child, code, signal)`.
    pub(crate) fn handle_runtime_exit(
        &self,
        child: &Rc<crate::process::ChildProcess>,
        exit: &ChildExit,
    ) {
        {
            let mut state = self.state.borrow_mut();
            let current = state
                .child_process
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, child));
            if state.closed || !current {
                return;
            }
            state.child_process = None;
        }
        self.fail_running_runtime_tasks();
        {
            let state = self.state.borrow();
            if state.active_foreground_turn_id.is_some() || state.autonomous_turn.is_some() {
                return;
            }
        }
        {
            let mut state = self.state.borrow_mut();
            state.query = None;
            state.input = None;
        }
        let detail = match (&exit.signal, exit.code) {
            (Some(signal), _) => format!("signal {signal}"),
            (None, Some(code)) => format!("exit code {code}"),
            (None, None) => "exit code unknown".to_owned(),
        };
        let failed = self.build_turn_failed_event(&format!(
            "Claude stopped unexpectedly ({detail}). Any background shells, monitors or other work it had running were terminated with it."
        ));
        self.dispatch_events(vec![failed]);
    }

    /// `failRunningRuntimeTasks()`.
    pub(crate) fn fail_running_runtime_tasks(&self) {
        let observations = self.task_protocol_source.borrow_mut().fail_running_tasks();
        let events = fold_subagent_observations(&observations)
            .into_iter()
            .map(provider_subagent)
            .collect();
        self.dispatch_events(events);
    }

    /// `emitSubmittedUserMessage(message, turnId, clientMessageId)`.
    pub(crate) fn emit_submitted_user_message(
        &self,
        message: &JsValue,
        turn_id: &str,
        client_message_id: Option<&str>,
    ) {
        let mut events = Vec::new();
        let _ = self.append_user_message_events(message, &mut events);
        if events.is_empty() {
            return;
        }
        self.state.borrow_mut().foreground_has_visible_activity = true;
        for event in events {
            if event.get("type").and_then(JsValue::as_str) == Some("timeline") {
                let item = event.get("item").cloned().unwrap_or(JsValue::Undefined);
                let item = match client_message_id.filter(|id| !id.is_empty()) {
                    Some(id)
                        if item.get("type").and_then(JsValue::as_str) == Some("user_message") =>
                    {
                        let mut object = spocky_contracts::js::spread(Some(&item));
                        object.insert("clientMessageId", text(id));
                        JsValue::Object(object)
                    }
                    _ => item,
                };
                let mut tagged = spocky_contracts::js::spread(Some(&event));
                tagged.insert("item", item);
                tagged.insert("turnId", text(turn_id));
                self.notify_subscribers(JsValue::Object(tagged));
            } else {
                self.notify_subscribers(event);
            }
        }
    }

    /// `run(prompt, options)` (`runProviderTurn` with
    /// `appendOrReplaceGrowingAssistantMessage`).
    ///
    /// # Errors
    ///
    /// A failed start, a `turn_failed` event, or a missing session id.
    pub async fn run(
        self: &Rc<Self>,
        prompt: &AgentPromptInput,
        client_message_id: Option<String>,
    ) -> Result<JsValue, AgentError> {
        // `runProviderTurn`: events delivered before `startTurn` resolves are
        // buffered by the channel and read in order once the turn id is known.
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<JsValue>();
        let subscription = self.subscribe(std::sync::Arc::new(move |event| {
            let _ = sender.send(event);
        }));
        let started = self.start_turn(prompt, client_message_id).await;
        let turn_id = match started {
            Ok(turn_id) => turn_id,
            Err(error) => {
                self.unsubscribe(subscription);
                return Err(error);
            }
        };
        let mut timeline: Vec<JsValue> = Vec::new();
        let mut final_text = String::new();
        let mut usage = JsValue::Undefined;
        let outcome: Result<(), AgentError> = loop {
            let Some(event) = receiver.recv().await else {
                // The subscription was dropped by `close()`: the baseline's
                // completion promise never settles.
                std::future::pending::<()>().await;
                break Ok(());
            };
            let event_turn = event.get("turnId").and_then(JsValue::as_str);
            if event_turn.is_some_and(|turn| turn != turn_id) {
                continue;
            }
            match event.get("type").and_then(JsValue::as_str) {
                Some("timeline") => {
                    let item = event.get("item").cloned().unwrap_or(JsValue::Undefined);
                    if item.get("type").and_then(JsValue::as_str) == Some("assistant_message") {
                        let item_text = spocky_contracts::js::js_string(item.get("text"));
                        final_text = if final_text.is_empty() || item_text.starts_with(&final_text)
                        {
                            item_text
                        } else {
                            format!("{final_text}{item_text}")
                        };
                    }
                    timeline.push(item);
                }
                Some("turn_completed") => {
                    usage = event.get("usage").cloned().unwrap_or(JsValue::Undefined);
                    break Ok(());
                }
                Some("turn_failed") => {
                    break Err(AgentError::new(spocky_contracts::js::js_string(
                        event.get("error"),
                    )));
                }
                Some("turn_canceled") => break Ok(()),
                _ => {}
            }
        };
        self.unsubscribe(subscription);
        outcome?;
        let session_id = self.id().unwrap_or_default();
        let mut result = JsObject::new();
        result.insert("sessionId", JsValue::String(session_id));
        result.insert("finalText", JsValue::String(final_text));
        result.insert("usage", usage);
        result.insert("timeline", JsValue::Array(timeline));
        {
            let mut state = self.state.borrow_mut();
            let mut info = JsObject::new();
            info.insert("provider", text("claude"));
            info.insert(
                "sessionId",
                state
                    .claude_session_id
                    .clone()
                    .map_or(JsValue::Null, JsValue::String),
            );
            info.insert(
                "model",
                state
                    .last_options_model
                    .clone()
                    .map_or(JsValue::Null, JsValue::String),
            );
            info.insert("modeId", text(&state.current_mode));
            state.cached_runtime_info = Some(JsValue::Object(info));
        }
        if self.state.borrow().claude_session_id.is_none() {
            return Err(AgentError::new("Session ID not set after run completed"));
        }
        Ok(JsValue::Object(result))
    }

    /// `listCommands()`.
    ///
    /// # Errors
    ///
    /// The query's failure.
    pub async fn list_commands(self: &Rc<Self>) -> Result<JsValue, AgentError> {
        let query = self.ensure_query().await?;
        let commands = query.supported_commands().await?;
        let mut map: Vec<(String, JsValue)> = Vec::new();
        for command in commands.as_array().unwrap_or_default() {
            let name = spocky_contracts::js::js_string(command.get("name"));
            if map.iter().any(|(existing, _)| *existing == name) {
                continue;
            }
            let mut entry = JsObject::new();
            entry.insert("name", JsValue::String(name.clone()));
            entry.insert(
                "description",
                command
                    .get("description")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            entry.insert(
                "argumentHint",
                command
                    .get("argumentHint")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            entry.insert("kind", text(classify_slash_command(&name)));
            map.push((name, JsValue::Object(entry)));
        }
        if !map.iter().any(|(name, _)| name == REWIND_COMMAND_NAME) {
            let mut rewind = JsObject::new();
            rewind.insert("name", text(REWIND_COMMAND_NAME));
            rewind.insert(
                "description",
                text("Rewind tracked files to a previous user message"),
            );
            rewind.insert("argumentHint", text("[user_message_uuid]"));
            map.push((REWIND_COMMAND_NAME.to_owned(), JsValue::Object(rewind)));
        }
        map.sort_by(|left, right| locale_compare(&left.0, &right.0));
        Ok(JsValue::Array(
            map.into_iter().map(|(_, entry)| entry).collect(),
        ))
    }
}
