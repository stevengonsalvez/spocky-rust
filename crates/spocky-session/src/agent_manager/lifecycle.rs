//! `closeAgent`, `cancelAgentRun` and `respondToPermission` from pinned
//! Paseo `agent/agent-manager.ts`, with the per-agent mutation lanes they
//! run in.

use std::sync::Arc;
use std::time::Duration;

use spocky_contracts::js::{js_string, spread};
use spocky_store::js_value::{JsObject, JsValue};
use tokio::sync::OnceCell;

use super::create::touch_updated_at;
use super::log_error::{err_binding, err_binding_with};
use super::run::TrackedRun;
use super::{AgentLifecycle, AgentManager, AgentManagerEvent, ManagedAgentSnapshot, State};
use crate::agent_projection::{AgentAttention, SnapshotOverrides};
use crate::agent_sdk::{AgentError, AgentSession};

/// `INTERRUPT_SESSION_TIMEOUT_MS`.
pub(crate) const INTERRUPT_SESSION_TIMEOUT_MS: u64 = 2_000;

/// `AgentRunCancellationResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunCancellationResult {
    NotRunning,
    Settled,
    Refused,
}

impl AgentRunCancellationResult {
    /// The `status` string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRunning => "not_running",
            Self::Settled => "settled",
            Self::Refused => "refused",
        }
    }
}

/// `TimeoutResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeoutResult {
    Completed,
    TimedOut,
}

/// `waitWithTimeout`: an error before the timeout rejects; one after it is
/// only logged.
async fn wait_with_timeout(
    operation: impl std::future::Future<Output = Result<(), AgentError>>,
    timeout_ms: u64,
) -> Result<TimeoutResult, AgentError> {
    match tokio::time::timeout(Duration::from_millis(timeout_ms), operation).await {
        Ok(Ok(())) => Ok(TimeoutResult::Completed),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(TimeoutResult::TimedOut),
    }
}

/// Resolves when the run behind `settled` has been cleared.
async fn run_settled(mut settled: tokio::sync::watch::Receiver<bool>) -> Result<(), AgentError> {
    while !*settled.borrow_and_update() {
        if settled.changed().await.is_err() {
            break;
        }
    }
    Ok(())
}

impl AgentManager {
    pub(super) fn lane(
        lanes: &mut std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>,
        agent_id: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(lanes.entry(agent_id.to_owned()).or_default())
    }

    /// `drainSessionEvents(agentId)`: waits until the agent's queued provider
    /// events have been handled.
    pub(crate) async fn drain_session_events_async(&self, agent_id: &str) {
        loop {
            let idle = self.inner.drain_idle.notified();
            if !self.lock().session_queues.contains_key(agent_id) {
                return;
            }
            idle.await;
        }
    }

    /// `waitForAgentClose(agentId)`: waits for the agent's queued lifecycle
    /// mutations, then for an in-flight close, ignoring its error. Loading
    /// during a reload waits for the replacement instead of resuming another
    /// writer.
    pub async fn wait_for_agent_close(&self, agent_id: &str) {
        let lane = self.lock().lifecycle_lanes.get(agent_id).cloned();
        if let Some(lane) = lane {
            drop(lane.lock().await);
        }
        let close = self.lock().inflight_closes.get(agent_id).cloned();
        if let Some(close) = close {
            // The closing call runs the initializer; this only waits for it.
            let _ = close.get_or_init(|| async { Ok(()) }).await;
        }
    }

    /// `closeAgent(agentId)`: concurrent calls share one close.
    ///
    /// # Errors
    ///
    /// The session's close error, or the closed snapshot's persist error
    /// (raised after the closed state is published).
    pub async fn close_agent(&self, agent_id: &str) -> Result<(), AgentError> {
        let close = {
            let mut state = self.lock();
            Arc::clone(
                state
                    .inflight_closes
                    .entry(agent_id.to_owned())
                    .or_default(),
            )
        };
        let result = close
            .get_or_init(|| async {
                let lane = Self::lane(&mut self.lock().lifecycle_lanes, agent_id);
                let _turn = lane.lock().await;
                if self.lock().agent(agent_id).is_some() {
                    self.close_agent_runtime(agent_id).await
                } else {
                    Ok(())
                }
            })
            .await
            .clone();
        let mut state = self.lock();
        if state
            .inflight_closes
            .get(agent_id)
            .is_some_and(|current| Arc::ptr_eq(current, &close))
        {
            state.inflight_closes.remove(agent_id);
        }
        result
    }

    /// `closeAgentRuntime(agentId)`.
    pub(super) async fn close_agent_runtime(&self, agent_id: &str) -> Result<(), AgentError> {
        let session = {
            let state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            agent.session.clone()
        };
        self.drain_session_events_async(agent_id).await;
        if let Some(session) = &session {
            session.close().await?;
        }
        let closed = {
            let mut state = self.lock();
            self.cancel_running_provider_subagents(&mut state, agent_id);
            self.prepare_agent_for_closure(&mut state, agent_id, "agent closed")
        };
        let Some(closed) = closed else {
            return Ok(());
        };
        let persist = self
            .persist_snapshot_of(agent_id, Some(closed.clone()), SnapshotOverrides::default())
            .await;
        {
            let mut state = self.lock();
            self.emit_detached_state_locked(&mut state, closed);
        }
        persist
    }

    /// `setTitle(agentId, title)`: a blank title is ignored; otherwise the
    /// agent is touched, persisted with the trimmed title, and emitted.
    ///
    /// # Errors
    ///
    /// The unknown-agent errors and the persist failure.
    pub async fn set_title(&self, agent_id: &str, title: &str) -> Result<(), AgentError> {
        let id = {
            let state = self.lock();
            Self::require_agent(&state, agent_id)?.snapshot.id.clone()
        };
        let normalized = crate::text::js_trim(title);
        if normalized.is_empty() {
            return Ok(());
        }
        if let Some(agent) = self.lock().agent_mut(&id) {
            touch_updated_at(&mut agent.snapshot);
        }
        self.persist_snapshot(
            &id,
            SnapshotOverrides {
                title: Some(Some(normalized.to_owned())),
                internal: None,
            },
        )
        .await?;
        self.emit_state(&id, false);
        Ok(())
    }

    /// The agent and its session for a setter that needs a live provider
    /// session (`requireSessionAgent`).
    fn require_live_session(
        &self,
        agent_id: &str,
    ) -> Result<(String, Arc<dyn AgentSession>), AgentError> {
        let state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        let Some(session) = agent.session.clone() else {
            return Err(AgentError::new(format!(
                "Agent '{}' has no managed session",
                agent.snapshot.id
            )));
        };
        Ok((agent.snapshot.id.clone(), session))
    }

    /// Sets `key` on the agent's config object, as `agent.config.key = value`
    /// does: an existing key keeps its place.
    fn set_config_value(state: &mut State, agent_id: &str, key: &str, value: JsValue) {
        if let Some(agent) = state.agent_mut(agent_id)
            && let JsValue::Object(config) = &mut agent.snapshot.config
        {
            config.insert(key, value);
        }
    }

    /// `agent.runtimeInfo = { ...agent.runtimeInfo, key: value }` when the
    /// agent has runtime info.
    fn set_runtime_info_value(state: &mut State, agent_id: &str, key: &str, value: JsValue) {
        if let Some(agent) = state.agent_mut(agent_id)
            && let Some(info) = &agent.snapshot.runtime_info
        {
            let mut next = spread(Some(info));
            next.insert(key, value);
            agent.snapshot.runtime_info = Some(JsValue::Object(next));
        }
    }

    /// `touchUpdatedAt` then `emitState(agent)`.
    fn touch_and_emit(&self, agent_id: &str) {
        let mut state = self.lock();
        if let Some(agent) = state.agent_mut(agent_id) {
            touch_updated_at(&mut agent.snapshot);
        }
        self.emit_state_locked(&mut state, agent_id, true);
    }

    /// `setAgentMode(agentId, modeId)`: the provider's notice, if any.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, or the session's.
    pub async fn set_agent_mode(
        &self,
        agent_id: &str,
        mode_id: &str,
    ) -> Result<Option<JsValue>, AgentError> {
        let (id, session) = self.require_live_session(agent_id)?;
        let notice = session.set_mode(mode_id).await?;
        self.drain_session_events_async(&id).await;
        let current = session
            .get_current_mode()
            .await?
            .unwrap_or_else(|| mode_id.to_owned());
        {
            let mut state = self.lock();
            Self::set_config_value(&mut state, &id, "modeId", JsValue::String(current.clone()));
            if let Some(agent) = state.agent_mut(&id) {
                agent.snapshot.current_mode_id = Some(current.clone());
            }
            Self::set_runtime_info_value(&mut state, &id, "modeId", JsValue::String(current));
        }
        self.touch_and_emit(&id);
        Ok(notice)
    }

    /// `setAgentModel(agentId, modelId)`: a blank id clears the model.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, or the session's.
    pub async fn set_agent_model(
        &self,
        agent_id: &str,
        model_id: Option<&str>,
    ) -> Result<(), AgentError> {
        let (id, session) = self.require_live_session(agent_id)?;
        let normalized = model_id.filter(|model| !crate::text::js_trim(model).is_empty());
        if let Some(set_model) = session.set_model(normalized) {
            set_model.await?;
        }
        self.drain_session_events_async(&id).await;
        let value = normalized.map_or(JsValue::Undefined, |model| {
            JsValue::String(model.to_owned())
        });
        {
            let mut state = self.lock();
            Self::set_config_value(&mut state, &id, "model", value.clone());
            let runtime = if matches!(value, JsValue::Undefined) {
                JsValue::Null
            } else {
                value
            };
            Self::set_runtime_info_value(&mut state, &id, "model", runtime);
            Self::refresh_session_persistence_locked(&mut state, &id);
        }
        self.touch_and_emit(&id);
        Ok(())
    }

    /// `setAgentThinkingOption(agentId, thinkingOptionId)`: the provider's
    /// notice, if any; a blank id clears the option, and the session's own
    /// answer wins when it gives one.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, or the session's.
    pub async fn set_agent_thinking_option(
        &self,
        agent_id: &str,
        thinking_option_id: Option<&str>,
    ) -> Result<Option<JsValue>, AgentError> {
        let (id, session) = self.require_live_session(agent_id)?;
        let normalized =
            thinking_option_id.filter(|option| !crate::text::js_trim(option).is_empty());
        let mut notice = None;
        if let Some(set_option) = session.set_thinking_option(normalized) {
            notice = set_option.await?;
        }
        self.drain_session_events_async(&id).await;
        let mut effective =
            normalized.map_or(JsValue::Null, |option| JsValue::String(option.to_owned()));
        let runtime_info = session.get_runtime_info().await?;
        if let Some(reported) = runtime_info
            .get("thinkingOptionId")
            .filter(|reported| !matches!(reported, JsValue::Undefined))
        {
            effective = reported.clone();
        }
        {
            let mut state = self.lock();
            let config_value = if matches!(effective, JsValue::Null) {
                JsValue::Undefined
            } else {
                effective.clone()
            };
            Self::set_config_value(&mut state, &id, "thinkingOptionId", config_value);
            Self::set_runtime_info_value(&mut state, &id, "thinkingOptionId", effective);
        }
        self.touch_and_emit(&id);
        Ok(notice)
    }

    /// `setAgentFeature(agentId, featureId, value)`.
    ///
    /// # Errors
    ///
    /// The unknown-agent error, `Agent session does not support setting
    /// features`, the `TypeError` for a missing session, or the session's.
    pub async fn set_agent_feature(
        &self,
        agent_id: &str,
        feature_id: &str,
        value: JsValue,
    ) -> Result<(), AgentError> {
        let (id, session) = {
            let state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            (agent.snapshot.id.clone(), agent.session.clone())
        };
        // Every map agent holds a session, so the missing session is the
        // `TypeError` pinned reading `setFeature` of `null` would throw.
        let Some(session) = session else {
            return Err(AgentError::named(
                "TypeError".to_owned(),
                "Cannot read properties of null (reading 'setFeature')".to_owned(),
            ));
        };
        let Some(set_feature) = session.set_feature(feature_id, value.clone()) else {
            return Err(AgentError::new(
                "Agent session does not support setting features",
            ));
        };
        set_feature.await?;
        self.drain_session_events_async(&id).await;
        {
            let mut state = self.lock();
            if let Some(agent) = state.agent_mut(&id)
                && let JsValue::Object(config) = &mut agent.snapshot.config
            {
                let mut features = spread(config.get("featureValues"));
                features.insert(feature_id, value);
                config.insert("featureValues", JsValue::Object(features));
            }
        }
        self.touch_and_emit(&id);
        Ok(())
    }

    /// `cancelRunningProviderSubagents(parentAgentId)`: marks each running
    /// provider child canceled and publishes the update.
    pub(super) fn cancel_running_provider_subagents(
        &self,
        state: &mut State,
        parent_agent_id: &str,
    ) {
        for subagent in state.provider_subagents.list(parent_agent_id) {
            if subagent.get("status").and_then(JsValue::as_str) != Some("running") {
                continue;
            }
            let mut cancel = JsObject::new();
            cancel.insert("type", JsValue::String("upsert".to_owned()));
            cancel.insert(
                "id",
                subagent.get("id").cloned().unwrap_or(JsValue::Undefined),
            );
            cancel.insert("status", JsValue::String("canceled".to_owned()));
            let provider = js_string(subagent.get("provider"));
            // An upsert never touches a timeline item, so it cannot fail.
            if let Ok(event) =
                state
                    .provider_subagents
                    .apply(parent_agent_id, &provider, &JsValue::Object(cancel))
            {
                self.dispatch(state, AgentManagerEvent::ProviderSubagent(event));
            }
        }
    }

    /// `prepareAgentForClosure(agent, cancelReason)`: removes the agent,
    /// cancels its turn waiters and run, and returns its closed copy.
    pub(crate) fn prepare_agent_for_closure(
        &self,
        state: &mut State,
        agent_id: &str,
        cancel_reason: &str,
    ) -> Option<ManagedAgentSnapshot> {
        #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
        let now = crate::clock::now_millis() as f64;
        let flushes = state.coalescer.flush_and_discard(agent_id, now);
        let _ = self.apply_coalescer_flushes(state, flushes);
        let index = state.agents.iter().position(|(id, _)| id == agent_id)?;
        let (_, mut agent) = state.agents.remove(index);
        state.previous_statuses.remove(agent_id);
        if let Some(unsubscribe) = agent.unsubscribe_session.take() {
            unsubscribe();
        }
        for waiter in &mut agent.foreground_turn_waiters {
            let mut event = JsObject::new();
            event.insert("type", JsValue::String("turn_canceled".to_owned()));
            event.insert("provider", JsValue::String(agent.snapshot.provider.clone()));
            event.insert("reason", JsValue::String(cancel_reason.to_owned()));
            event.insert("turnId", JsValue::String(waiter.turn_id.clone()));
            if let Some(tx) = waiter.tx.take() {
                let _ = tx.send(JsValue::Object(event));
            }
        }
        Self::clear_run(state, agent_id);
        let mut closed = agent.snapshot;
        closed.lifecycle = AgentLifecycle::Closed;
        closed.active_foreground_turn_id = None;
        closed.active_turn_id = None;
        closed.active_turn_started_at_millis = None;
        closed.pending_permissions = Vec::new();
        closed.pending_replacement = false;
        Some(closed)
    }

    /// `emitState` for an agent no longer in the map (`emitClosedAgent`).
    pub(super) fn emit_detached_state_locked(
        &self,
        state: &mut State,
        snapshot: ManagedAgentSnapshot,
    ) {
        let previous = state
            .previous_statuses
            .insert(snapshot.id.clone(), snapshot.lifecycle);
        let mut snapshot = snapshot;
        if !snapshot.internal && matches!(snapshot.attention, AgentAttention::None) {
            let reason = if previous == Some(AgentLifecycle::Running)
                && snapshot.lifecycle == AgentLifecycle::Idle
            {
                Some("finished")
            } else if previous != Some(AgentLifecycle::Error)
                && snapshot.lifecycle == AgentLifecycle::Error
            {
                Some("error")
            } else {
                None
            };
            if let Some(reason) = reason {
                snapshot.attention = AgentAttention::Required {
                    reason: reason.to_owned(),
                    timestamp_millis: crate::clock::now_millis(),
                };
            }
        }
        self.dispatch(state, AgentManagerEvent::AgentState(Box::new(snapshot)));
    }

    /// `interruptSession(session, agentId)`: whether the interrupt was
    /// acknowledged in time. The interrupt keeps running after a timeout,
    /// and a failure then is only logged.
    async fn interrupt_session(&self, session: &Arc<dyn AgentSession>, agent_id: &str) -> bool {
        let timeout_ms = self.inner.interrupt_session_ms;
        let mut interrupt = tokio::spawn({
            let session = Arc::clone(session);
            async move { session.interrupt().await }
        });
        match tokio::time::timeout(Duration::from_millis(timeout_ms), &mut interrupt).await {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(error))) => {
                self.emit_error(
                    interrupt_bindings(agent_id, Some(&error), None),
                    "Failed to interrupt session",
                );
                false
            }
            Ok(Err(_)) => false,
            Err(_) => {
                #[allow(clippy::cast_precision_loss, reason = "a few seconds in milliseconds")]
                let timeout = timeout_ms as f64;
                self.emit_warn(
                    interrupt_bindings(agent_id, None, Some(timeout)),
                    "Timed out interrupting session during cancel",
                );
                let manager = self.clone();
                let agent_id = agent_id.to_owned();
                tokio::spawn(async move {
                    if let Ok(Err(error)) = interrupt.await {
                        manager.emit_warn(
                            interrupt_bindings(&agent_id, Some(&error), None),
                            "Session interrupt failed after timeout during cancel",
                        );
                    }
                });
                false
            }
        }
    }

    /// `cancelAgentRun(agentId)`, serialized with other foreground mutations.
    ///
    /// # Errors
    ///
    /// The baseline's unknown agent and missing session errors.
    pub async fn cancel_agent_run(
        &self,
        agent_id: &str,
    ) -> Result<AgentRunCancellationResult, AgentError> {
        let lane = Self::lane(&mut self.lock().foreground_lanes, agent_id);
        let _turn = lane.lock().await;
        self.cancel_agent_run_now(agent_id).await
    }

    async fn cancel_agent_run_now(
        &self,
        agent_id: &str,
    ) -> Result<AgentRunCancellationResult, AgentError> {
        let (session, settled, provider) = {
            let mut state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            let Some(session) = agent.session.clone() else {
                return Err(AgentError::new(format!(
                    "Agent '{}' has no managed session",
                    agent.snapshot.id
                )));
            };
            let provider = agent.snapshot.provider.clone();
            let running = agent.snapshot.lifecycle == AgentLifecycle::Running;
            if !state.runs.contains_key(agent_id) && running {
                Self::track_autonomous_run(&mut state, agent_id, None);
            }
            let Some(run) = state.runs.get(agent_id) else {
                return Ok(AgentRunCancellationResult::NotRunning);
            };
            (session, run.settled(), provider)
        };
        let acknowledged = self.interrupt_session(&session, agent_id).await;
        let timeout = if acknowledged {
            INTERRUPT_SESSION_TIMEOUT_MS
        } else {
            self.inner.interrupt_session_ms
        };
        let settlement = wait_with_timeout(run_settled(settled.clone()), timeout)
            .await
            .unwrap_or(TimeoutResult::TimedOut);
        if !acknowledged {
            return Ok(if settlement == TimeoutResult::Completed {
                AgentRunCancellationResult::Settled
            } else {
                AgentRunCancellationResult::Refused
            });
        }
        if settlement == TimeoutResult::TimedOut {
            self.force_cancel_after_timeout(agent_id, &provider, settled)
                .await;
        }
        let mut state = self.lock();
        let has_pending = state
            .agent(agent_id)
            .is_some_and(|agent| !agent.snapshot.pending_permissions.is_empty());
        if has_pending {
            self.resolve_pending_permissions_for_agent(
                &mut state,
                agent_id,
                &JsValue::String(provider),
                false,
                "Interrupted",
            )?;
            if let Some(agent) = state.agent_mut(agent_id) {
                touch_updated_at(&mut agent.snapshot);
            }
            self.emit_state_locked(&mut state, agent_id, true);
        }
        Ok(AgentRunCancellationResult::Settled)
    }

    /// The timed-out branches of `cancelAgentRunNow`.
    async fn force_cancel_after_timeout(
        &self,
        agent_id: &str,
        provider: &str,
        settled: tokio::sync::watch::Receiver<bool>,
    ) {
        // Logged once the state lock is released, so a sink may call back into
        // the manager.
        let mut warning = None;
        let wait_for_settle = {
            let mut state = self.lock();
            let run = state.runs.get(agent_id);
            let turn_id = run.and_then(TrackedRun::turn_id);
            let foreground_token = match run {
                Some(TrackedRun::Foreground { token, .. }) => Some(*token),
                _ => None,
            };
            let autonomous = matches!(run, Some(TrackedRun::Autonomous { .. }));
            let mut warn_cancel = |turn_id: Option<&str>, kind: &str, message: &'static str| {
                let mut bindings = JsObject::new();
                bindings.insert("agentId", JsValue::String(agent_id.to_owned()));
                if let Some(turn_id) = turn_id {
                    bindings.insert("turnId", JsValue::String(turn_id.to_owned()));
                }
                bindings.insert("kind", JsValue::String(kind.to_owned()));
                warning = Some((JsValue::Object(bindings), message));
            };
            let kind = if autonomous {
                "autonomous"
            } else {
                "foreground"
            };
            let mut event = JsObject::new();
            event.insert("type", JsValue::String("turn_canceled".to_owned()));
            event.insert("provider", JsValue::String(provider.to_owned()));
            event.insert("reason", JsValue::String("interrupted".to_owned()));
            if let Some(turn_id) = turn_id.filter(|turn| !turn.is_empty()) {
                warn_cancel(
                    Some(&turn_id),
                    kind,
                    "cancelAgentRun: acknowledged turn still active after timeout, force-canceling",
                );
                event.insert("turnId", JsValue::String(turn_id));
                let _ = self.dispatch_session_event_locked(
                    &mut state,
                    agent_id,
                    &JsValue::Object(event),
                );
                true
            } else if let Some(token) = foreground_token {
                warn_cancel(
                    None,
                    kind,
                    "cancelAgentRun: acknowledged pending turn still active after timeout, clearing it",
                );
                Self::settle_foreground_run(&mut state, agent_id, token);
                let replacing = state
                    .agent(agent_id)
                    .is_some_and(|agent| agent.snapshot.pending_replacement);
                if !replacing {
                    if let Some(agent) = state.agent_mut(agent_id) {
                        agent.snapshot.lifecycle = AgentLifecycle::Idle;
                        touch_updated_at(&mut agent.snapshot);
                    }
                    self.emit_state_locked(&mut state, agent_id, true);
                }
                false
            } else {
                if autonomous {
                    warn_cancel(
                        None,
                        kind,
                        "cancelAgentRun: acknowledged turn still active after timeout, force-canceling",
                    );
                    let _ = self.dispatch_session_event_locked(
                        &mut state,
                        agent_id,
                        &JsValue::Object(event),
                    );
                }
                false
            }
        };
        if let Some((bindings, message)) = warning {
            self.emit_warn(bindings, message);
        }
        if wait_for_settle {
            let _ = run_settled(settled).await;
        }
    }

    /// `getPendingPermissions(agentId)`.
    ///
    /// # Errors
    ///
    /// The baseline's unknown agent and missing session errors.
    pub fn get_pending_permissions(&self, agent_id: &str) -> Result<Vec<JsValue>, AgentError> {
        let state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        if agent.session.is_none() {
            return Err(AgentError::new(format!(
                "Agent '{}' has no managed session",
                agent.snapshot.id
            )));
        }
        Ok(agent
            .snapshot
            .pending_permissions
            .iter()
            .map(|(_, request)| request.clone())
            .collect())
    }

    /// `respondToPermission(agentId, requestId, response)`: the session's
    /// `AgentPermissionResult`, if any.
    ///
    /// # Errors
    ///
    /// The baseline's unknown agent and duplicate response errors, the
    /// session's error, or the snapshot's persist error.
    pub async fn respond_to_permission(
        &self,
        agent_id: &str,
        request_id: &str,
        response: JsValue,
    ) -> Result<Option<JsValue>, AgentError> {
        let session = {
            let mut state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            if agent
                .in_flight_permission_responses
                .iter()
                .any(|id| id == request_id)
            {
                return Err(AgentError::new(
                    "A response to this permission request is already being submitted",
                ));
            }
            let session = agent.session.clone();
            if let Some(agent) = state.agent_mut(agent_id) {
                agent
                    .in_flight_permission_responses
                    .push(request_id.to_owned());
            }
            session
        };
        let result = self
            .respond_to_permission_inner(agent_id, request_id, response, session)
            .await;
        if let Some(agent) = self.lock().agent_mut(agent_id) {
            agent
                .in_flight_permission_responses
                .retain(|id| id != request_id);
            agent
                .buffered_permission_resolutions
                .retain(|(id, _)| id != request_id);
        }
        result
    }

    async fn respond_to_permission_inner(
        &self,
        agent_id: &str,
        request_id: &str,
        response: JsValue,
        session: Option<Arc<dyn AgentSession>>,
    ) -> Result<Option<JsValue>, AgentError> {
        let Some(session) = session else {
            return Err(AgentError::named(
                "TypeError".to_owned(),
                "Cannot read properties of null (reading 'respondToPermission')".to_owned(),
            ));
        };
        let result = session.respond_to_permission(request_id, response).await?;
        if let Some(agent) = self.lock().agent_mut(agent_id) {
            agent
                .snapshot
                .pending_permissions
                .retain(|(id, _)| id != request_id);
        }
        self.refresh_session_state(agent_id, true).await;
        if let Some(agent) = self.lock().agent_mut(agent_id) {
            touch_updated_at(&mut agent.snapshot);
        }
        self.persist_snapshot(agent_id, SnapshotOverrides::default())
            .await?;
        let mut state = self.lock();
        self.emit_state_locked(&mut state, agent_id, true);
        let buffered = state.agent_mut(agent_id).and_then(|agent| {
            let index = agent
                .buffered_permission_resolutions
                .iter()
                .position(|(id, _)| id == request_id)?;
            Some(agent.buffered_permission_resolutions.remove(index).1)
        });
        if let Some(buffered) = buffered {
            self.dispatch_stream_locked(
                &state,
                agent_id,
                &buffered,
                None,
                None,
                Some(crate::clock::now_iso()),
            )?;
        }
        Ok(result)
    }

    /// `persistSnapshot` for `agent_id`, or for `detached` when the agent has
    /// left the map.
    pub(crate) async fn persist_snapshot_of(
        &self,
        agent_id: &str,
        detached: Option<ManagedAgentSnapshot>,
        overrides: SnapshotOverrides,
    ) -> Result<(), AgentError> {
        self.persist_snapshot_raw(agent_id, detached, overrides)
            .await
            .map_err(|error| match error {
                crate::agent_storage::StorageError::Projection(error) => {
                    AgentError::named("TypeError".to_owned(), error.0.clone())
                }
                crate::agent_storage::StorageError::Store(error) => {
                    AgentError::new(error.to_string())
                }
            })
    }

    /// [`Self::persist_snapshot_of`] with the storage error as it came, for
    /// callers that log the error's own properties.
    pub(crate) async fn persist_snapshot_raw(
        &self,
        agent_id: &str,
        detached: Option<ManagedAgentSnapshot>,
        overrides: SnapshotOverrides,
    ) -> Result<(), crate::agent_storage::StorageError> {
        let Some(registry) = self.inner.registry.clone() else {
            return Ok(());
        };
        let Some(fallback) = detached.or_else(|| self.get_agent(agent_id)) else {
            return Ok(());
        };
        if fallback.internal {
            return Ok(());
        }
        let inner = Arc::clone(&self.inner);
        let id = agent_id.to_owned();
        registry
            .apply_snapshot(
                agent_id,
                move || {
                    super::lock(&inner.state).agent(&id).map_or_else(
                        || fallback.record_view(),
                        |agent| agent.snapshot.record_view(),
                    )
                },
                overrides,
            )
            .await
    }
}

/// The bindings of the interrupt logs: `{ err, agentId }` for a failure,
/// `{ agentId, timeoutMs }` for the timeout.
fn interrupt_bindings(
    agent_id: &str,
    error: Option<&AgentError>,
    timeout_ms: Option<f64>,
) -> JsValue {
    let mut bindings = JsObject::new();
    if let Some(error) = error {
        bindings.insert("err", err_binding(error));
    }
    bindings.insert("agentId", JsValue::String(agent_id.to_owned()));
    if let Some(timeout_ms) = timeout_ms {
        bindings.insert("timeoutMs", JsValue::Number(timeout_ms));
    }
    JsValue::Object(bindings)
}

/// The `err` binding pino prints for a storage failure. A file system error
/// is an `Error` with its `errno`, `code`, `syscall`, `path` and `dest`; a
/// record that does not project is the `TypeError` it threw.
pub(crate) fn storage_error_binding(error: &crate::agent_storage::StorageError) -> JsValue {
    use crate::agent_storage::StorageError;
    match error {
        StorageError::Projection(error) => err_binding_with("TypeError", &error.0, Vec::new()),
        StorageError::Store(store) => {
            let mut extras = Vec::new();
            if let spocky_store::StoreError::Fs(fs) = &**store {
                if let Some(errno) = fs.source.raw_os_error() {
                    extras.push(("errno", JsValue::Number(-f64::from(errno))));
                }
                extras.push(("code", JsValue::String(fs.code())));
                extras.push(("syscall", JsValue::String(fs.syscall.to_owned())));
                if let Some(path) = &fs.path {
                    extras.push(("path", JsValue::String(path.clone())));
                }
                if let Some(dest) = &fs.dest {
                    extras.push(("dest", JsValue::String(dest.clone())));
                }
            }
            err_binding_with("Error", &store.to_string(), extras)
        }
    }
}

/// A shared close result.
pub(crate) type SharedClose = Arc<OnceCell<Result<(), AgentError>>>;
