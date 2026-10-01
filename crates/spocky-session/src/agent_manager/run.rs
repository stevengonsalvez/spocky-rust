//! Foreground runs from pinned Paseo `agent/agent-run-state.ts` and
//! `agent/agent-manager.ts`: `streamAgent`, `runAgent`, turn finalization,
//! and `waitForAgentEvent`.

use std::sync::{Arc, Mutex};

use spocky_store::js_value::{JsObject, JsValue};
use tokio::sync::{mpsc, oneshot};

use super::create::{attach_persistence_cwd, touch_updated_at};
use super::events::{
    SubmittedPrompt, TerminalDisposition, event_type, format_turn_failed_message,
    is_turn_terminal_event, raw_turn_id,
};
use super::{AgentLifecycle, AgentManager, AgentManagerEvent, State, SubscribeOptions};
use crate::agent_prompt::submitted_prompt_text;
use crate::agent_sdk::{AbortSignal, AgentError, AgentPromptInput, AgentRunOptions, AgentSession};
use crate::js::{js_string, truthy};

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

/// `TrackedAgentRun`.
pub(crate) enum TrackedRun {
    Foreground {
        token: u64,
        start: RunStart,
        staged: Vec<JsValue>,
    },
    Autonomous {
        turn_id: Option<String>,
    },
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

/// `WaitForAgentOptions`.
#[derive(Debug, Clone, Default)]
pub struct WaitForAgentOptions {
    pub signal: Option<AbortSignal>,
    pub wait_for_active: bool,
}

/// The fallback message of `waitForAgentEvent`'s abort error.
const WAIT_ABORTED: &str = "wait_for_agent aborted";

/// `createAbortError(signal, fallbackMessage)`: an `AbortError` whose
/// message is a string reason, else the fallback. `abortMessage` also takes
/// an `Error` reason's message, which [`AbortSignal`] cannot carry: its
/// reasons are [`JsValue`]s.
fn abort_error(signal: &AbortSignal, fallback: &str) -> AgentError {
    let message = match signal.reason() {
        Some(JsValue::String(reason)) => reason.clone(),
        _ => fallback.to_owned(),
    };
    AgentError {
        name: "AbortError".to_owned(),
        message,
    }
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
            Some(TrackedRun::Autonomous { turn_id: run_turn }) => {
                run_turn.as_ref().is_some_and(|run_turn| {
                    turn_id.is_some()
                        && turn_id.and_then(JsValue::as_str) != Some(run_turn.as_str())
                })
            }
        };
        if !keep {
            state.runs.remove(agent_id);
        }
    }

    /// `runs.trackAutonomousRun(agentId, turnId)`.
    pub(crate) fn track_autonomous_run(state: &mut State, agent_id: &str, turn_id: Option<String>) {
        state
            .runs
            .entry(agent_id.to_owned())
            .or_insert(TrackedRun::Autonomous { turn_id });
    }

    /// `runs.settleForegroundRun(agentId, token)`.
    fn settle_foreground_run(state: &mut State, agent_id: &str, token: u64) {
        if matches!(state.runs.get(agent_id), Some(TrackedRun::Foreground { token: current, .. }) if *current == token)
        {
            state.runs.remove(agent_id);
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
        if !hold_busy {
            touch_updated_at(&mut agent.snapshot);
            self.emit_state_locked(state, agent_id, true);
        }
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
        if agent.snapshot.active_foreground_turn_id.is_some()
            || state.runs.contains_key(&normalized)
        {
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
        self.emit_state_locked(&mut state, agent_id, true);
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
            AgentManagerEvent::TimelineReplacement { .. } => {}
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
