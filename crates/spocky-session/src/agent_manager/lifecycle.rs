//! `closeAgent`, `cancelAgentRun` and `respondToPermission` from pinned
//! Paseo `agent/agent-manager.ts`, with the per-agent mutation lanes they
//! run in.

use std::sync::Arc;
use std::time::Duration;

use spocky_contracts::js::js_string;
use spocky_store::js_value::{JsObject, JsValue};
use tokio::sync::OnceCell;

use super::create::touch_updated_at;
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

    /// `cancelRunningProviderSubagents(parentAgentId)`: marks each running
    /// provider child canceled and publishes the update.
    fn cancel_running_provider_subagents(&self, state: &mut State, parent_agent_id: &str) {
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
    fn emit_detached_state_locked(&self, state: &mut State, snapshot: ManagedAgentSnapshot) {
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
    /// acknowledged in time.
    async fn interrupt_session(&self, session: &Arc<dyn AgentSession>) -> bool {
        matches!(
            wait_with_timeout(session.interrupt(), self.inner.interrupt_session_ms).await,
            Ok(TimeoutResult::Completed)
        )
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
        let acknowledged = self.interrupt_session(&session).await;
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
        let wait_for_settle = {
            let mut state = self.lock();
            let run = state.runs.get(agent_id);
            let turn_id = run.and_then(TrackedRun::turn_id);
            let foreground_token = match run {
                Some(TrackedRun::Foreground { token, .. }) => Some(*token),
                _ => None,
            };
            let autonomous = matches!(run, Some(TrackedRun::Autonomous { .. }));
            let mut event = JsObject::new();
            event.insert("type", JsValue::String("turn_canceled".to_owned()));
            event.insert("provider", JsValue::String(provider.to_owned()));
            event.insert("reason", JsValue::String("interrupted".to_owned()));
            if let Some(turn_id) = turn_id.filter(|turn| !turn.is_empty()) {
                event.insert("turnId", JsValue::String(turn_id));
                let _ = self.dispatch_session_event_locked(
                    &mut state,
                    agent_id,
                    &JsValue::Object(event),
                );
                true
            } else if let Some(token) = foreground_token {
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
                    let _ = self.dispatch_session_event_locked(
                        &mut state,
                        agent_id,
                        &JsValue::Object(event),
                    );
                }
                false
            }
        };
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
            return Err(AgentError {
                name: "TypeError".to_owned(),
                message: "Cannot read properties of null (reading 'respondToPermission')"
                    .to_owned(),
            });
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
            .map_err(|error| match error {
                crate::agent_storage::StorageError::Projection(error) => AgentError {
                    name: "TypeError".to_owned(),
                    message: error.0.clone(),
                },
                crate::agent_storage::StorageError::Store(error) => {
                    AgentError::new(error.to_string())
                }
            })
    }
}

/// A shared close result.
pub(crate) type SharedClose = Arc<OnceCell<Result<(), AgentError>>>;
