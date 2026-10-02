//! The query pump: reads the SDK message stream and routes each frame to the
//! turn it belongs to.

use std::rc::Rc;
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::AgentError;

use super::timeline::BlockOptions;
use super::{ClaudeSession, text};
use crate::process::terminate_with_tree_kill;
use crate::sdk_query::ClaudeQuery;
use crate::transcript::{read_event_message_id, read_parent_tool_use_id, read_transcript_uuid};

const MAX_INTERRUPT_ABORT_RECOVERIES: u32 = 3;

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `shouldRecoverInterruptedQueryAbort(error, consecutiveRecoveries)`.
fn should_recover_interrupted_query_abort(error: &AgentError, consecutive_recoveries: u32) -> bool {
    consecutive_recoveries < MAX_INTERRUPT_ABORT_RECOVERIES
        && error.message.to_lowercase().contains("request was aborted")
}

fn is_assistantish_message(message: &JsValue) -> bool {
    matches!(
        str_of(message, "type"),
        Some("assistant" | "stream_event" | "tool_progress")
    ) || (str_of(message, "type") == Some("system")
        && str_of(message, "subtype") == Some("task_notification"))
}

fn event_is(event: &JsValue, kind: &str) -> bool {
    str_of(event, "type") == Some(kind)
}

impl ClaudeSession {
    fn is_active_query(&self, active: &Rc<dyn ClaudeQuery>) -> bool {
        let state = self.state.borrow();
        !state.closed
            && state
                .query
                .as_ref()
                .is_some_and(|query| Rc::ptr_eq(query, active))
    }

    /// `startQueryPump()`.
    pub(crate) fn start_query_pump(self: &Rc<Self>) {
        let generation = {
            let mut state = self.state.borrow_mut();
            if state.closed || state.query_pump_running {
                return;
            }
            state.query_pump_running = true;
            state.query_pump_generation += 1;
            state.query_pump_generation
        };
        let session = Rc::clone(self);
        tokio::task::spawn_local(async move {
            session.run_query_pump().await;
            let mut state = session.state.borrow_mut();
            if state.query_pump_generation == generation {
                state.query_pump_running = false;
            }
        });
    }

    /// `runQueryPump()`.
    async fn run_query_pump(self: &Rc<Self>) {
        let active_query = match self.ensure_query().await {
            Ok(query) => query,
            Err(error) => {
                let _ = self.fail_active_turns(&error.message);
                return;
            }
        };
        let mut recoveries = 0_u32;
        while self.is_active_query(&active_query) {
            match self
                .drain_active_query(&active_query, &mut recoveries)
                .await
            {
                Ok(true) => break,
                Ok(false) => {
                    if self.is_active_query(&active_query) {
                        let _ =
                            self.fail_active_turns("Claude stream ended before terminal result");
                    }
                    break;
                }
                Err(error) => {
                    if self.is_active_query(&active_query)
                        && should_recover_interrupted_query_abort(&error, recoveries)
                    {
                        recoveries += 1;
                        continue;
                    }
                    if self.is_active_query(&active_query) {
                        self.await_recent_stderr_after_process_exit(&error.message)
                            .await;
                        let _ = self.fail_active_turns(&error.message);
                    }
                    break;
                }
            }
        }
        let mut state = self.state.borrow_mut();
        if state
            .query
            .as_ref()
            .is_some_and(|query| Rc::ptr_eq(query, &active_query))
        {
            state.query = None;
            state.input = None;
        }
    }

    /// `drainActiveQuery()`: `true` when the pump should stop.
    async fn drain_active_query(
        self: &Rc<Self>,
        active_query: &Rc<dyn ClaudeQuery>,
        recoveries: &mut u32,
    ) -> Result<bool, AgentError> {
        loop {
            match active_query.next().await {
                None => return Ok(false),
                Some(Err(error)) => return Err(error),
                Some(Ok(message)) => {
                    *recoveries = 0;
                    // A loop body that returns or throws closes the iterator.
                    match self.handle_pumped_message(&message, active_query).await {
                        Ok(false) => {}
                        Ok(true) => {
                            active_query.return_().await;
                            return Ok(true);
                        }
                        Err(error) => {
                            active_query.return_().await;
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    async fn handle_pumped_message(
        self: &Rc<Self>,
        message: &JsValue,
        active_query: &Rc<dyn ClaudeQuery>,
    ) -> Result<bool, AgentError> {
        if self
            .handle_missing_resumed_conversation(message, active_query)
            .await?
        {
            return Ok(true);
        }
        self.route_sdk_message_from_pump(message)?;
        Ok(false)
    }

    /// `handleMissingResumedConversation(message, activeQuery)`.
    async fn handle_missing_resumed_conversation(
        self: &Rc<Self>,
        message: &JsValue,
        active_query: &Rc<dyn ClaudeQuery>,
    ) -> Result<bool, AgentError> {
        let Some(stale_resume_error) = self.read_missing_resumed_conversation_error(message) else {
            return Ok(false);
        };
        self.fail_active_turns(&stale_resume_error)?;
        // Ending the input retires the process on purpose; detach first so its
        // exit is not reported as a crash.
        let (retired_child, input) = {
            let mut state = self.state.borrow_mut();
            (state.child_process.take(), state.input.clone())
        };
        if let Some(input) = input {
            input.end();
        }
        Self::await_with_timeout(Some(active_query.return_())).await;
        // Tree-kill: MCP children of the retired process outlive it otherwise.
        if let Some(child) = retired_child {
            terminate_with_tree_kill(
                &child,
                Duration::from_millis(2000),
                Duration::from_millis(2000),
            )
            .await;
        }
        {
            let mut state = self.state.borrow_mut();
            if state
                .query
                .as_ref()
                .is_some_and(|query| Rc::ptr_eq(query, active_query))
            {
                state.query = None;
                state.input = None;
            }
            state.persistence = None;
            state.persisted_history.clear();
            state.persisted_provider_subagent_events.clear();
            state.history_pending = false;
            state.cached_runtime_info = None;
            state.query_restart_needed = false;
            state.autonomous_turn = None;
            state.active_foreground_turn_id = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
        }
        self.sync_turn_state();
        Ok(true)
    }

    /// `shouldSuppressStaleResult(message)`.
    fn should_suppress_stale_result(&self, message: &JsValue) -> bool {
        // Results from interrupted requests: the cancel path already emitted
        // the terminal event. The flag is consumed on any result.
        let is_result = str_of(message, "type") == Some("result");
        let success = str_of(message, "subtype") == Some("success");
        if is_result && self.state.borrow().pending_interrupt_abort {
            self.state.borrow_mut().pending_interrupt_abort = false;
            if !success {
                return true;
            }
        }
        is_result && !success && Self::is_abort_error(message)
    }

    /// `shouldStartAutonomousTurn(message)`.
    fn should_start_autonomous_turn(&self, message: &JsValue) -> bool {
        let state = self.state.borrow();
        if state.active_foreground_turn_id.is_some() || state.pending_interrupt_abort {
            return false;
        }
        is_assistantish_message(message)
    }

    /// `routeSdkMessageFromPump(message)`.
    fn route_sdk_message_from_pump(self: &Rc<Self>, message: &JsValue) -> Result<(), AgentError> {
        if self.should_suppress_stale_result(message) {
            return Ok(());
        }
        let is_foreground = self.state.borrow().active_foreground_turn_id.is_some();
        if self.should_start_autonomous_turn(message) {
            self.start_autonomous_turn();
        }
        let is_result = str_of(message, "type") == Some("result");
        if !is_foreground && self.state.borrow().autonomous_turn.is_none() && is_result {
            return Ok(());
        }
        let turn_id = {
            let state = self.state.borrow();
            state
                .active_foreground_turn_id
                .clone()
                .or_else(|| state.autonomous_turn.clone())
        };
        let message_id = read_event_message_id(message);
        self.remember_transcript_progress(message, read_transcript_uuid(message).as_deref());
        let events =
            self.build_pumped_message_events(message, message_id.as_deref(), turn_id.as_deref())?;
        if events.is_empty() {
            return Ok(());
        }
        let (has_terminal, foreground_visible) = {
            let state = self.state.borrow();
            (
                state.pending_interrupt_abort,
                state.foreground_has_visible_activity,
            )
        };
        let foreground_active = self.state.borrow().active_foreground_turn_id.is_some();
        if has_terminal
            && is_result
            && events
                .iter()
                .any(|event| event_is(event, "turn_completed") || event_is(event, "turn_failed"))
            && (!foreground_active || !foreground_visible)
        {
            self.state.borrow_mut().pending_interrupt_abort = false;
            return Ok(());
        }
        if events.iter().any(|event| {
            event_is(event, "timeline")
                && event
                    .get("item")
                    .is_some_and(|item| str_of(item, "type") == Some("assistant_message"))
        }) {
            self.state.borrow_mut().active_turn_has_assistant_text = true;
        }
        if foreground_active
            && events.iter().any(|event| {
                event_is(event, "timeline")
                    || event_is(event, "permission_requested")
                    || event_is(event, "permission_resolved")
            })
        {
            self.state.borrow_mut().foreground_has_visible_activity = true;
        }
        self.dispatch_events(events);
        Ok(())
    }

    /// `buildPumpedMessageEvents(message, messageIdHint, turnId)`.
    fn build_pumped_message_events(
        &self,
        message: &JsValue,
        message_id_hint: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<Vec<JsValue>, AgentError> {
        let mut events = self.translate_message_to_events(
            message,
            BlockOptions {
                user_text: false,
                suppress_assistant_text: true,
                suppress_reasoning: true,
            },
        )?;
        if read_parent_tool_use_id(message).is_none() {
            let items = self.state.borrow_mut().timeline_assembler.consume(
                message,
                turn_id,
                message_id_hint,
            );
            for item in items {
                let mut event = JsObject::new();
                event.insert("type", text("timeline"));
                event.insert("item", item);
                event.insert("provider", text("claude"));
                events.push(JsValue::Object(event));
            }
        }
        Ok(events)
    }
}
