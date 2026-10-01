//! The session event pipeline from pinned Paseo `agent/agent-manager.ts`:
//! staging and queueing provider events, `handleStreamEvent` and its
//! per-type handlers, timeline recording, state emission, attention, and
//! snapshot persistence.
//!
//! Without a durable timeline store every step of `handleStreamEvent` is
//! synchronous, so each event is handled in one locked stretch; the
//! baseline's only awaits there resolve immediately in that configuration.

use std::sync::Arc;

use spocky_contracts::js::strict_equals;
use spocky_store::js_value::{JsObject, JsValue};

use super::create::{attach_persistence_cwd, touch_updated_at};
use super::run::TrackedRun;
use super::{AgentAttentionNotice, AgentLifecycle, AgentManager, AgentManagerEvent, State};
use crate::agent_labels::is_delegated_agent;
use crate::agent_projection::{AgentAttention, SnapshotOverrides};
use crate::agent_prompt::is_system_injected_envelope;
use crate::agent_sdk::AgentError;
use crate::external_state::command_may_have_changed_external_state;
use crate::stream_coalescer::CoalescerFlush;
use crate::text::js_trim;
use crate::timeline::{TimelineError, TimelineRow};
use crate::timeline_content::limit_agent_timeline_item_content;
use spocky_contracts::js::{js_string, spread, truthy};

/// `SYSTEM_ERROR_PREFIX`.
const SYSTEM_ERROR_PREFIX: &str = "[System Error]";

/// `ActiveTurnTerminalDisposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalDisposition {
    ClosedCurrent,
    Stale,
    Untracked,
}

/// `recordSubmittedPrompt(agent, prompt, clientMessageId, { messageId,
/// providerMessageId, turnId })` with the prompt already reduced to text.
pub(crate) struct SubmittedPrompt {
    pub(crate) text: String,
    pub(crate) client_message_id: String,
    pub(crate) message_id: Option<String>,
    pub(crate) provider_message_id: Option<String>,
    pub(crate) turn_id: Option<JsValue>,
}

/// `StreamEventFlags`.
struct StreamEventFlags {
    should_dispatch_event: bool,
    should_notify_waiters: bool,
}

pub(crate) fn event_type(event: &JsValue) -> Option<&str> {
    event.get("type").and_then(JsValue::as_str)
}

/// `isTurnTerminalEvent`.
pub(crate) fn is_turn_terminal_event(event: &JsValue) -> bool {
    matches!(
        event_type(event),
        Some("turn_completed" | "turn_failed" | "turn_canceled")
    )
}

/// `getAgentStreamEventTurnId`: `"turnId" in event ? event.turnId :
/// undefined`; `None` is `undefined`.
pub(crate) fn raw_turn_id(event: &JsValue) -> Option<JsValue> {
    event
        .get("turnId")
        .filter(|turn| !matches!(turn, JsValue::Undefined))
        .cloned()
}

fn type_error(error: &crate::timeline::JsTypeError) -> AgentError {
    AgentError {
        name: "TypeError".to_owned(),
        message: error.0.clone(),
    }
}

fn timeline_error(error: TimelineError) -> AgentError {
    match error {
        TimelineError::Type(error) => type_error(&error),
        TimelineError::UnknownAgent(error) => AgentError::new(error.to_string()),
    }
}

/// `limitAgentStreamEventContent`.
fn limit_stream_event_content(event: &JsValue) -> Result<JsValue, AgentError> {
    if event_type(event) != Some("timeline") {
        return Ok(event.clone());
    }
    let item =
        limit_agent_timeline_item_content(event.get("item").cloned().unwrap_or(JsValue::Undefined))
            .map_err(|error| type_error(&error))?;
    let mut copy = spread(Some(event));
    copy.insert("item", item);
    Ok(JsValue::Object(copy))
}

fn with_turn_id(event: &JsValue, turn_id: &str) -> JsValue {
    let mut copy = spread(Some(event));
    copy.insert("turnId", JsValue::String(turn_id.to_owned()));
    JsValue::Object(copy)
}

/// `formatTurnFailedMessage(event)`.
pub(crate) fn format_turn_failed_message(event: &JsValue) -> String {
    let base = js_trim(&js_string(event.get("error"))).to_owned();
    let mut parts = vec![if base.is_empty() {
        "Provider run failed".to_owned()
    } else {
        base.clone()
    }];
    if let Some(code) = event
        .get("code")
        .and_then(JsValue::as_str)
        .map(js_trim)
        .filter(|code| !code.is_empty())
    {
        parts.push(format!("code: {code}"));
    }
    if let Some(diagnostic) = event
        .get("diagnostic")
        .and_then(JsValue::as_str)
        .map(js_trim)
        .filter(|diagnostic| !diagnostic.is_empty() && *diagnostic != base)
    {
        parts.push(diagnostic.to_owned());
    }
    parts.join("\n\n")
}

impl AgentManager {
    /// `recordTimeline(agentId, item, { timestamp, providerMessageId, turnId })`.
    pub(crate) fn record_timeline_locked(
        state: &mut State,
        agent_id: &str,
        item: JsValue,
        timestamp: Option<String>,
        provider_message_id: Option<String>,
        turn_id: Option<String>,
    ) -> Result<TimelineRow, AgentError> {
        let item = limit_agent_timeline_item_content(item).map_err(|error| type_error(&error))?;
        state
            .timeline
            .append(agent_id, item, timestamp, turn_id, provider_message_id)
            .map_err(timeline_error)
    }

    pub(crate) fn record_timeline(
        &self,
        agent_id: &str,
        item: JsValue,
        timestamp: Option<String>,
        provider_message_id: Option<String>,
        turn_id: Option<String>,
    ) -> Result<TimelineRow, AgentError> {
        let mut state = self.lock();
        Self::record_timeline_locked(
            &mut state,
            agent_id,
            item,
            timestamp,
            provider_message_id,
            turn_id,
        )
    }

    /// `dispatchStream(agentId, event, metadata)`.
    pub(crate) fn dispatch_stream_locked(
        &self,
        state: &State,
        agent_id: &str,
        event: &JsValue,
        seq: Option<i64>,
        epoch: Option<String>,
        timestamp: Option<String>,
    ) -> Result<(), AgentError> {
        let event = limit_stream_event_content(event)?;
        self.dispatch(
            state,
            AgentManagerEvent::AgentStream {
                agent_id: agent_id.to_owned(),
                event,
                seq,
                epoch,
                timestamp,
            },
        );
        Ok(())
    }

    /// `recordAndDispatchTimelineItem(agentId, item, provider, turnId,
    /// { providerMessageId })`: returns the dispatched event.
    pub(crate) fn record_and_dispatch_timeline_item_locked(
        &self,
        state: &mut State,
        agent_id: &str,
        item: JsValue,
        provider: JsValue,
        turn_id: Option<JsValue>,
        provider_message_id: Option<String>,
    ) -> Result<JsValue, AgentError> {
        let turn_text = turn_id
            .as_ref()
            .filter(|turn| truthy(Some(turn)))
            .map(|turn| js_string(Some(turn)));
        let shell_command = (item.get("type").and_then(JsValue::as_str) == Some("tool_call")
            && item.get("status").and_then(JsValue::as_str) == Some("completed")
            && item
                .get("detail")
                .and_then(|detail| detail.get("type"))
                .and_then(JsValue::as_str)
                == Some("shell"))
        .then(|| {
            item.get("detail")
                .and_then(|detail| detail.get("command"))
                .map(|command| js_string(Some(command)))
        })
        .flatten();
        let row = Self::record_timeline_locked(
            state,
            agent_id,
            item.clone(),
            None,
            provider_message_id,
            turn_text,
        )?;
        let mut event = JsObject::new();
        event.insert("type", JsValue::String("timeline".to_owned()));
        event.insert("item", item);
        event.insert("provider", provider);
        if let Some(turn_id) = turn_id {
            event.insert("turnId", turn_id);
        }
        let event = JsValue::Object(event);
        let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
        self.dispatch_stream_locked(
            state,
            agent_id,
            &event,
            Some(row.seq),
            epoch,
            Some(row.timestamp),
        )?;
        if let Some(command) = shell_command
            && command_may_have_changed_external_state(&command)
            && let (Some(agent), Some(callback)) = (
                state.agent(agent_id),
                self.inner.on_workspace_state_may_have_changed.clone(),
            )
        {
            let cwd = agent.snapshot.cwd.clone();
            self.call_in_order(move || callback(&cwd));
        }
        Ok(event)
    }

    /// `notifyForegroundTurnWaiters(agentId, event)`: non-terminal.
    fn notify_foreground_turn_waiters_locked(state: &mut State, agent_id: &str, event: &JsValue) {
        let Some(turn_id) = raw_turn_id(event).filter(|turn| !turn.is_null()) else {
            return;
        };
        if let Some(agent) = state.agent_mut(agent_id) {
            Self::notify_waiters(agent, &turn_id, event, false);
        }
    }

    /// `notifyWaiters` for the waiters whose turn is `turn_id`.
    pub(crate) fn notify_waiters(
        agent: &mut super::ManagedAgent,
        turn_id: &JsValue,
        event: &JsValue,
        terminal: bool,
    ) {
        for waiter in &mut agent.foreground_turn_waiters {
            if turn_id.as_str() == Some(waiter.turn_id.as_str()) && waiter.tx.is_some() {
                if let Some(tx) = &waiter.tx {
                    let _ = tx.send(event.clone());
                }
                if terminal {
                    waiter.tx = None;
                }
            }
        }
    }

    /// The coalescer's `onFlush`: record, dispatch, and notify waiters.
    pub(crate) fn apply_coalescer_flushes(
        &self,
        state: &mut State,
        flushes: Vec<CoalescerFlush>,
    ) -> Result<(), AgentError> {
        for flush in flushes {
            let event = self.record_and_dispatch_timeline_item_locked(
                state,
                &flush.agent_id,
                flush.item,
                flush.provider,
                flush.turn_id,
                None,
            )?;
            Self::notify_foreground_turn_waiters_locked(state, &flush.agent_id, &event);
        }
        Ok(())
    }

    fn schedule_coalescer_timer(&self, request: crate::stream_coalescer::TimerRequest) {
        let manager = self.clone();
        tokio::spawn(async move {
            let delay = std::time::Duration::from_secs_f64(request.delay_ms.max(0.0) / 1000.0);
            tokio::time::sleep(delay).await;
            let mut state = manager.lock();
            #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
            let now = crate::clock::now_millis() as f64;
            let flushes = state.coalescer.fire(&request.agent_id, request.token, now);
            // A throw from a timer callback has no caller to reach.
            let _ = manager.apply_coalescer_flushes(&mut state, flushes);
        });
    }

    /// `agentStreamCoalescer.flushAll()`.
    pub(crate) fn flush_coalescer_all(&self) {
        let mut state = self.lock();
        #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
        let now = crate::clock::now_millis() as f64;
        let flushes = state.coalescer.flush_all(now);
        let _ = self.apply_coalescer_flushes(&mut state, flushes);
    }

    /// `refreshSessionPersistence(agent)`.
    pub(crate) fn refresh_session_persistence_locked(state: &mut State, agent_id: &str) {
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        let handle = agent
            .session
            .as_ref()
            .and_then(|session| session.describe_persistence());
        if let Some(handle) = attach_persistence_cwd(handle, &agent.snapshot.cwd) {
            agent.snapshot.persistence = Some(handle);
        }
    }

    pub(crate) fn refresh_session_persistence(&self, agent_id: &str) {
        Self::refresh_session_persistence_locked(&mut self.lock(), agent_id);
    }

    /// `syncFeaturesFromSession(agent)`.
    fn sync_features_from_session(agent: &mut super::ManagedAgent) {
        if let Some(features) = agent
            .session
            .as_ref()
            .and_then(|session| session.features())
            .filter(|features| truthy(Some(features)))
        {
            agent.snapshot.features = Some(features);
        }
    }

    /// `broadcastAgentAttention(agent, reason)`.
    fn broadcast_agent_attention(&self, agent: &super::ManagedAgent, reason: &str) {
        if is_delegated_agent(Some(&agent.snapshot.labels)) {
            return;
        }
        if let Some(callback) = self.inner.on_agent_attention.clone() {
            let notice = AgentAttentionNotice {
                agent_id: agent.snapshot.id.clone(),
                provider: agent.snapshot.provider.clone(),
                reason: reason.to_owned(),
            };
            self.call_in_order(move || callback(notice));
        }
    }

    /// `checkAndSetAttention(agent)`.
    fn check_and_set_attention(&self, state: &mut State, agent_id: &str) {
        let Some(current) = state.agent(agent_id).map(|agent| agent.snapshot.lifecycle) else {
            return;
        };
        let previous = state.previous_statuses.insert(agent_id.to_owned(), current);
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        if agent.snapshot.internal
            || matches!(agent.snapshot.attention, AgentAttention::Required { .. })
        {
            return;
        }
        let reason = if previous == Some(AgentLifecycle::Running) && current == AgentLifecycle::Idle
        {
            "finished"
        } else if previous != Some(AgentLifecycle::Error) && current == AgentLifecycle::Error {
            "error"
        } else {
            return;
        };
        agent.snapshot.attention = AgentAttention::Required {
            reason: reason.to_owned(),
            timestamp_millis: crate::clock::now_millis(),
        };
        let agent = state.agent(agent_id).expect("agent present");
        self.broadcast_agent_attention(agent, reason);
    }

    /// `emitState(agent, { persist })`.
    pub(crate) fn emit_state_locked(&self, state: &mut State, agent_id: &str, persist: bool) {
        if state.agent(agent_id).is_none() {
            return;
        }
        self.check_and_set_attention(state, agent_id);
        if persist {
            self.enqueue_background_persist(agent_id);
        }
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        Self::sync_features_from_session(agent);
        let snapshot = Box::new(agent.snapshot.clone());
        self.dispatch(state, AgentManagerEvent::AgentState(snapshot));
    }

    pub(crate) fn emit_state(&self, agent_id: &str, persist: bool) {
        let mut state = self.lock();
        self.emit_state_locked(&mut state, agent_id, persist);
    }

    /// `enqueueBackgroundPersist(agent)`: failures are only logged.
    fn enqueue_background_persist(&self, agent_id: &str) {
        let manager = self.clone();
        let agent_id = agent_id.to_owned();
        self.track_background_task(async move {
            let _ = manager
                .persist_snapshot(&agent_id, SnapshotOverrides::default())
                .await;
        });
    }

    /// `persistSnapshot(agent, options)`.
    pub(crate) async fn persist_snapshot(
        &self,
        agent_id: &str,
        overrides: SnapshotOverrides,
    ) -> Result<(), AgentError> {
        self.persist_snapshot_of(agent_id, None, overrides).await
    }

    /// `refreshRuntimeInfo(agent, { emit })`: a failed read keeps the
    /// previous runtime info.
    pub(crate) async fn refresh_runtime_info(&self, agent_id: &str, emit: bool) {
        let Some(session) = self
            .lock()
            .agent(agent_id)
            .and_then(|agent| agent.session.clone())
        else {
            return;
        };
        let Ok(info) = session.get_runtime_info().await else {
            return;
        };
        let mut state = self.lock();
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        let previous = agent.snapshot.runtime_info.clone();
        let changed = ["model", "thinkingOptionId", "sessionId", "modeId"]
            .iter()
            .any(|key| {
                !strict_equals(
                    info.get(key),
                    previous.as_ref().and_then(|previous| previous.get(key)),
                )
            });
        if agent.snapshot.persistence.is_none()
            && let Some(session_id) = info.get("sessionId").filter(|id| truthy(Some(id)))
        {
            let mut handle = JsObject::new();
            handle.insert("provider", JsValue::String(agent.snapshot.provider.clone()));
            handle.insert("sessionId", session_id.clone());
            agent.snapshot.persistence =
                attach_persistence_cwd(Some(JsValue::Object(handle)), &agent.snapshot.cwd);
        }
        agent.snapshot.runtime_info = Some(info);
        if changed && emit {
            self.emit_state_locked(&mut state, agent_id, true);
        }
    }

    /// `void this.refreshRuntimeInfo(agent)`.
    fn spawn_refresh_runtime_info(&self, agent_id: &str) {
        let manager = self.clone();
        let agent_id = agent_id.to_owned();
        tokio::spawn(async move { manager.refresh_runtime_info(&agent_id, true).await });
    }

    /// `refreshSessionState(agent, { emit })`.
    pub(crate) async fn refresh_session_state(&self, agent_id: &str, emit: bool) {
        let Some(session) = self
            .lock()
            .agent(agent_id)
            .and_then(|agent| agent.session.clone())
        else {
            return;
        };
        let modes = session
            .get_available_modes()
            .await
            .ok()
            .and_then(|modes| modes.as_array().map(<[JsValue]>::to_vec))
            .unwrap_or_default();
        {
            let mut state = self.lock();
            if let Some(agent) = state.agent_mut(agent_id) {
                agent.snapshot.available_modes = modes;
            }
        }
        let current_mode = session.get_current_mode().await.ok().flatten();
        {
            let mut state = self.lock();
            if let Some(agent) = state.agent_mut(agent_id) {
                agent.snapshot.current_mode_id = current_mode;
                agent.snapshot.pending_permissions = match session.get_pending_permissions() {
                    Ok(pending) => pending
                        .into_iter()
                        .map(|request| (js_string(request.get("id")), request))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                Self::sync_features_from_session(agent);
            }
        }
        self.refresh_runtime_info(agent_id, emit).await;
    }

    /// `subscribeToSession(agent)`.
    pub(crate) fn subscribe_to_session(&self, agent_id: &str) {
        let session = {
            let state = self.lock();
            let Some(agent) = state.agent(agent_id) else {
                return;
            };
            if agent.unsubscribe_session.is_some() {
                return;
            }
            agent.session.clone()
        };
        let Some(session) = session else {
            return;
        };
        let weak = Arc::downgrade(&self.inner);
        let id = agent_id.to_owned();
        let unsubscribe = session.subscribe(Arc::new(move |event| {
            if let Some(inner) = weak.upgrade() {
                AgentManager { inner }.enqueue_session_event(&id, event);
            }
        }));
        if let Some(agent) = self.lock().agent_mut(agent_id) {
            agent.unsubscribe_session = Some(unsubscribe);
        }
    }

    /// `enqueueSessionEvent(agentId, event)`: staged while a foreground run's
    /// turn is starting, otherwise queued for the agent's drain task.
    pub(crate) fn enqueue_session_event(&self, agent_id: &str, event: JsValue) {
        let mut state = self.lock();
        if let Some(TrackedRun::Foreground {
            start: super::run::RunStart::Pending,
            staged,
            ..
        }) = state.runs.get_mut(agent_id)
        {
            staged.push(event);
            return;
        }
        let queue = state.session_queues.entry(agent_id.to_owned()).or_default();
        queue.events.push_back(event);
        if queue.draining {
            return;
        }
        queue.draining = true;
        drop(state);
        let manager = self.clone();
        let agent_id = agent_id.to_owned();
        self.track_background_task(async move { manager.drain_session_events(&agent_id) });
    }

    /// The agent's drain task: one event at a time, in arrival order.
    pub(crate) fn drain_session_events(&self, agent_id: &str) {
        loop {
            let mut state = self.lock();
            let Some(queue) = state.session_queues.get_mut(agent_id) else {
                return;
            };
            let Some(event) = queue.events.pop_front() else {
                state.session_queues.remove(agent_id);
                drop(state);
                self.inner.drain_idle.notify_waiters();
                return;
            };
            let live = state
                .agent(agent_id)
                .is_some_and(|agent| agent.session.is_some());
            if live {
                // "Failed to process session event" is only logged.
                let _ = self.dispatch_session_event_locked(&mut state, agent_id, &event);
            }
        }
    }

    /// `dispatchSessionEvent(agent, event)`.
    pub(crate) fn dispatch_session_event_locked(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
    ) -> Result<(), AgentError> {
        if event_type(event) == Some("provider_subagent") {
            let update = state
                .provider_subagents
                .apply(
                    agent_id,
                    &js_string(event.get("provider")),
                    event.get("event").unwrap_or(&JsValue::Undefined),
                )
                .map_err(timeline_error)?;
            self.dispatch(state, AgentManagerEvent::ProviderSubagent(update));
            return Ok(());
        }
        let turn_id = raw_turn_id(event).filter(|turn| !turn.is_null());
        let should_notify = self.handle_stream_event_locked(state, agent_id, event, false)?;
        if should_notify
            && let Some(turn_id) = turn_id
            && let Some(agent) = state.agent_mut(agent_id)
        {
            Self::notify_waiters(agent, &turn_id, event, is_turn_terminal_event(event));
        }
        Ok(())
    }

    /// `attachManagedTurnIdentity(agent, event, fromHistory)`.
    fn attach_managed_turn_identity(
        agent: &super::ManagedAgent,
        event: JsValue,
        from_history: bool,
    ) -> (JsValue, Option<JsValue>) {
        let existing = raw_turn_id(&event);
        if from_history || existing.is_some() {
            return (event, existing);
        }
        let current = agent
            .snapshot
            .active_foreground_turn_id
            .clone()
            .or_else(|| agent.snapshot.active_turn_id.clone());
        match event_type(&event) {
            Some("turn_started") => {
                let turn_id = current
                    .unwrap_or_else(|| format!("autonomous-{}", crate::clock::random_uuid()));
                (
                    with_turn_id(&event, &turn_id),
                    Some(JsValue::String(turn_id)),
                )
            }
            Some("turn_completed" | "turn_failed" | "turn_canceled" | "timeline") => {
                match current.filter(|turn| !turn.is_empty()) {
                    Some(turn_id) => (
                        with_turn_id(&event, &turn_id),
                        Some(JsValue::String(turn_id)),
                    ),
                    None => (event, None),
                }
            }
            _ => (event, None),
        }
    }

    /// `applyActiveTurnTerminal(agent, turnId, fromHistory)`.
    pub(crate) fn apply_active_turn_terminal(
        agent: &mut super::ManagedAgent,
        turn_id: Option<&JsValue>,
        from_history: bool,
    ) -> TerminalDisposition {
        if from_history {
            return TerminalDisposition::Stale;
        }
        let Some(active) = agent
            .snapshot
            .active_turn_id
            .clone()
            .filter(|id| !id.is_empty())
        else {
            return TerminalDisposition::Untracked;
        };
        if let Some(turn_id) = turn_id.filter(|turn| truthy(Some(turn)))
            && turn_id.as_str() != Some(active.as_str())
        {
            return TerminalDisposition::Stale;
        }
        agent.snapshot.active_turn_id = None;
        agent.snapshot.active_turn_started_at_millis = None;
        TerminalDisposition::ClosedCurrent
    }

    /// `handleStreamEvent(agent, event, { fromHistory })`: whether waiters
    /// should hear the event.
    pub(crate) fn handle_stream_event_locked(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        from_history: bool,
    ) -> Result<bool, AgentError> {
        let event = limit_stream_event_content(event)?;
        let Some(agent) = state.agent(agent_id) else {
            return Ok(false);
        };
        let (event, event_turn_id) = Self::attach_managed_turn_identity(agent, event, from_history);
        let active_foreground = agent
            .snapshot
            .active_foreground_turn_id
            .clone()
            .map_or(JsValue::Null, JsValue::String);
        let is_foreground_event = strict_equals(Some(&active_foreground), event_turn_id.as_ref());
        let terminal = is_turn_terminal_event(&event);
        if terminal
            && let Some(turn_id) = event_turn_id.as_ref().filter(|turn| truthy(Some(turn)))
            && agent
                .finalized_foreground_turn_ids
                .iter()
                .any(|finalized| turn_id.as_str() == Some(finalized.as_str()))
        {
            return Ok(false);
        }
        if !from_history {
            if let Some(agent) = state.agent_mut(agent_id) {
                touch_updated_at(&mut agent.snapshot);
            }
            #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
            let now = crate::clock::now_millis() as f64;
            let outcome = state.coalescer.handle(agent_id, &event, now);
            if let Some(timer) = outcome.timer {
                self.schedule_coalescer_timer(timer);
            }
            self.apply_coalescer_flushes(state, outcome.flushes)?;
            if outcome.coalesced {
                return Ok(false);
            }
            let flushes = state.coalescer.flush_for(agent_id, now);
            self.apply_coalescer_flushes(state, flushes)?;
        }
        let disposition = if terminal {
            state
                .agent_mut(agent_id)
                .map_or(TerminalDisposition::Untracked, |agent| {
                    Self::apply_active_turn_terminal(agent, event_turn_id.as_ref(), from_history)
                })
        } else {
            TerminalDisposition::Untracked
        };
        let mut flags = StreamEventFlags {
            should_dispatch_event: true,
            should_notify_waiters: true,
        };
        self.dispatch_stream_event_by_type(
            state,
            agent_id,
            &event,
            from_history,
            is_foreground_event,
            event_turn_id.as_ref(),
            disposition,
            &mut flags,
        )?;
        if !from_history {
            if terminal {
                Self::settle_terminal_run(state, agent_id, event_turn_id.as_ref());
                if is_foreground_event {
                    let turn_id = event_turn_id
                        .as_ref()
                        .and_then(JsValue::as_str)
                        .map(str::to_owned);
                    self.finalize_foreground_turn_locked(state, agent_id, turn_id.as_deref());
                }
            }
            if flags.should_dispatch_event {
                self.dispatch_stream_locked(
                    state,
                    agent_id,
                    &event,
                    None,
                    None,
                    Some(crate::clock::now_iso()),
                )?;
            }
        }
        Ok(flags.should_notify_waiters)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the baseline's handler parameters"
    )]
    fn dispatch_stream_event_by_type(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        from_history: bool,
        is_foreground_event: bool,
        event_turn_id: Option<&JsValue>,
        disposition: TerminalDisposition,
        flags: &mut StreamEventFlags,
    ) -> Result<(), AgentError> {
        match event_type(event) {
            Some("thread_started") => self.on_stream_thread_started(state, agent_id),
            Some("usage_updated") => {
                if let Some(agent) = state.agent_mut(agent_id) {
                    agent.snapshot.last_usage = event.get("usage").cloned();
                }
                self.emit_state_locked(state, agent_id, true);
            }
            Some("mode_changed") => self.on_stream_mode_changed(state, agent_id, event, flags),
            Some("model_changed") => self.on_stream_model_changed(state, agent_id, event, flags),
            Some("thinking_option_changed") => {
                self.on_stream_thinking_option_changed(state, agent_id, event, flags);
            }
            Some("timeline") => {
                self.on_stream_timeline_event(state, agent_id, event, from_history, flags)?;
            }
            Some("turn_completed") => {
                self.on_stream_turn_completed(
                    state,
                    agent_id,
                    event,
                    is_foreground_event,
                    disposition,
                );
            }
            Some("turn_failed") => self.on_stream_turn_failed(
                state,
                agent_id,
                event,
                is_foreground_event,
                disposition,
                from_history,
            )?,
            Some("turn_canceled") => self.on_stream_turn_canceled(
                state,
                agent_id,
                event,
                is_foreground_event,
                disposition,
                from_history,
            )?,
            Some("turn_started") => {
                self.on_stream_turn_started(
                    state,
                    agent_id,
                    event_turn_id,
                    is_foreground_event,
                    flags,
                );
            }
            Some("permission_requested") => {
                self.on_stream_permission_requested(state, agent_id, event);
            }
            Some("permission_resolved") => {
                self.on_stream_permission_resolved(state, agent_id, event, from_history, flags);
            }
            _ => {}
        }
        Ok(())
    }

    /// The `thread_started` case of `dispatchStreamEventByType`.
    fn on_stream_thread_started(&self, state: &mut State, agent_id: &str) {
        // `agent.persistence?.sessionId ?? null`.
        let previous = state
            .agent(agent_id)
            .and_then(|agent| agent.snapshot.persistence.clone())
            .and_then(|handle| {
                handle
                    .get("sessionId")
                    .filter(|id| !matches!(id, JsValue::Undefined))
                    .cloned()
            });
        Self::refresh_session_persistence_locked(state, agent_id);
        let current = state
            .agent(agent_id)
            .and_then(|agent| agent.snapshot.persistence.clone())
            .and_then(|handle| handle.get("sessionId").cloned());
        if !strict_equals(
            Some(current.as_ref().unwrap_or(&JsValue::Undefined)),
            Some(previous.as_ref().unwrap_or(&JsValue::Null)),
        ) {
            self.emit_state_locked(state, agent_id, true);
        }
        self.spawn_refresh_runtime_info(agent_id);
    }

    /// The `mode_changed` case of `dispatchStreamEventByType`.
    fn on_stream_mode_changed(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        flags: &mut StreamEventFlags,
    ) {
        if let Some(agent) = state.agent_mut(agent_id) {
            let mode = event
                .get("currentModeId")
                .cloned()
                .unwrap_or(JsValue::Undefined);
            agent.snapshot.current_mode_id = mode.as_str().map(str::to_owned);
            agent.snapshot.available_modes = event
                .get("availableModes")
                .and_then(JsValue::as_array)
                .map(<[JsValue]>::to_vec)
                .unwrap_or_default();
            if let Some(info) = &agent.snapshot.runtime_info {
                let mut copy = spread(Some(info));
                copy.insert("modeId", mode);
                agent.snapshot.runtime_info = Some(JsValue::Object(copy));
            }
        }
        flags.should_dispatch_event = false;
        self.emit_state_locked(state, agent_id, true);
    }

    /// The `model_changed` case of `dispatchStreamEventByType`.
    fn on_stream_model_changed(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        flags: &mut StreamEventFlags,
    ) {
        if let Some(agent) = state.agent_mut(agent_id) {
            let info = event
                .get("runtimeInfo")
                .cloned()
                .unwrap_or(JsValue::Undefined);
            if agent.snapshot.persistence.is_none()
                && let Some(session_id) = info.get("sessionId").filter(|id| truthy(Some(id)))
            {
                let mut handle = JsObject::new();
                handle.insert("provider", JsValue::String(agent.snapshot.provider.clone()));
                handle.insert("sessionId", session_id.clone());
                agent.snapshot.persistence =
                    attach_persistence_cwd(Some(JsValue::Object(handle)), &agent.snapshot.cwd);
            }
            if let Some(mode) = info
                .get("modeId")
                .filter(|mode| !matches!(mode, JsValue::Undefined | JsValue::Null))
            {
                agent.snapshot.current_mode_id = mode.as_str().map(str::to_owned);
            }
            agent.snapshot.runtime_info = Some(info);
        }
        flags.should_dispatch_event = false;
        self.emit_state_locked(state, agent_id, true);
    }

    /// The `thinking_option_changed` case of `dispatchStreamEventByType`.
    fn on_stream_thinking_option_changed(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        flags: &mut StreamEventFlags,
    ) {
        if let Some(agent) = state.agent_mut(agent_id) {
            let option = event
                .get("thinkingOptionId")
                .cloned()
                .unwrap_or(JsValue::Undefined);
            let mut config = spread(Some(&agent.snapshot.config));
            config.insert(
                "thinkingOptionId",
                if option.is_null() {
                    JsValue::Undefined
                } else {
                    option.clone()
                },
            );
            agent.snapshot.config = JsValue::Object(config);
            if let Some(info) = &agent.snapshot.runtime_info {
                let mut copy = spread(Some(info));
                copy.insert("thinkingOptionId", option);
                agent.snapshot.runtime_info = Some(JsValue::Object(copy));
            }
        }
        flags.should_dispatch_event = false;
        self.emit_state_locked(state, agent_id, true);
    }

    /// The `permission_requested` case of `dispatchStreamEventByType`.
    fn on_stream_permission_requested(&self, state: &mut State, agent_id: &str, event: &JsValue) {
        let request = event.get("request").cloned().unwrap_or(JsValue::Undefined);
        let had_pending = state
            .agent(agent_id)
            .is_some_and(|agent| !agent.snapshot.pending_permissions.is_empty());
        if let Some(agent) = state.agent_mut(agent_id) {
            let id = js_string(request.get("id"));
            match agent
                .snapshot
                .pending_permissions
                .iter_mut()
                .find(|(key, _)| *key == id)
            {
                Some((_, existing)) => *existing = request,
                None => agent.snapshot.pending_permissions.push((id, request)),
            }
        }
        Self::refresh_session_persistence_locked(state, agent_id);
        if let Some(agent) = state.agent(agent_id)
            && !had_pending
            && !agent.snapshot.internal
        {
            self.broadcast_agent_attention(agent, "permission");
        }
        self.emit_state_locked(state, agent_id, true);
    }

    /// The `permission_resolved` case of `dispatchStreamEventByType`.
    fn on_stream_permission_resolved(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        from_history: bool,
        flags: &mut StreamEventFlags,
    ) {
        let request_id = js_string(event.get("requestId"));
        if let Some(agent) = state.agent_mut(agent_id) {
            agent
                .snapshot
                .pending_permissions
                .retain(|(id, _)| *id != request_id);
        }
        Self::refresh_session_persistence_locked(state, agent_id);
        let buffered = !from_history
            && state
                .agent(agent_id)
                .is_some_and(|agent| agent.in_flight_permission_responses.contains(&request_id));
        if buffered {
            if let Some(agent) = state.agent_mut(agent_id) {
                match agent
                    .buffered_permission_resolutions
                    .iter_mut()
                    .find(|(id, _)| *id == request_id)
                {
                    Some((_, existing)) => *existing = event.clone(),
                    None => agent
                        .buffered_permission_resolutions
                        .push((request_id, event.clone())),
                }
            }
            flags.should_dispatch_event = false;
        } else {
            self.emit_state_locked(state, agent_id, true);
        }
    }

    /// `onStreamTimelineEvent`.
    fn on_stream_timeline_event(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        from_history: bool,
        flags: &mut StreamEventFlags,
    ) -> Result<(), AgentError> {
        let item = event.get("item").cloned().unwrap_or(JsValue::Undefined);
        let is_user_message = item.get("type").and_then(JsValue::as_str) == Some("user_message");
        if is_user_message && is_system_injected_envelope(&js_string(item.get("text"))) {
            flags.should_dispatch_event = false;
            flags.should_notify_waiters = false;
            return Ok(());
        }
        if is_user_message
            && truthy(item.get("clientMessageId"))
            && self.reconcile_submitted_prompt_echo(state, agent_id, &item, raw_turn_id(event))?
        {
            flags.should_dispatch_event = false;
            flags.should_notify_waiters = false;
            return Ok(());
        }
        if from_history {
            Self::record_timeline_locked(
                state,
                agent_id,
                item,
                event
                    .get("timestamp")
                    .filter(|timestamp| truthy(Some(timestamp)))
                    .map(|timestamp| js_string(Some(timestamp))),
                None,
                None,
            )?;
            flags.should_dispatch_event = false;
            flags.should_notify_waiters = false;
            return Ok(());
        }
        let provider = event.get("provider").cloned().unwrap_or(JsValue::Undefined);
        self.record_and_dispatch_timeline_item_locked(
            state,
            agent_id,
            item,
            provider,
            raw_turn_id(event),
            None,
        )?;
        if is_user_message {
            if let Some(agent) = state.agent_mut(agent_id) {
                agent.snapshot.last_user_message_at_millis = Some(crate::clock::now_millis());
            }
            self.emit_state_locked(state, agent_id, true);
        }
        flags.should_dispatch_event = false;
        flags.should_notify_waiters = true;
        Ok(())
    }

    /// `recordSubmittedPrompt(agent, prompt, clientMessageId, options)`.
    pub(crate) fn record_submitted_prompt_locked(
        &self,
        state: &mut State,
        agent_id: &str,
        prompt: SubmittedPrompt,
    ) -> Result<(), AgentError> {
        let SubmittedPrompt {
            text: prompt_text,
            client_message_id,
            message_id,
            provider_message_id,
            turn_id,
        } = prompt;
        let client_message_id = client_message_id.as_str();
        if state
            .timeline
            .submitted_user_message(agent_id, client_message_id)
            .map_err(|error| AgentError::new(error.to_string()))?
            .is_some()
        {
            return Ok(());
        }
        let provider = match state.agent_mut(agent_id) {
            Some(agent) => {
                touch_updated_at(&mut agent.snapshot);
                agent.snapshot.last_user_message_at_millis = Some(crate::clock::now_millis());
                agent.snapshot.provider.clone()
            }
            None => return Ok(()),
        };
        let mut item = JsObject::new();
        item.insert("type", JsValue::String("user_message".to_owned()));
        item.insert("text", JsValue::String(prompt_text));
        item.insert(
            "clientMessageId",
            JsValue::String(client_message_id.to_owned()),
        );
        if let Some(message_id) = message_id.filter(|id| !id.is_empty()) {
            item.insert("messageId", JsValue::String(message_id));
        }
        self.record_and_dispatch_timeline_item_locked(
            state,
            agent_id,
            JsValue::Object(item),
            JsValue::String(provider),
            turn_id,
            provider_message_id,
        )?;
        Ok(())
    }

    /// `reconcileSubmittedPromptEcho(agent, item, turnId)`: whether the echo
    /// matched a submitted prompt.
    fn reconcile_submitted_prompt_echo(
        &self,
        state: &mut State,
        agent_id: &str,
        item: &JsValue,
        turn_id: Option<JsValue>,
    ) -> Result<bool, AgentError> {
        let client_message_id = js_string(item.get("clientMessageId"));
        let message_id = item
            .get("messageId")
            .filter(|id| truthy(Some(id)))
            .map(|id| js_string(Some(id)));
        let exists = |state: &State| {
            state
                .timeline
                .submitted_user_message(agent_id, &client_message_id)
                .ok()
                .flatten()
        };
        if exists(state).is_none() {
            self.record_submitted_prompt_locked(
                state,
                agent_id,
                SubmittedPrompt {
                    text: js_string(item.get("text")),
                    client_message_id: client_message_id.clone(),
                    message_id: Some(client_message_id.clone()),
                    provider_message_id: message_id.clone(),
                    turn_id: turn_id.filter(|turn| truthy(Some(turn))),
                },
            )?;
        }
        let Some(existing) = exists(state) else {
            return Ok(false);
        };
        if existing.item.get("type").and_then(JsValue::as_str) != Some("user_message") {
            return Ok(false);
        }
        if let Some(message_id) = message_id {
            state
                .timeline
                .enrich_submitted_user_message(agent_id, &client_message_id, &message_id)
                .map_err(|error| AgentError::new(error.to_string()))?;
        }
        Ok(true)
    }

    fn on_stream_turn_completed(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        is_foreground_event: bool,
        disposition: TerminalDisposition,
    ) {
        if disposition == TerminalDisposition::Stale {
            return;
        }
        let mut emit = false;
        if let Some(agent) = state.agent_mut(agent_id) {
            if let Some(usage) = event.get("usage").filter(|usage| truthy(Some(usage))) {
                let mut merged = spread(agent.snapshot.last_usage.as_ref());
                spocky_contracts::js::spread_into(&mut merged, Some(usage));
                agent.snapshot.last_usage = Some(JsValue::Object(merged));
            }
            agent.snapshot.last_error = None;
            if !is_foreground_event
                && agent.snapshot.active_foreground_turn_id.is_none()
                && agent.snapshot.lifecycle != AgentLifecycle::Idle
                && !agent.snapshot.pending_replacement
            {
                agent.snapshot.lifecycle = AgentLifecycle::Idle;
                emit = true;
            }
        }
        if emit {
            self.emit_state_locked(state, agent_id, true);
        }
        self.spawn_refresh_runtime_info(agent_id);
    }

    fn on_stream_turn_failed(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        is_foreground_event: bool,
        disposition: TerminalDisposition,
        from_history: bool,
    ) -> Result<(), AgentError> {
        if disposition == TerminalDisposition::Stale {
            return Ok(());
        }
        let background = !is_foreground_event
            && state
                .agent(agent_id)
                .is_some_and(|agent| agent.snapshot.active_foreground_turn_id.is_none());
        if let Some(agent) = state.agent_mut(agent_id) {
            if background {
                agent.snapshot.lifecycle = AgentLifecycle::Error;
            }
            agent.snapshot.last_error = Some(js_string(event.get("error")));
        }
        let provider = event.get("provider").cloned().unwrap_or(JsValue::Undefined);
        self.append_system_error_timeline_message(
            state,
            agent_id,
            &provider,
            &format_turn_failed_message(event),
            from_history,
        )?;
        self.resolve_pending_permissions_for_agent(
            state,
            agent_id,
            &provider,
            from_history,
            "Turn failed",
        )?;
        if background {
            self.emit_state_locked(state, agent_id, true);
        }
        Ok(())
    }

    fn on_stream_turn_canceled(
        &self,
        state: &mut State,
        agent_id: &str,
        event: &JsValue,
        is_foreground_event: bool,
        disposition: TerminalDisposition,
        from_history: bool,
    ) -> Result<(), AgentError> {
        if disposition == TerminalDisposition::Stale {
            return Ok(());
        }
        let background = !is_foreground_event
            && state
                .agent(agent_id)
                .is_some_and(|agent| agent.snapshot.active_foreground_turn_id.is_none());
        if let Some(agent) = state.agent_mut(agent_id) {
            if background && !agent.snapshot.pending_replacement {
                agent.snapshot.lifecycle = AgentLifecycle::Idle;
            }
            agent.snapshot.last_error = None;
        }
        let provider = event.get("provider").cloned().unwrap_or(JsValue::Undefined);
        self.resolve_pending_permissions_for_agent(
            state,
            agent_id,
            &provider,
            from_history,
            "Interrupted",
        )?;
        if background {
            self.emit_state_locked(state, agent_id, true);
        }
        Ok(())
    }

    fn on_stream_turn_started(
        &self,
        state: &mut State,
        agent_id: &str,
        event_turn_id: Option<&JsValue>,
        is_foreground_event: bool,
        flags: &mut StreamEventFlags,
    ) {
        let has_foreground = state
            .agent(agent_id)
            .is_some_and(|agent| agent.snapshot.active_foreground_turn_id.is_some());
        if is_foreground_event || has_foreground {
            flags.should_dispatch_event = false;
            flags.should_notify_waiters = false;
            return;
        }
        let turn_text = event_turn_id
            .filter(|turn| !turn.is_null())
            .map(|turn| js_string(Some(turn)));
        Self::track_autonomous_run(state, agent_id, turn_text.clone());
        if let Some(agent) = state.agent_mut(agent_id) {
            if let Some(turn_id) = turn_text.filter(|turn| !turn.is_empty()) {
                agent.snapshot.active_turn_id = Some(turn_id);
                agent.snapshot.active_turn_started_at_millis = Some(crate::clock::now_millis());
            }
            agent.snapshot.lifecycle = AgentLifecycle::Running;
        }
        self.emit_state_locked(state, agent_id, true);
    }

    /// `resolvePendingPermissionsForAgent(agent, provider, options, message)`.
    pub(crate) fn resolve_pending_permissions_for_agent(
        &self,
        state: &mut State,
        agent_id: &str,
        provider: &JsValue,
        from_history: bool,
        message: &str,
    ) -> Result<(), AgentError> {
        let pending: Vec<String> = state
            .agent_mut(agent_id)
            .map(|agent| {
                std::mem::take(&mut agent.snapshot.pending_permissions)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default();
        if from_history {
            return Ok(());
        }
        for request_id in pending {
            let mut resolution = JsObject::new();
            resolution.insert("behavior", JsValue::String("deny".to_owned()));
            resolution.insert("message", JsValue::String(message.to_owned()));
            let mut event = JsObject::new();
            event.insert("type", JsValue::String("permission_resolved".to_owned()));
            event.insert("provider", provider.clone());
            event.insert("requestId", JsValue::String(request_id));
            event.insert("resolution", JsValue::Object(resolution));
            self.dispatch_stream_locked(
                state,
                agent_id,
                &JsValue::Object(event),
                None,
                None,
                None,
            )?;
        }
        Ok(())
    }

    /// `appendSystemErrorTimelineMessage(agent, provider, message, options)`.
    fn append_system_error_timeline_message(
        &self,
        state: &mut State,
        agent_id: &str,
        provider: &JsValue,
        message: &str,
        from_history: bool,
    ) -> Result<(), AgentError> {
        if from_history {
            return Ok(());
        }
        let normalized = js_trim(message);
        if normalized.is_empty() {
            return Ok(());
        }
        let text = format!("{SYSTEM_ERROR_PREFIX} {normalized}");
        let last_item = state.timeline.last_item(agent_id).ok().flatten();
        if let Some(last) = last_item
            && last.get("type").and_then(JsValue::as_str) == Some("assistant_message")
            && last.get("text").and_then(JsValue::as_str) == Some(text.as_str())
        {
            return Ok(());
        }
        let mut item = JsObject::new();
        item.insert("type", JsValue::String("assistant_message".to_owned()));
        item.insert("text", JsValue::String(text));
        let item = JsValue::Object(item);
        let row = Self::record_timeline_locked(state, agent_id, item.clone(), None, None, None)?;
        let mut event = JsObject::new();
        event.insert("type", JsValue::String("timeline".to_owned()));
        event.insert("item", item);
        event.insert("provider", provider.clone());
        let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
        self.dispatch_stream_locked(
            state,
            agent_id,
            &JsValue::Object(event),
            Some(row.seq),
            epoch,
            Some(row.timestamp),
        )
    }
}

/// `HydrateTimelineOptions.broadcast`: a flag, or a function the deferred
/// (non-forced) path asks once the history is recorded.
pub enum HydrateBroadcast {
    Now(bool),
    Deferred(Box<dyn Fn() -> bool + Send + Sync>),
}

impl HydrateBroadcast {
    fn evaluate(&self) -> bool {
        match self {
            Self::Now(flag) => *flag,
            Self::Deferred(decide) => decide(),
        }
    }
}

/// `HydrateTimelineOptions`.
#[derive(Default)]
pub struct HydrateTimelineOptions {
    pub force: bool,
    /// `broadcast ?? false`.
    pub broadcast: Option<HydrateBroadcast>,
    /// `broadcastTimeline ?? broadcast`.
    pub broadcast_timeline: Option<bool>,
}

/// Events already read from a provider's history, replayed as a stream.
pub(crate) struct ReplayedHistory(std::vec::IntoIter<JsValue>);

impl crate::agent_sdk::AgentEventStream for ReplayedHistory {
    fn next(
        &mut self,
    ) -> crate::agent_sdk::BoxFuture<'_, Option<crate::agent_sdk::AgentResult<JsValue>>> {
        let next = self.0.next();
        Box::pin(async move { next.map(Ok) })
    }
}

/// `registerSession`'s startup read: the session's whole history,
/// content-limited, before the agent is published, so a provider failure
/// leaves the session unregistered.
pub(crate) async fn read_startup_history(
    session: &dyn crate::agent_sdk::AgentSession,
) -> Result<ReplayedHistory, AgentError> {
    let mut history = session.stream_history();
    let mut events = Vec::new();
    while let Some(event) = history.next().await {
        events.push(limit_stream_event_content(&event?)?);
    }
    Ok(ReplayedHistory(events.into_iter()))
}

/// Provider history split as the hydration paths read it.
struct ReadHistory {
    timeline: Vec<JsValue>,
    subagents: Vec<JsValue>,
}

/// Reads `history` to the end: each event content-limited, user messages
/// that are system-injected envelopes dropped, other non-timeline events
/// ignored. A stream error stops the read and is returned.
async fn read_history(
    mut history: Box<dyn crate::agent_sdk::AgentEventStream>,
) -> Result<ReadHistory, AgentError> {
    let mut read = ReadHistory {
        timeline: Vec::new(),
        subagents: Vec::new(),
    };
    while let Some(event) = history.next().await {
        let event = limit_stream_event_content(&event?)?;
        match event_type(&event) {
            Some("provider_subagent") => read.subagents.push(event),
            Some("timeline") => {
                let item = event.get("item");
                let injected = item
                    .and_then(|item| item.get("type"))
                    .and_then(JsValue::as_str)
                    == Some("user_message")
                    && is_system_injected_envelope(&js_string(
                        item.and_then(|item| item.get("text")),
                    ));
                if !injected {
                    read.timeline.push(event);
                }
            }
            _ => {}
        }
    }
    Ok(read)
}

/// `event.timestamp ? { timestamp } : undefined`.
fn history_timestamp(event: &JsValue) -> Option<String> {
    event
        .get("timestamp")
        .filter(|timestamp| truthy(Some(timestamp)))
        .map(|timestamp| js_string(Some(timestamp)))
}

impl AgentManager {
    /// `hydrateTimelineFromProvider(agentId, options)`: replays the
    /// provider's history into the timeline, once unless `force`.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, a history stream error,
    /// or a timeline `TypeError`.
    pub async fn hydrate_timeline_from_provider(
        &self,
        agent_id: &str,
        options: HydrateTimelineOptions,
    ) -> Result<(), AgentError> {
        let (id, session, primed) = {
            let state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            let Some(session) = agent.session.clone() else {
                return Err(AgentError::new(format!(
                    "Agent '{}' has no managed session",
                    agent.snapshot.id
                )));
            };
            (
                agent.snapshot.id.clone(),
                session,
                agent.snapshot.history_primed,
            )
        };
        if primed && !options.force {
            return Ok(());
        }
        let broadcast = options.broadcast.unwrap_or(HydrateBroadcast::Now(false));
        if options.force {
            let now = broadcast.evaluate();
            let timeline = options
                .broadcast_timeline
                .unwrap_or_else(|| broadcast.evaluate());
            return self
                .force_hydrate_from_history(&id, session.stream_history(), now, timeline)
                .await;
        }
        self.prime_from_history(&id, &broadcast, session.stream_history())
            .await
    }

    /// `forceHydrateTimelineFromLegacyProviderHistory`: the history
    /// replaces the timeline and the provider children.
    async fn force_hydrate_from_history(
        &self,
        agent_id: &str,
        history: Box<dyn crate::agent_sdk::AgentEventStream>,
        broadcast: bool,
        broadcast_timeline: bool,
    ) -> Result<(), AgentError> {
        let read = read_history(history).await?;
        let mut state = self.lock();
        #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
        let now = crate::clock::now_millis() as f64;
        let flushes = state.coalescer.flush_and_discard(agent_id, now);
        let _ = self.apply_coalescer_flushes(&mut state, flushes);
        state.timeline.delete(agent_id);
        state
            .timeline
            .initialize(
                agent_id,
                Vec::new(),
                None,
                None,
                Some(crate::clock::now_iso()),
            )
            .map_err(|error| type_error(&error))?;
        if let Some(agent) = state.agent_mut(agent_id) {
            agent.snapshot.history_primed = true;
        }
        for event in state.provider_subagents.delete_parent(agent_id) {
            if broadcast {
                self.dispatch(&state, AgentManagerEvent::ProviderSubagent(event));
            }
        }
        for event in &read.subagents {
            let update = state
                .provider_subagents
                .apply(
                    agent_id,
                    &js_string(event.get("provider")),
                    event.get("event").unwrap_or(&JsValue::Undefined),
                )
                .map_err(timeline_error)?;
            if broadcast {
                self.dispatch(&state, AgentManagerEvent::ProviderSubagent(update));
            }
        }
        for event in &read.timeline {
            let row = Self::record_timeline_locked(
                &mut state,
                agent_id,
                event.get("item").cloned().unwrap_or(JsValue::Undefined),
                history_timestamp(event),
                None,
                None,
            )?;
            if broadcast_timeline {
                let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
                self.dispatch_stream_locked(
                    &state,
                    agent_id,
                    event,
                    Some(row.seq),
                    epoch,
                    Some(row.timestamp),
                )?;
            }
        }
        if let Some(agent) = state.agent_mut(agent_id) {
            touch_updated_at(&mut agent.snapshot);
        }
        self.emit_state_locked(&mut state, agent_id, true);
        Ok(())
    }

    /// `primeTimelineFromLegacyProviderHistory(agent, broadcast, history)`:
    /// appends the history to the timeline. The whole replay is read
    /// before either store changes, so a failed read leaves them as they
    /// were (and the agent unprimed).
    pub(crate) async fn prime_from_history(
        &self,
        agent_id: &str,
        broadcast: &HydrateBroadcast,
        history: Box<dyn crate::agent_sdk::AgentEventStream>,
    ) -> Result<(), AgentError> {
        if let Some(agent) = self.lock().agent_mut(agent_id) {
            agent.snapshot.history_primed = false;
        }
        let read = read_history(history).await?;
        let deferred = matches!(broadcast, HydrateBroadcast::Deferred(_));
        let immediate = matches!(broadcast, HydrateBroadcast::Now(true));
        let mut state = self.lock();
        let mut subagent_events = Vec::new();
        for event in &read.subagents {
            let update = state
                .provider_subagents
                .apply(
                    agent_id,
                    &js_string(event.get("provider")),
                    event.get("event").unwrap_or(&JsValue::Undefined),
                )
                .map_err(timeline_error)?;
            if deferred {
                subagent_events.push(update);
            } else if immediate {
                self.dispatch(&state, AgentManagerEvent::ProviderSubagent(update));
            }
        }
        let mut timeline_events = Vec::new();
        for event in &read.timeline {
            let row = Self::record_timeline_locked(
                &mut state,
                agent_id,
                event.get("item").cloned().unwrap_or(JsValue::Undefined),
                history_timestamp(event),
                None,
                None,
            )?;
            if deferred {
                timeline_events.push((event, row));
            } else if immediate {
                let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
                self.dispatch_stream_locked(
                    &state,
                    agent_id,
                    event,
                    Some(row.seq),
                    epoch,
                    Some(row.timestamp),
                )?;
            }
        }
        if let Some(agent) = state.agent_mut(agent_id) {
            agent.snapshot.history_primed = true;
        }
        if !deferred || !broadcast.evaluate() {
            return Ok(());
        }
        for event in subagent_events {
            self.dispatch(&state, AgentManagerEvent::ProviderSubagent(event));
        }
        for (event, row) in timeline_events {
            let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
            self.dispatch_stream_locked(
                &state,
                agent_id,
                event,
                Some(row.seq),
                epoch,
                Some(row.timestamp),
            )?;
        }
        Ok(())
    }
}
