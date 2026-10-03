//! Foreground runs from pinned Paseo `agent/agent-run-state.ts` and
//! `agent/agent-manager.ts`: `streamAgent`, `runAgent`, turn finalization,
//! and `waitForAgentEvent`.

use std::sync::{Arc, Mutex};

use spocky_store::js_value::{JsObject, JsValue};
use tokio::sync::{mpsc, oneshot, watch};

use super::create::{attach_persistence_cwd, touch_updated_at};
use super::events::{HydrateBroadcast, HydrateTimelineOptions};
use super::events::{
    SubmittedPrompt, TerminalDisposition, event_type, format_turn_failed_message,
    is_turn_terminal_event, raw_turn_id,
};
use super::lifecycle::AgentRunCancellationResult;
use super::log_error::err_binding;
use super::trace;
use super::{AgentLifecycle, AgentManager, AgentManagerEvent, State, SubscribeOptions};
use crate::agent_projection::SnapshotOverrides;
use crate::agent_prompt::submitted_prompt_text;
use crate::agent_sdk::{
    AbortReason, AbortSignal, AgentError, AgentPromptInput, AgentRunOptions, AgentSession,
    SteerActiveTurnOptions, SteerResult, StreamCallback,
};
use crate::rewind::{RewindMode, invoke_rewind_capability};
use spocky_contracts::js::{js_string, truthy};

/// `AgentSteerOptions`: run options plus whether to clear pending
/// permissions.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentSteerOptions {
    pub run: AgentRunOptions,
    pub clear_pending_permissions: Option<bool>,
}

/// `ActiveTurnSteerDispatchResult`.
pub enum SteerDispatch {
    Inactive,
    Steered,
    /// The turn was replaced; this is the new turn's stream.
    Replaced(Box<TurnEventStream>),
}

/// What a steer targets: the agent, its session and the turn it expects.
struct SteerTarget {
    id: String,
    session: Arc<dyn AgentSession>,
    expected: Option<String>,
}

/// `finalizedForegroundTurnIds` keeps at most this many ids.
const FINALIZED_TURN_LIMIT: usize = 50;

/// `ForegroundTurnWaiter`: `tx` is `None` once settled.
pub(crate) struct ForegroundTurnWaiter {
    pub(crate) id: u64,
    pub(crate) turn_id: String,
    pub(crate) tx: Option<mpsc::UnboundedSender<JsValue>>,
}

/// `PendingForegroundRun.start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunStart {
    Pending,
    Started(String),
    Failed(String),
}

/// `TrackedAgentRun`. `settled` turns true when the run is cleared
/// (`settledPromise`).
pub(crate) enum TrackedRun {
    Foreground {
        token: u64,
        start: RunStart,
        staged: Vec<JsValue>,
        settled: watch::Sender<bool>,
    },
    Autonomous {
        turn_id: Option<String>,
        settled: watch::Sender<bool>,
    },
}

impl TrackedRun {
    /// A receiver that sees the run settle.
    pub(crate) fn settled(&self) -> watch::Receiver<bool> {
        match self {
            Self::Foreground { settled, .. } | Self::Autonomous { settled, .. } => {
                settled.subscribe()
            }
        }
    }

    /// `runs.getTurnId`: the autonomous turn, or the started foreground turn.
    pub(crate) fn turn_id(&self) -> Option<String> {
        match self {
            Self::Autonomous { turn_id, .. } => turn_id.clone(),
            Self::Foreground {
                start: RunStart::Started(turn_id),
                ..
            } => Some(turn_id.clone()),
            Self::Foreground { .. } => None,
        }
    }
}

/// `AgentRunResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunResult {
    pub session_id: String,
    pub final_text: String,
    pub usage: Option<JsValue>,
    pub timeline: Vec<JsValue>,
    pub canceled: bool,
}

impl AgentRunResult {
    /// The object `runAgent` resolves: `usage` stays an own `undefined`
    /// property when the turn reported none.
    #[must_use]
    pub fn to_js(&self) -> JsValue {
        let mut value = JsObject::new();
        value.insert("sessionId", JsValue::String(self.session_id.clone()));
        value.insert("finalText", JsValue::String(self.final_text.clone()));
        value.insert("usage", self.usage.clone().unwrap_or(JsValue::Undefined));
        value.insert("timeline", JsValue::Array(self.timeline.clone()));
        value.insert("canceled", JsValue::Bool(self.canceled));
        JsValue::Object(value)
    }
}

/// `WaitForAgentOptions`.
#[derive(Debug, Clone, Default)]
pub struct WaitForAgentOptions {
    pub signal: Option<AbortSignal>,
    pub wait_for_active: bool,
}

/// The fallback message of `waitForAgentEvent`'s abort error.
const WAIT_ABORTED: &str = "wait_for_agent aborted";

/// `createAbortError(signal, fallbackMessage)`: an `AbortError` whose
/// message is `abortMessage(signal.reason)`: a string reason, an `Error`
/// reason's message, else the fallback.
fn abort_error(signal: &AbortSignal, fallback: &str) -> AgentError {
    let message = match signal.reason() {
        Some(AbortReason::Value(JsValue::String(reason))) => reason.clone(),
        Some(AbortReason::Error(error)) => error.message.clone(),
        _ => fallback.to_owned(),
    };
    AgentError::named("AbortError", message)
}

/// Runs the wait's `unsubscribe` when the wait ends, however it ends:
/// settled, aborted, or dropped by its caller.
struct Unsubscribing<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for Unsubscribing<F> {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

/// `WaitForAgentResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitForAgentResult {
    pub status: AgentLifecycle,
    pub permission: Option<JsValue>,
    pub last_message: Option<String>,
}

impl WaitForAgentResult {
    /// The object `waitForAgentEvent` resolves.
    #[must_use]
    pub fn to_js(&self) -> JsValue {
        let mut value = JsObject::new();
        value.insert("status", JsValue::String(self.status.as_str().to_owned()));
        value.insert(
            "permission",
            self.permission.clone().unwrap_or(JsValue::Null),
        );
        value.insert(
            "lastMessage",
            self.last_message
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        JsValue::Object(value)
    }
}

impl AgentManager {
    fn next_token(state: &mut State) -> u64 {
        state.next_token += 1;
        state.next_token
    }

    /// `runs.settleTerminalRun(agentId, turnId)`.
    pub(crate) fn settle_terminal_run(
        state: &mut State,
        agent_id: &str,
        turn_id: Option<&JsValue>,
    ) {
        let keep = match state.runs.get(agent_id) {
            None => return,
            Some(TrackedRun::Foreground { start, .. }) => match start {
                RunStart::Started(started) => {
                    turn_id.and_then(JsValue::as_str) != Some(started.as_str())
                }
                _ => true,
            },
            Some(TrackedRun::Autonomous {
                turn_id: run_turn, ..
            }) => run_turn.as_ref().is_some_and(|run_turn| {
                turn_id.is_some() && turn_id.and_then(JsValue::as_str) != Some(run_turn.as_str())
            }),
        };
        if !keep {
            Self::clear_run(state, agent_id);
        }
    }

    /// `runs.trackAutonomousRun(agentId, turnId)`.
    pub(crate) fn track_autonomous_run(state: &mut State, agent_id: &str, turn_id: Option<String>) {
        state
            .runs
            .entry(agent_id.to_owned())
            .or_insert_with(|| TrackedRun::Autonomous {
                turn_id,
                settled: watch::channel(false).0,
            });
    }

    /// `runs.settleForegroundRun(agentId, token)`.
    pub(crate) fn settle_foreground_run(state: &mut State, agent_id: &str, token: u64) {
        if matches!(state.runs.get(agent_id), Some(TrackedRun::Foreground { token: current, .. }) if *current == token)
        {
            Self::clear_run(state, agent_id);
        }
    }

    /// `clearRun`: drops the run and settles it.
    pub(crate) fn clear_run(state: &mut State, agent_id: &str) {
        if let Some(run) = state.runs.remove(agent_id) {
            match run {
                TrackedRun::Foreground { settled, .. } | TrackedRun::Autonomous { settled, .. } => {
                    settled.send_replace(true);
                }
            }
        }
    }

    /// `finalizeForegroundTurn(agent, turnId)`.
    pub(crate) fn finalize_foreground_turn_locked(
        &self,
        state: &mut State,
        agent_id: &str,
        turn_id: Option<&str>,
    ) {
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        if let Some(turn_id) = turn_id.filter(|turn| !turn.is_empty()) {
            agent.finalized_foreground_turn_ids.push(turn_id.to_owned());
            if agent.finalized_foreground_turn_ids.len() > FINALIZED_TURN_LIMIT {
                agent.finalized_foreground_turn_ids.remove(0);
            }
        }
        agent.snapshot.active_foreground_turn_id = None;
        let turn_value = turn_id.map(|turn| JsValue::String(turn.to_owned()));
        let _: TerminalDisposition =
            Self::apply_active_turn_terminal(agent, turn_value.as_ref(), false);
        let terminal_error = agent
            .snapshot
            .last_error
            .as_ref()
            .is_some_and(|error| !error.is_empty());
        let hold_busy = agent.snapshot.pending_replacement && !terminal_error;
        agent.snapshot.lifecycle = if hold_busy {
            AgentLifecycle::Running
        } else if terminal_error {
            AgentLifecycle::Error
        } else {
            AgentLifecycle::Idle
        };
        let handle = agent
            .session
            .as_ref()
            .and_then(|session| session.describe_persistence())
            .or_else(|| {
                let session_id = agent
                    .snapshot
                    .runtime_info
                    .as_ref()
                    .and_then(|info| info.get("sessionId"))
                    .filter(|id| truthy(Some(id)))?
                    .clone();
                let mut handle = JsObject::new();
                handle.insert("provider", JsValue::String(agent.snapshot.provider.clone()));
                handle.insert("sessionId", session_id);
                Some(JsValue::Object(handle))
            });
        if let Some(attached) = attach_persistence_cwd(handle, &agent.snapshot.cwd) {
            agent.snapshot.persistence = Some(attached);
        }
        self.emit_trace(
            || {
                let snapshot = &agent.snapshot;
                trace::bindings([
                    ("agentId", trace::text(agent_id)),
                    ("provider", trace::text(&snapshot.provider)),
                    ("sessionId", trace::session_id(snapshot)),
                    ("turnId", turn_id.map_or(JsValue::Undefined, trace::text)),
                    ("lifecycle", trace::lifecycle(snapshot)),
                    (
                        "terminalError",
                        snapshot
                            .last_error
                            .as_deref()
                            .map_or(JsValue::Undefined, trace::text),
                    ),
                    (
                        "pendingReplacement",
                        JsValue::Bool(snapshot.pending_replacement),
                    ),
                ])
            },
            "agent.manager.finalize",
        );
        if !hold_busy {
            touch_updated_at(&mut agent.snapshot);
            self.emit_state_locked(state, agent_id, true);
        }
    }

    /// `replaceAgentRun(agentId, prompt, options)`: an idle agent just
    /// streams the prompt; a busy one is marked as being replaced
    /// (`pendingReplacement`, running), its run is cancelled, then the prompt
    /// streams. An unacknowledged cancellation fails the replacement and
    /// clears the mark.
    ///
    /// # Errors
    ///
    /// `Unknown agent`, a missing session, `Cannot replace agent <id> because
    /// its active run cancellation was not acknowledged`, or the cancel or
    /// stream error.
    pub async fn replace_agent_run(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> Result<TurnEventStream, AgentError> {
        {
            let mut state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            let id = agent.snapshot.id.clone();
            if agent.snapshot.lifecycle != AgentLifecycle::Running
                && agent.snapshot.active_foreground_turn_id.is_none()
                && !state.runs.contains_key(&id)
            {
                drop(state);
                return self.stream_agent(agent_id, prompt, options);
            }
            if agent.session.is_none() {
                return Err(AgentError::new(format!(
                    "Agent '{id}' has no managed session"
                )));
            }
            self.mark_replacing_locked(&mut state, &id);
        }
        self.cancel_and_stream_replacement(agent_id, prompt, options)
            .await
    }

    /// The agent is busy for a replacement: pending, running, touched and
    /// published.
    fn mark_replacing_locked(&self, state: &mut State, id: &str) {
        if let Some(agent) = state.agent_mut(id) {
            agent.snapshot.pending_replacement = true;
            agent.snapshot.lifecycle = AgentLifecycle::Running;
            touch_updated_at(&mut agent.snapshot);
        }
        self.emit_state_locked(state, id, true);
    }

    /// Cancels the active run, then streams the replacement; the pending
    /// mark is cleared when either fails.
    async fn cancel_and_stream_replacement(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> Result<TurnEventStream, AgentError> {
        let attempt = async {
            self.cancel_agent_run_before(agent_id, "replace").await?;
            self.stream_agent(agent_id, prompt, options)
        }
        .await;
        if attempt.is_err()
            && let Some(agent) = self.lock().agent_mut(agent_id)
        {
            agent.snapshot.pending_replacement = false;
        }
        attempt
    }

    /// `streamAgent(agentId, prompt, options)`: checks run now; the turn
    /// starts on the first [`TurnEventStream::next`].
    ///
    /// # Errors
    ///
    /// The baseline's unknown agent, missing session, and active run errors.
    pub fn stream_agent(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> Result<TurnEventStream, AgentError> {
        let mut state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        let normalized = agent.snapshot.id.clone();
        let Some(session) = agent.session.clone() else {
            return Err(AgentError::new(format!(
                "Agent '{normalized}' has no managed session"
            )));
        };
        self.emit_trace(
            || {
                let snapshot = &agent.snapshot;
                trace::bindings([
                    ("agentId", trace::text(agent_id)),
                    ("provider", trace::text(&snapshot.provider)),
                    ("sessionId", trace::session_id(snapshot)),
                    ("turnId", trace::foreground_turn_id_or_undefined(snapshot)),
                    ("lifecycle", trace::lifecycle(snapshot)),
                    (
                        "activeForegroundTurnId",
                        trace::foreground_turn_id(snapshot),
                    ),
                    (
                        "hasTrackedRun",
                        JsValue::Bool(state.runs.contains_key(&normalized)),
                    ),
                    (
                        "promptType",
                        trace::text(match prompt {
                            AgentPromptInput::Text(_) => "string",
                            AgentPromptInput::Blocks(_) => "structured",
                        }),
                    ),
                    ("hasRunOptions", JsValue::Bool(options.is_some())),
                ])
            },
            "agent.manager.stream.request",
        );
        if agent.snapshot.active_foreground_turn_id.is_some()
            || state.runs.contains_key(&normalized)
        {
            self.emit_trace(
                || {
                    let snapshot = &agent.snapshot;
                    trace::bindings([
                        ("agentId", trace::text(agent_id)),
                        ("provider", trace::text(&snapshot.provider)),
                        ("sessionId", trace::session_id(snapshot)),
                        ("turnId", trace::foreground_turn_id_or_undefined(snapshot)),
                        ("lifecycle", trace::lifecycle(snapshot)),
                        (
                            "hasTrackedRun",
                            JsValue::Bool(state.runs.contains_key(&normalized)),
                        ),
                    ])
                },
                "agent.manager.stream.reject",
            );
            return Err(AgentError::new(format!(
                "Agent {normalized} already has an active run"
            )));
        }
        let mut is_replacement = false;
        if let Some(agent) = state.agent_mut(&normalized) {
            is_replacement = agent.snapshot.pending_replacement;
            agent.snapshot.last_error = None;
        }
        let token = Self::next_token(&mut state);
        state.runs.insert(
            normalized.clone(),
            TrackedRun::Foreground {
                token,
                start: RunStart::Pending,
                staged: Vec::new(),
                settled: watch::channel(false).0,
            },
        );
        Ok(TurnEventStream {
            manager: self.clone(),
            agent_id: normalized,
            phase: Phase::Start {
                session,
                prompt,
                options,
                is_replacement,
            },
            token,
            waiter_id: None,
        })
    }

    /// The agent, its session and the turn a steer would target
    /// (`activeForegroundTurnId ?? activeTurnId`, none when empty).
    fn steer_target(&self, agent_id: &str) -> Result<SteerTarget, AgentError> {
        let state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        let id = agent.snapshot.id.clone();
        let Some(session) = agent.session.clone() else {
            return Err(AgentError::new(format!(
                "Agent '{id}' has no managed session"
            )));
        };
        let expected = agent
            .snapshot
            .active_foreground_turn_id
            .clone()
            .or_else(|| agent.snapshot.active_turn_id.clone())
            .filter(|turn| !turn.is_empty());
        Ok(SteerTarget {
            id,
            session,
            expected,
        })
    }

    /// `assertSteerAdmissionOwnsTurn(agent, expectedTurnId)`.
    fn assert_steer_admission_owns_turn(&self, id: &str, expected: &str) -> Result<(), AgentError> {
        let owns = self
            .lock()
            .agent(id)
            .is_some_and(|agent| agent.snapshot.active_turn_id.as_deref() == Some(expected));
        if owns {
            Ok(())
        } else {
            Err(AgentError::new(
                "Active turn changed before steering could be delivered",
            ))
        }
    }

    /// `recordAcceptedSteer(agent, prompt, clientMessageId, expectedTurnId)`.
    fn record_accepted_steer(
        &self,
        id: &str,
        prompt: &AgentPromptInput,
        client_message_id: Option<&str>,
        expected: &str,
    ) -> Result<(), AgentError> {
        let Some(client_message_id) = client_message_id.filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let mut state = self.lock();
        self.record_submitted_prompt_locked(
            &mut state,
            id,
            SubmittedPrompt {
                text: submitted_prompt_text(prompt),
                client_message_id: client_message_id.to_owned(),
                message_id: Some(client_message_id.to_owned()),
                provider_message_id: None,
                turn_id: Some(JsValue::String(expected.to_owned())),
            },
        )?;
        self.emit_state_locked(&mut state, id, true);
        Ok(())
    }

    /// `runSteerAdmission`: in the agent's foreground lane, the session's
    /// queued events are drained and its stream buffer flushed, the turn is
    /// confirmed, and the provider's events during the steer are held until
    /// it has answered.
    async fn run_steer_admission(
        &self,
        id: &str,
        session: &Arc<dyn AgentSession>,
        expected: &str,
        prompt: &AgentPromptInput,
        options: &AgentSteerOptions,
    ) -> Result<SteerResult, AgentError> {
        let lane = Self::lane(&mut self.lock().foreground_lanes, id);
        let _turn = lane.lock().await;
        self.drain_session_events_async(id).await;
        {
            let mut state = self.lock();
            #[allow(clippy::cast_precision_loss, reason = "Date.now() is a double")]
            let now = crate::clock::now_millis() as f64;
            let flushes = state.coalescer.flush_for(id, now);
            self.apply_coalescer_flushes(&mut state, flushes)?;
        }
        self.assert_steer_admission_owns_turn(id, expected)?;
        self.lock()
            .steer_event_barriers
            .insert(id.to_owned(), Vec::new());
        let admission = async {
            let steer_options = SteerActiveTurnOptions {
                run: options.run.clone(),
                clear_pending_permissions: options.clear_pending_permissions,
                expected_turn_id: expected.to_owned(),
            };
            let Some(steer) = session.steer_active_turn(prompt, &steer_options) else {
                return Ok(SteerResult::Unavailable);
            };
            let admission = steer.await?;
            if admission == SteerResult::Accepted {
                self.record_accepted_steer(
                    id,
                    prompt,
                    options.run.client_message_id.as_deref(),
                    expected,
                )?;
            }
            Ok(admission)
        }
        .await;
        let held = self
            .lock()
            .steer_event_barriers
            .remove(id)
            .unwrap_or_default();
        for event in held {
            self.enqueue_session_event(id, event);
        }
        self.drain_session_events_async(id).await;
        admission
    }

    /// `steerAgentRun(agentId, prompt, options)`.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, `Active turn changed before
    /// steering could be delivered`, or the provider's error.
    pub async fn steer_agent_run(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentSteerOptions>,
    ) -> Result<SteerResult, AgentError> {
        let SteerTarget {
            id,
            session,
            expected,
        } = self.steer_target(agent_id)?;
        let Some(expected) = expected.filter(|_| session.supports_steer_active_turn()) else {
            return Ok(SteerResult::Unavailable);
        };
        let options = options.unwrap_or_default();
        let result = self
            .run_steer_admission(&id, &session, &expected, &prompt, &options)
            .await?;
        // An unavailable answer is only safe to fall back from while this
        // admission still owns the active turn.
        if result == SteerResult::Unavailable {
            self.assert_steer_admission_owns_turn(&id, &expected)?;
        }
        Ok(result)
    }

    /// `steerOrReplaceActiveTurn(agentId, prompt, options)`.
    ///
    /// # Errors
    ///
    /// As [`Self::steer_agent_run`], plus a replacement's own errors.
    pub async fn steer_or_replace_active_turn(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentSteerOptions>,
    ) -> Result<SteerDispatch, AgentError> {
        let SteerTarget {
            id,
            session,
            expected,
        } = self.steer_target(agent_id)?;
        let Some(expected) = expected else {
            return Ok(SteerDispatch::Inactive);
        };
        let steer_options = options.clone().unwrap_or_default();
        let result = if session.supports_steer_active_turn() {
            self.run_steer_admission(&id, &session, &expected, &prompt, &steer_options)
                .await?
        } else {
            SteerResult::Unavailable
        };
        if result == SteerResult::Accepted {
            return Ok(SteerDispatch::Steered);
        }
        // Providers without autonomous steering keep their existing dispatch
        // behavior: only an accepted steer can own the turn without a
        // replacement.
        let untracked = self.lock().agent(&id).is_some_and(|agent| {
            agent.snapshot.active_foreground_turn_id.is_none()
                && agent.snapshot.active_turn_id.as_deref() == Some(expected.as_str())
        });
        if untracked {
            return Ok(SteerDispatch::Inactive);
        }
        if let Some(hook) = &self.inner.before_steer_unavailable_fallback {
            hook(id.clone(), expected.clone()).await;
        }
        self.assert_steer_admission_owns_turn(&id, &expected)?;
        // `replaceAdmittedForegroundTurn`
        self.assert_steer_admission_owns_turn(&id, &expected)?;
        {
            let mut state = self.lock();
            self.mark_replacing_locked(&mut state, &id);
        }
        let stream = self
            .cancel_and_stream_replacement(&id, prompt, options.map(|options| options.run))
            .await?;
        Ok(SteerDispatch::Replaced(Box::new(stream)))
    }

    /// `rewind(agentId, messageId, mode)`: reverts the provider session to
    /// before `message_id`, holding the agent's foreground run slot while it
    /// does, then rebuilds the timeline from the provider's history unless
    /// only files were reverted.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, `Cannot rewind before the
    /// provider acknowledges the submitted prompt`, `AgentRunCancellationError`
    /// when an active run cannot be cancelled, `RewindCapabilityError`, or
    /// the provider's, history or persistence error.
    pub async fn rewind(
        &self,
        agent_id: &str,
        message_id: &str,
        mode: RewindMode,
    ) -> Result<(), AgentError> {
        let (id, session, provider, provider_message_id) = {
            let state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            let id = agent.snapshot.id.clone();
            let Some(session) = agent.session.clone() else {
                return Err(AgentError::new(format!(
                    "Agent '{id}' has no managed session"
                )));
            };
            let submitted = state.timeline.rows(&id).ok().and_then(|rows| {
                rows.iter()
                    .find(|row| {
                        row.item.get("type").and_then(JsValue::as_str) == Some("user_message")
                            && row.item.get("messageId").and_then(JsValue::as_str)
                                == Some(message_id)
                            && row.item.get("clientMessageId").and_then(JsValue::as_str)
                                == Some(message_id)
                    })
                    .map(|row| row.provider_message_id.clone())
            });
            let acknowledged = match submitted {
                Some(provider_message_id) => {
                    match provider_message_id.filter(|provider_id| !provider_id.is_empty()) {
                        Some(provider_id) => provider_id,
                        None => {
                            return Err(AgentError::new(
                                "Cannot rewind before the provider acknowledges the submitted prompt",
                            ));
                        }
                    }
                }
                None => message_id.to_owned(),
            };
            (id, session, agent.snapshot.provider.clone(), acknowledged)
        };

        if self.has_in_flight_run(agent_id) {
            self.cancel_agent_run_before(agent_id, "rewind").await?;
        }

        let token = {
            let mut state = self.lock();
            let token = Self::next_token(&mut state);
            state.runs.insert(
                id.clone(),
                TrackedRun::Foreground {
                    token,
                    start: RunStart::Pending,
                    staged: Vec::new(),
                    settled: watch::channel(false).0,
                },
            );
            token
        };
        let attempt: Result<(), AgentError> = async {
            self.log_rewind("agent.rewind.start", &id, &provider, message_id, mode);
            invoke_rewind_capability(&*session, &provider_message_id, mode).await?;
            if mode != RewindMode::Files {
                self.hydrate_timeline_from_provider(
                    &id,
                    HydrateTimelineOptions {
                        force: true,
                        broadcast: Some(HydrateBroadcast::Now(true)),
                        broadcast_timeline: Some(false),
                    },
                )
                .await?;
                let state = self.lock();
                let epoch = state
                    .timeline
                    .epoch(&id)
                    .map_err(|error| AgentError::named("TypeError".to_owned(), error.to_string()))?
                    .to_owned();
                self.dispatch(
                    &state,
                    AgentManagerEvent::TimelineReplacement {
                        agent_id: id.clone(),
                        epoch,
                    },
                );
            }
            self.refresh_session_persistence(&id);
            self.refresh_session_state(&id, false).await;
            self.persist_snapshot(&id, SnapshotOverrides::default())
                .await?;
            self.emit_state(&id, false);
            self.log_rewind("agent.rewind.complete", &id, &provider, message_id, mode);
            Ok(())
        }
        .await;
        if let Err(error) = &attempt {
            self.warn_rewind_failed(error, &id, &provider, message_id, mode);
        }
        Self::settle_foreground_run(&mut self.lock(), &id, token);
        attempt
    }

    /// `logger.info({ agentId, provider, messageId, mode }, message)`.
    fn log_rewind(
        &self,
        message: &str,
        agent_id: &str,
        provider: &str,
        message_id: &str,
        mode: RewindMode,
    ) {
        let Some(info) = &self.inner.log_info else {
            return;
        };
        info(
            Self::rewind_bindings(None, agent_id, provider, message_id, mode),
            message,
        );
    }

    /// The bindings `rewind` logs: `err` first when there is one.
    fn rewind_bindings(
        err: Option<JsValue>,
        agent_id: &str,
        provider: &str,
        message_id: &str,
        mode: RewindMode,
    ) -> JsValue {
        let mut bindings = JsObject::new();
        if let Some(err) = err {
            bindings.insert("err", err);
        }
        bindings.insert("agentId", JsValue::String(agent_id.to_owned()));
        bindings.insert("provider", JsValue::String(provider.to_owned()));
        bindings.insert("messageId", JsValue::String(message_id.to_owned()));
        bindings.insert("mode", JsValue::String(mode.as_str().to_owned()));
        JsValue::Object(bindings)
    }

    /// `logger.warn({ err, agentId, provider, messageId, mode },
    /// "agent.rewind.failed")`.
    fn warn_rewind_failed(
        &self,
        error: &AgentError,
        agent_id: &str,
        provider: &str,
        message_id: &str,
        mode: RewindMode,
    ) {
        let Some(warn) = &self.inner.log_warn else {
            return;
        };
        warn(
            Self::rewind_bindings(
                Some(err_binding(error)),
                agent_id,
                provider,
                message_id,
                mode,
            ),
            "agent.rewind.failed",
        );
    }

    /// `cancelAgentRunBefore(agentId, action)`: cancels the active run, and
    /// fails with `AgentRunCancellationError` when the cancellation is
    /// refused.
    pub(super) async fn cancel_agent_run_before(
        &self,
        agent_id: &str,
        action: &str,
    ) -> Result<(), AgentError> {
        if self.cancel_agent_run(agent_id).await? == AgentRunCancellationResult::Refused {
            return Err(AgentError::named(
                "AgentRunCancellationError".to_owned(),
                format!(
                    "Cannot {action} agent {agent_id} because its active run cancellation was not acknowledged"
                ),
            ));
        }
        Ok(())
    }

    fn pending_run_settled(&self, agent_id: &str, token: u64) -> bool {
        !matches!(self.lock().runs.get(agent_id), Some(TrackedRun::Foreground { token: current, .. }) if *current == token)
    }

    /// `startPendingForegroundTurn`.
    async fn start_pending_foreground_turn(
        &self,
        agent_id: &str,
        token: u64,
        session: &Arc<dyn AgentSession>,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> Result<String, AgentError> {
        let result = session
            .start_turn(prompt, options)
            .await
            .and_then(|turn_id| {
                if self.pending_run_settled(agent_id, token) {
                    Err(AgentError::new(format!(
                        "Agent {agent_id} run was canceled before its turn started"
                    )))
                } else {
                    Ok(turn_id)
                }
            });
        let error = match result {
            Ok(turn_id) => return Ok(turn_id),
            Err(error) => error,
        };
        if self.pending_run_settled(agent_id, token) {
            return Err(error);
        }
        let mut state = self.lock();
        if let Some(TrackedRun::Foreground { start, .. }) = state.runs.get_mut(agent_id) {
            *start = RunStart::Failed(error.message.clone());
        }
        if error.is_stale_provider_session() {
            if let Some(agent) = state.agent_mut(agent_id) {
                agent.snapshot.pending_replacement = false;
                if agent.snapshot.active_foreground_turn_id.is_none() {
                    agent.snapshot.lifecycle = AgentLifecycle::Idle;
                }
            }
            Self::settle_foreground_run(&mut state, agent_id, token);
            return Err(error);
        }
        let provider = match state.agent_mut(agent_id) {
            Some(agent) => {
                agent.snapshot.pending_replacement = false;
                agent.snapshot.provider.clone()
            }
            None => return Err(error),
        };
        let mut failed = JsObject::new();
        failed.insert("type", JsValue::String("turn_failed".to_owned()));
        failed.insert("provider", JsValue::String(provider));
        failed.insert("error", JsValue::String(error.message.clone()));
        let _ =
            self.handle_stream_event_locked(&mut state, agent_id, &JsValue::Object(failed), false);
        self.finalize_foreground_turn_locked(&mut state, agent_id, None);
        Self::settle_foreground_run(&mut state, agent_id, token);
        Err(error)
    }

    /// `logger.trace(..., "agent.manager.stream.start")`.
    fn trace_stream_start(&self, state: &State, agent_id: &str, turn_id: &str) {
        self.emit_trace(
            || {
                let snapshot = state.agent(agent_id).map(|agent| &agent.snapshot);
                trace::bindings([
                    ("agentId", trace::text(agent_id)),
                    (
                        "provider",
                        snapshot.map_or(JsValue::Undefined, |agent| trace::text(&agent.provider)),
                    ),
                    (
                        "sessionId",
                        snapshot.map_or(JsValue::Undefined, trace::session_id),
                    ),
                    ("turnId", trace::text(turn_id)),
                    (
                        "lifecycle",
                        snapshot.map_or(JsValue::Undefined, trace::lifecycle),
                    ),
                    (
                        "activeForegroundTurnId",
                        snapshot.map_or(JsValue::Undefined, trace::foreground_turn_id),
                    ),
                ])
            },
            "agent.manager.stream.start",
        );
    }

    /// The accepted-turn stretch of `streamAgent` after `startTurn`
    /// resolved, up to registering the turn stream's waiter.
    fn accept_foreground_turn(
        &self,
        agent_id: &str,
        token: u64,
        turn_id: &str,
        prompt: &AgentPromptInput,
        options: Option<&AgentRunOptions>,
        is_replacement: bool,
    ) -> Result<(u64, mpsc::UnboundedReceiver<JsValue>), AgentError> {
        let mut state = self.lock();
        let now = crate::clock::now_millis();
        let staged = match state.runs.get_mut(agent_id) {
            Some(TrackedRun::Foreground { start, staged, .. }) => {
                *start = RunStart::Started(turn_id.to_owned());
                std::mem::take(staged)
            }
            _ => Vec::new(),
        };
        let provider = {
            let agent = state
                .agent_mut(agent_id)
                .ok_or_else(|| AgentError::new(format!("Unknown agent '{agent_id}'")))?;
            if is_replacement {
                agent.snapshot.pending_replacement = false;
            }
            agent.snapshot.active_foreground_turn_id = Some(turn_id.to_owned());
            agent.snapshot.active_turn_id = Some(turn_id.to_owned());
            agent.snapshot.active_turn_started_at_millis = Some(now);
            agent.snapshot.lifecycle = AgentLifecycle::Running;
            touch_updated_at(&mut agent.snapshot);
            agent.snapshot.provider.clone()
        };
        let mut started = JsObject::new();
        started.insert("type", JsValue::String("turn_started".to_owned()));
        started.insert("provider", JsValue::String(provider));
        started.insert("turnId", JsValue::String(turn_id.to_owned()));
        self.dispatch_stream_locked(
            &state,
            agent_id,
            &JsValue::Object(started),
            None,
            None,
            Some(crate::clock::iso_from_millis(now)),
        )?;
        let client_message_id = options.and_then(|options| options.client_message_id.clone());
        let (echo_message_id, replay) =
            split_staged_events(staged, turn_id, client_message_id.as_deref());
        if let Some(client_message_id) = &client_message_id {
            self.record_submitted_prompt_locked(
                &mut state,
                agent_id,
                SubmittedPrompt {
                    text: submitted_prompt_text(prompt),
                    client_message_id: client_message_id.clone(),
                    message_id: Some(client_message_id.clone()),
                    provider_message_id: echo_message_id,
                    turn_id: Some(JsValue::String(turn_id.to_owned())),
                },
            )?;
        }
        for event in &replay {
            self.trace_event_locked(
                &state,
                agent_id,
                event,
                super::events::raw_turn_id(event).unwrap_or(JsValue::Undefined),
                "agent.manager.enqueue",
            );
        }
        self.emit_state_locked(&mut state, agent_id, true);
        self.trace_stream_start(&state, agent_id, turn_id);
        let (tx, rx) = mpsc::unbounded_channel();
        let waiter_id = Self::next_token(&mut state);
        if let Some(agent) = state.agent_mut(agent_id) {
            agent.foreground_turn_waiters.push(ForegroundTurnWaiter {
                id: waiter_id,
                turn_id: turn_id.to_owned(),
                tx: Some(tx),
            });
        }
        // `enqueueSessionEvent` for the staged events runs before the waiter
        // exists in the baseline, but their handling waits for this lock, so
        // the waiter still sees them as it does there.
        for event in replay {
            let queue = state.session_queues.entry(agent_id.to_owned()).or_default();
            queue.events.push_back(event);
            if !queue.draining {
                queue.draining = true;
                let manager = self.clone();
                let id = agent_id.to_owned();
                self.track_background_task(async move { manager.drain_session_events(&id) });
            }
        }
        let _ = token;
        Ok((waiter_id, rx))
    }

    /// The `finally` of `streamAgent`'s generator.
    async fn finish_turn_stream(&self, agent_id: &str, token: u64, waiter_id: Option<u64>) {
        let refresh = {
            let mut state = self.lock();
            if let Some(waiter_id) = waiter_id
                && let Some(agent) = state.agent_mut(agent_id)
            {
                agent
                    .foreground_turn_waiters
                    .retain(|waiter| waiter.id != waiter_id);
            }
            Self::settle_foreground_run(&mut state, agent_id, token);
            state
                .agent(agent_id)
                .is_some_and(|agent| agent.snapshot.active_foreground_turn_id.is_none())
        };
        if refresh {
            self.refresh_runtime_info(agent_id, true).await;
        }
    }

    /// `runAgent(agentId, prompt, options)`.
    ///
    /// # Errors
    ///
    /// The stream's errors, the formatted `turn_failed` message, or the
    /// missing session id after the run.
    pub async fn run_agent(
        &self,
        agent_id: &str,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> Result<AgentRunResult, AgentError> {
        let mut events = self.stream_agent(agent_id, prompt, options)?;
        let mut timeline = Vec::new();
        let mut usage = None;
        let mut canceled = false;
        while let Some(event) = events.next().await {
            let event = event?;
            match event_type(&event) {
                Some("timeline") => {
                    timeline.push(event.get("item").cloned().unwrap_or(JsValue::Undefined));
                }
                Some("turn_completed") => usage = event.get("usage").cloned(),
                Some("turn_failed") => {
                    events.close().await;
                    return Err(AgentError::new(format_turn_failed_message(&event)));
                }
                Some("turn_canceled") => canceled = true,
                _ => {}
            }
        }
        let final_text = last_assistant_message_from_items(&timeline).unwrap_or_default();
        let state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        let session_id = agent
            .snapshot
            .persistence
            .as_ref()
            .and_then(|handle| handle.get("sessionId"))
            .filter(|id| truthy(Some(id)))
            .map(|id| js_string(Some(id)))
            .ok_or_else(|| {
                AgentError::new(format!(
                    "Agent {agent_id} has no persistence.sessionId after run completed"
                ))
            })?;
        Ok(AgentRunResult {
            session_id,
            final_text,
            usage: usage.filter(|usage| !matches!(usage, JsValue::Undefined)),
            timeline,
            canceled,
        })
    }

    /// `getLastAssistantMessage(agentId)` without a durable store.
    #[must_use]
    pub fn get_last_assistant_message(&self, agent_id: &str) -> Option<String> {
        let state = self.lock();
        state.agent(agent_id)?;
        let items: Vec<JsValue> = state
            .timeline
            .rows(agent_id)
            .ok()?
            .iter()
            .map(|row| row.item.clone())
            .collect();
        last_assistant_message_from_items(&items)
    }

    /// `peekPendingPermission(agent)`: the first pending request.
    fn peek_pending_permission(snapshot: &super::ManagedAgentSnapshot) -> Option<JsValue> {
        snapshot
            .pending_permissions
            .first()
            .map(|(_, request)| request.clone())
    }

    /// `waitForAgentEvent(agentId, options)`.
    ///
    /// # Errors
    ///
    /// `Agent <id> not found` for an unknown agent, and an `AbortError` when
    /// `options.signal` aborts before the wait settles.
    pub async fn wait_for_agent_event(
        &self,
        agent_id: &str,
        options: WaitForAgentOptions,
    ) -> Result<WaitForAgentResult, AgentError> {
        let (snapshot, pending_run) = {
            let state = self.lock();
            let Some(agent) = state.agent(agent_id) else {
                return Err(AgentError::new(format!("Agent {agent_id} not found")));
            };
            let pending = match state.runs.get(agent_id) {
                Some(TrackedRun::Foreground { start, .. }) => Some(start.clone()),
                _ => None,
            };
            (agent.snapshot.clone(), pending)
        };
        let has_foreground_turn =
            snapshot.active_foreground_turn_id.is_some() || pending_run.is_some();
        if let Some(permission) = Self::peek_pending_permission(&snapshot) {
            return Ok(WaitForAgentResult {
                status: snapshot.lifecycle,
                permission: Some(permission),
                last_message: self.get_last_assistant_message(agent_id),
            });
        }
        let initial_busy = snapshot.lifecycle.is_busy() || has_foreground_turn;
        if !initial_busy {
            return Ok(WaitForAgentResult {
                status: snapshot.lifecycle,
                permission: None,
                last_message: self.get_last_assistant_message(agent_id),
            });
        }
        if let Some(signal) = options.signal.as_ref().filter(|signal| signal.aborted()) {
            return Err(abort_error(signal, WAIT_ABORTED));
        }
        let has_started = snapshot.lifecycle.is_busy()
            || snapshot.active_foreground_turn_id.is_some()
            || matches!(pending_run, Some(RunStart::Started(_)));
        let watch = Arc::new(Mutex::new(WaitWatch {
            status: snapshot.lifecycle,
            has_started,
            terminal_override: None,
            done: None,
        }));
        let (done_tx, done_rx) = oneshot::channel::<Option<JsValue>>();
        watch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .done = Some(done_tx);
        let callback_watch = Arc::clone(&watch);
        let wait_for_active = options.wait_for_active;
        let unsubscribe = Unsubscribing(Some(self.subscribe(
            Arc::new(move |event: &AgentManagerEvent| {
                let mut watch = callback_watch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                watch.observe(event, wait_for_active);
            }),
            SubscribeOptions {
                agent_id: Some(agent_id.to_owned()),
                replay_state: Some(true),
            },
        )?));
        let permission = match &options.signal {
            Some(signal) => tokio::select! {
                biased;
                permission = done_rx => permission.unwrap_or(None),
                () = signal.wait() => return Err(abort_error(signal, WAIT_ABORTED)),
            },
            None => done_rx.await.unwrap_or(None),
        };
        drop(unsubscribe);
        let status = watch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status;
        Ok(WaitForAgentResult {
            status,
            permission,
            last_message: self.get_last_assistant_message(agent_id),
        })
    }
}

impl AgentManager {
    /// `tryRunOutOfBand(agentId, prompt, options)`: whether the session took
    /// the prompt as a side-effect command (such as `/goal pause`) that runs
    /// without a foreground turn. Its events persist and broadcast like
    /// timeline events; a failure becomes an `[Error]` assistant message.
    ///
    /// # Errors
    ///
    /// `requireSessionAgent`'s errors for an unknown agent or one without a
    /// session.
    pub fn try_run_out_of_band(
        &self,
        agent_id: &str,
        prompt: &AgentPromptInput,
        options: Option<&AgentRunOptions>,
    ) -> Result<bool, AgentError> {
        let (id, provider, session) = {
            let state = self.lock();
            let agent = Self::require_agent(&state, agent_id)?;
            let id = agent.snapshot.id.clone();
            let Some(session) = agent.session.clone() else {
                return Err(AgentError::new(format!(
                    "Agent '{id}' has no managed session"
                )));
            };
            (id, agent.snapshot.provider.clone(), session)
        };
        let Some(Some(handler)) = session.try_handle_out_of_band(prompt) else {
            return Ok(false);
        };
        if let Some(client_message_id) = options
            .and_then(|options| options.client_message_id.clone())
            .filter(|id| !id.is_empty())
        {
            let mut state = self.lock();
            self.record_submitted_prompt_locked(
                &mut state,
                &id,
                SubmittedPrompt {
                    text: submitted_prompt_text(prompt),
                    client_message_id,
                    message_id: None,
                    provider_message_id: None,
                    turn_id: None,
                },
            )?;
            self.emit_state_locked(&mut state, &id, true);
        }
        let manager = self.clone();
        let emit_id = id.clone();
        let dispatch: StreamCallback = Arc::new(move |event: JsValue| {
            manager.dispatch_out_of_band(&emit_id, &event);
        });
        let manager = self.clone();
        tokio::spawn(async move {
            if let Err(error) = handler.run(Arc::clone(&dispatch)).await {
                let mut item = JsObject::new();
                item.insert("type", JsValue::String("assistant_message".to_owned()));
                item.insert(
                    "text",
                    JsValue::String(format!("[Error] {}", error.message)),
                );
                let mut event = JsObject::new();
                event.insert("type", JsValue::String("timeline".to_owned()));
                event.insert("provider", JsValue::String(provider));
                event.insert("item", JsValue::Object(item));
                manager.dispatch_out_of_band(&id, &JsValue::Object(event));
            }
        });
        Ok(true)
    }

    /// The `dispatch` of `tryRunOutOfBand`: timeline items are recorded and
    /// broadcast with their row; other events are broadcast only.
    fn dispatch_out_of_band(&self, agent_id: &str, event: &JsValue) {
        let mut state = self.lock();
        if event_type(event) == Some("timeline") {
            if let Some(agent) = state.agent_mut(agent_id) {
                touch_updated_at(&mut agent.snapshot);
            }
            let item = event.get("item").cloned().unwrap_or(JsValue::Undefined);
            let Ok(row) =
                Self::record_timeline_locked(&mut state, agent_id, item, None, None, None)
            else {
                return;
            };
            let epoch = state.timeline.epoch(agent_id).ok().map(str::to_owned);
            let _ = self.dispatch_stream_locked(
                &state,
                agent_id,
                event,
                Some(row.seq),
                epoch,
                Some(row.timestamp),
            );
            return;
        }
        let _ = self.dispatch_stream_locked(
            &state,
            agent_id,
            event,
            None,
            None,
            Some(crate::clock::now_iso()),
        );
    }
}

/// The fallback message of `waitForAgentRunStart`'s abort error.
const WAIT_START_ABORTED: &str = "wait_for_agent_start aborted";

/// One `waitForAgentRunStart` subscription.
pub(crate) struct RunStartWaiter {
    id: u64,
    agent_id: String,
    done: oneshot::Sender<Result<(), AgentError>>,
}

/// The foreground run's start (`runs.getPendingRun(agentId)?.start`).
fn pending_start<'a>(state: &'a State, agent_id: &str) -> Option<&'a RunStart> {
    match state.runs.get(agent_id) {
        Some(TrackedRun::Foreground { start, .. }) => Some(start),
        _ => None,
    }
}

/// `checkCurrentState()` of `waitForAgentRunStart`: the outcome once
/// decided, `None` while the run has yet to start.
fn run_start_outcome(state: &State, agent_id: &str) -> Option<Result<(), AgentError>> {
    let Some(agent) = state.agent(agent_id) else {
        return Some(Err(AgentError::new(format!("Agent {agent_id} not found"))));
    };
    let snapshot = &agent.snapshot;
    let pending = pending_start(state, agent_id);
    if (snapshot.lifecycle == AgentLifecycle::Running
        || matches!(pending, Some(RunStart::Started(_))))
        && !snapshot.pending_replacement
    {
        return Some(Ok(()));
    }
    if let Some(RunStart::Failed(error)) = pending {
        return Some(Err(AgentError::new(error.clone())));
    }
    if snapshot.lifecycle == AgentLifecycle::Error && pending.is_none() {
        return Some(Err(AgentError::new(
            snapshot
                .last_error
                .clone()
                .unwrap_or_else(|| format!("Agent {agent_id} failed to start")),
        )));
    }
    if pending.is_none()
        && snapshot.active_foreground_turn_id.is_none()
        && !snapshot.pending_replacement
    {
        return Some(Err(AgentError::new(format!(
            "Agent {agent_id} run finished before starting"
        ))));
    }
    None
}

/// Removes its waiter when the wait ends, however it ends.
struct RunStartGuard<'a> {
    manager: &'a AgentManager,
    id: u64,
}

impl Drop for RunStartGuard<'_> {
    fn drop(&mut self) {
        self.manager
            .inner
            .run_start_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|waiter| waiter.id != self.id);
    }
}

impl AgentManager {
    /// Settles the run-start waiters of `agent_id` as an `agent_state` for
    /// it is dispatched, against the state at that moment.
    pub(crate) fn settle_run_start_waiters(&self, state: &State, agent_id: &str) {
        let mut waiters = self
            .inner
            .run_start_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !waiters.iter().any(|waiter| waiter.agent_id == agent_id) {
            return;
        }
        let Some(outcome) = run_start_outcome(state, agent_id) else {
            return;
        };
        let mut index = 0;
        while index < waiters.len() {
            if waiters[index].agent_id == agent_id {
                let waiter = waiters.remove(index);
                let _ = waiter.done.send(outcome.clone());
            } else {
                index += 1;
            }
        }
    }

    /// `waitForAgentRunStart(agentId, { signal })`: resolves once the
    /// agent's pending foreground run has started.
    ///
    /// # Errors
    ///
    /// The baseline's errors: an unknown agent, no pending run, a run that
    /// failed or finished before starting, an agent in error, a malformed
    /// agent id at subscription, and an `AbortError` when `signal` aborts.
    pub async fn wait_for_agent_run_start(
        &self,
        agent_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<(), AgentError> {
        let (id, done) = {
            let state = self.lock();
            let Some(agent) = state.agent(agent_id) else {
                return Err(AgentError::new(format!("Agent {agent_id} not found")));
            };
            let snapshot = &agent.snapshot;
            let pending = pending_start(&state, agent_id);
            if (snapshot.lifecycle == AgentLifecycle::Running
                || matches!(pending, Some(RunStart::Started(_))))
                && !snapshot.pending_replacement
            {
                return Ok(());
            }
            if snapshot.active_foreground_turn_id.is_none()
                && pending.is_none()
                && !snapshot.pending_replacement
            {
                return Err(AgentError::new(format!(
                    "Agent {agent_id} has no pending run"
                )));
            }
            if let Some(signal) = signal.as_ref().filter(|signal| signal.aborted()) {
                return Err(abort_error(signal, WAIT_START_ABORTED));
            }
            super::validate_agent_id(agent_id, "subscribe")?;
            if let Some(outcome) = run_start_outcome(&state, agent_id) {
                return outcome;
            }
            let id = {
                let mut state = state;
                state.next_token += 1;
                state.next_token
            };
            let (done_tx, done) = oneshot::channel();
            self.inner
                .run_start_waiters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(RunStartWaiter {
                    id,
                    agent_id: agent_id.to_owned(),
                    done: done_tx,
                });
            (id, done)
        };
        let _guard = RunStartGuard { manager: self, id };
        let outcome = match &signal {
            Some(signal) => tokio::select! {
                biased;
                outcome = done => outcome,
                () = signal.wait() => return Err(abort_error(signal, WAIT_START_ABORTED)),
            },
            None => done.await,
        };
        outcome.unwrap_or_else(|_| Err(AgentError::new(format!("Agent {agent_id} not found"))))
    }
}

/// The subscriber state of one `waitForAgentEvent`.
struct WaitWatch {
    status: AgentLifecycle,
    has_started: bool,
    terminal_override: Option<AgentLifecycle>,
    done: Option<oneshot::Sender<Option<JsValue>>>,
}

impl WaitWatch {
    fn finish(&mut self, permission: Option<JsValue>) {
        if let Some(done) = self.done.take() {
            let _ = done.send(permission);
        }
    }

    fn observe(&mut self, event: &AgentManagerEvent, wait_for_active: bool) {
        if self.done.is_none() {
            return;
        }
        match event {
            AgentManagerEvent::AgentState(agent) => {
                self.status = agent.lifecycle;
                if let Some(permission) = AgentManager::peek_pending_permission(agent) {
                    self.finish(Some(permission));
                    return;
                }
                if agent.lifecycle.is_busy() {
                    self.has_started = true;
                    return;
                }
                if !wait_for_active || self.has_started {
                    if let Some(status) = self.terminal_override {
                        self.status = status;
                    }
                    self.finish(None);
                }
            }
            AgentManagerEvent::AgentStream { event, .. } => match event_type(event) {
                Some("permission_requested") => self.finish(event.get("request").cloned()),
                Some("turn_failed") => {
                    self.has_started = true;
                    self.terminal_override = Some(AgentLifecycle::Error);
                }
                Some("turn_completed" | "turn_canceled") => self.has_started = true,
                _ => {}
            },
            AgentManagerEvent::TimelineReplacement { .. }
            | AgentManagerEvent::ProviderSubagent(_) => {}
        }
    }
}

/// Splits the events staged while the turn was starting: the provider's
/// echo of the submitted prompt (only its `messageId` is kept) and the
/// accepted `turn_started` are dropped; the rest replay in order.
fn split_staged_events(
    staged: Vec<JsValue>,
    turn_id: &str,
    client_message_id: Option<&str>,
) -> (Option<String>, Vec<JsValue>) {
    let echo = client_message_id.and_then(|client_message_id| {
        staged.iter().position(|event| {
            let item = event.get("item");
            event_type(event) == Some("timeline")
                && item
                    .and_then(|item| item.get("type"))
                    .and_then(JsValue::as_str)
                    == Some("user_message")
                && item
                    .and_then(|item| item.get("clientMessageId"))
                    .and_then(JsValue::as_str)
                    == Some(client_message_id)
        })
    });
    let echo_message_id = echo
        .and_then(|index| staged[index].get("item"))
        .and_then(|item| item.get("messageId"))
        .filter(|id| truthy(Some(id)))
        .map(|id| js_string(Some(id)));
    let replay = staged
        .into_iter()
        .enumerate()
        .filter(|(index, event)| {
            let accepted_start = event_type(event) == Some("turn_started")
                && raw_turn_id(event).as_ref().and_then(JsValue::as_str) == Some(turn_id);
            !accepted_start && Some(*index) != echo
        })
        .map(|(_, event)| event)
        .collect();
    (echo_message_id, replay)
}

/// `getLastAssistantMessageFromTimeline`: the last run of assistant chunks.
fn last_assistant_message_from_items(items: &[JsValue]) -> Option<String> {
    let mut chunks: Vec<String> = Vec::new();
    for item in items.iter().rev() {
        if item.get("type").and_then(JsValue::as_str) != Some("assistant_message") {
            if chunks.is_empty() {
                continue;
            }
            break;
        }
        chunks.push(js_string(item.get("text")));
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.reverse();
    Some(chunks.concat())
}

enum Phase {
    Start {
        session: Arc<dyn AgentSession>,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
        is_replacement: bool,
    },
    Streaming {
        rx: mpsc::UnboundedReceiver<JsValue>,
    },
    /// The terminal event was yielded; the next call runs the `finally`.
    Terminal,
    Done,
}

/// The `AsyncGenerator<AgentStreamEvent>` `streamAgent` returns.
pub struct TurnEventStream {
    manager: AgentManager,
    agent_id: String,
    phase: Phase,
    token: u64,
    waiter_id: Option<u64>,
}

impl TurnEventStream {
    /// `next()`: `None` once the generator is done.
    pub async fn next(&mut self) -> Option<Result<JsValue, AgentError>> {
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::Start {
                session,
                prompt,
                options,
                is_replacement,
            } => {
                let turn_id = match self
                    .manager
                    .start_pending_foreground_turn(
                        &self.agent_id,
                        self.token,
                        &session,
                        prompt.clone(),
                        options.clone(),
                    )
                    .await
                {
                    Ok(turn_id) => turn_id,
                    Err(error) => return Some(Err(error)),
                };
                let (waiter_id, rx) = match self.manager.accept_foreground_turn(
                    &self.agent_id,
                    self.token,
                    &turn_id,
                    &prompt,
                    options.as_ref(),
                    is_replacement,
                ) {
                    Ok(accepted) => accepted,
                    Err(error) => return Some(Err(error)),
                };
                self.waiter_id = Some(waiter_id);
                self.phase = Phase::Streaming { rx };
                let mut started = JsObject::new();
                started.insert("type", JsValue::String("turn_started".to_owned()));
                started.insert("provider", JsValue::String(session.provider()));
                started.insert("turnId", JsValue::String(turn_id));
                Some(Ok(JsValue::Object(started)))
            }
            Phase::Streaming { mut rx } => {
                if let Some(event) = rx.recv().await {
                    self.phase = if is_turn_terminal_event(&event) {
                        Phase::Terminal
                    } else {
                        Phase::Streaming { rx }
                    };
                    return Some(Ok(event));
                }
                self.close().await;
                None
            }
            Phase::Terminal => {
                self.close().await;
                None
            }
            Phase::Done => None,
        }
    }

    /// `return()`: runs the generator's `finally` if it has not run.
    pub async fn close(&mut self) {
        let started = !matches!(self.phase, Phase::Start { .. });
        self.phase = Phase::Done;
        if started && let Some(waiter_id) = self.waiter_id.take() {
            self.manager
                .finish_turn_stream(&self.agent_id, self.token, Some(waiter_id))
                .await;
        }
    }
}

impl Drop for TurnEventStream {
    fn drop(&mut self) {
        if let Some(waiter_id) = self.waiter_id.take() {
            let manager = self.manager.clone();
            let agent_id = std::mem::take(&mut self.agent_id);
            let token = self.token;
            tokio::spawn(async move {
                manager
                    .finish_turn_stream(&agent_id, token, Some(waiter_id))
                    .await;
            });
        }
    }
}
