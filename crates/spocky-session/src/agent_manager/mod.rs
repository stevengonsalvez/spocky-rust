//! The agent manager from pinned Paseo `agent/agent-manager.ts`: live agents
//! over provider sessions, their timelines, runs, attention, persistence,
//! and the event feed the session layer forwards to clients.
//!
//! Concurrency follows the baseline's single event loop. All manager state
//! sits behind one lock that is never held across an `await`, so each
//! synchronous stretch of a baseline method runs atomically and every
//! `await` is a point where other work may interleave, as in JavaScript.
//! Provider events are staged or queued synchronously when they arrive (the
//! baseline's `enqueueSessionEvent`) and each agent's queue drains in order
//! on its own task. Subscriber callbacks run on one dispatcher task, in the
//! order events were dispatched, so a callback may call back into the
//! manager.
//!
//! Ported for the G1 path: provider registry, `createAgent` and
//! `registerSession`, the session event pipeline with stream coalescing,
//! `streamAgent`, `runAgent`, `waitForAgentEvent`, `subscribe`, and the
//! timeline queries. Not ported yet (each is absent here, not stubbed):
//! plugin lifecycle hooks, the durable timeline store, the Paseo tool
//! catalog factory, provider sub-agents, resume, import, reload, close,
//! archive, steer, replace, cancel, rewind, permission responses, and the
//! per-field setters.

mod create;
mod events;
mod lifecycle;
mod run;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use spocky_store::js_value::JsValue;
use tokio::sync::{Notify, mpsc};

pub use create::CreateAgentOptions;
pub use lifecycle::AgentRunCancellationResult;
pub use run::{AgentRunResult, TurnEventStream, WaitForAgentOptions, WaitForAgentResult};

use crate::agent_projection::{AgentAttention, AgentPayloadView, ManagedAgentRecordView};
use crate::agent_sdk::{AgentClient, AgentError, AgentSession, Unsubscribe};
use crate::agent_storage::AgentStorage;
use crate::stream_coalescer::{AGENT_STREAM_COALESCE_DEFAULT_WINDOW_MS, AgentStreamCoalescer};
use crate::timeline::TimelineStore;

/// `AgentLifecycleStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    Initializing,
    Idle,
    Running,
    Error,
    Closed,
}

impl AgentLifecycle {
    /// The status string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Error => "error",
            Self::Closed => "closed",
        }
    }

    /// `isAgentBusy`: initializing or running.
    #[must_use]
    pub const fn is_busy(self) -> bool {
        matches!(self, Self::Initializing | Self::Running)
    }
}

/// A copy of a `ManagedAgent` as `{ ...agent }` hands it out, without the
/// provider session.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedAgentSnapshot {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    pub workspace_id: Option<String>,
    pub owner: Option<JsValue>,
    /// `AgentCapabilityFlags`.
    pub capabilities: JsValue,
    /// `AgentSessionConfig`.
    pub config: JsValue,
    pub runtime_info: Option<JsValue>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
    pub available_modes: Vec<JsValue>,
    pub features: Option<JsValue>,
    pub current_mode_id: Option<String>,
    /// `pendingPermissions` as `[id, request]` in map order.
    pub pending_permissions: Vec<(String, JsValue)>,
    pub pending_replacement: bool,
    pub persistence: Option<JsValue>,
    pub history_primed: bool,
    pub last_user_message_at_millis: Option<i64>,
    pub active_turn_id: Option<String>,
    pub active_turn_started_at_millis: Option<i64>,
    pub last_usage: Option<JsValue>,
    pub last_error: Option<String>,
    pub attention: AgentAttention,
    pub internal: bool,
    /// `Record<string, string>`.
    pub labels: JsValue,
    pub lifecycle: AgentLifecycle,
    pub active_foreground_turn_id: Option<String>,
}

impl ManagedAgentSnapshot {
    /// The fields `toStoredAgentRecord` reads.
    #[must_use]
    pub fn record_view(&self) -> ManagedAgentRecordView {
        ManagedAgentRecordView {
            id: self.id.clone(),
            provider: self.provider.clone(),
            cwd: self.cwd.clone(),
            workspace_id: self.workspace_id.clone(),
            created_at_millis: self.created_at_millis,
            updated_at_millis: self.updated_at_millis,
            last_user_message_at_millis: self.last_user_message_at_millis,
            labels: self.labels.clone(),
            lifecycle: self.lifecycle.as_str().to_owned(),
            current_mode_id: self.current_mode_id.clone(),
            config: self.config.clone(),
            runtime_info: self.runtime_info.clone(),
            features: self.features.clone(),
            persistence: self.persistence.clone(),
            last_error: self.last_error.clone(),
            attention: self.attention.clone(),
            internal: Some(self.internal),
            owner: self.owner.clone(),
        }
    }

    /// The fields `toAgentPayload` reads.
    #[must_use]
    pub fn payload_view(&self) -> AgentPayloadView {
        AgentPayloadView {
            record: self.record_view(),
            capabilities: self.capabilities.clone(),
            available_modes: self.available_modes.clone(),
            pending_permissions: self
                .pending_permissions
                .iter()
                .map(|(_, request)| request.clone())
                .collect(),
            active_turn_id: self.active_turn_id.clone(),
            active_turn_started_at_millis: self.active_turn_started_at_millis,
            last_usage: self.last_usage.clone(),
        }
    }
}

/// `AgentManagerEvent`.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentManagerEvent {
    /// `{ type: "agent_state", agent }`.
    AgentState(Box<ManagedAgentSnapshot>),
    /// `{ type: "timeline_replacement", agentId, epoch }`.
    TimelineReplacement { agent_id: String, epoch: String },
    /// `{ type: "agent_stream", agentId, event, seq?, epoch?, timestamp? }`.
    AgentStream {
        agent_id: String,
        event: JsValue,
        seq: Option<i64>,
        epoch: Option<String>,
        timestamp: Option<String>,
    },
}

/// `AgentSubscriber`.
pub type AgentSubscriber = Arc<dyn Fn(&AgentManagerEvent) + Send + Sync>;

/// `SubscribeOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscribeOptions {
    pub agent_id: Option<String>,
    /// `replayState`; `None` replays, as `!== false` does.
    pub replay_state: Option<bool>,
}

/// The `onAgentAttention` callback's parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttentionNotice {
    pub agent_id: String,
    pub provider: String,
    /// `finished`, `error` or `permission`.
    pub reason: String,
}

/// `idFactory`.
pub type IdFactory = Arc<dyn Fn() -> String + Send + Sync>;
/// `onAgentAttention`.
pub type AttentionCallback = Arc<dyn Fn(AgentAttentionNotice) + Send + Sync>;
/// `onWorkspaceStateMayHaveChanged({ cwd })`, given the cwd.
pub type WorkspaceStateCallback = Arc<dyn Fn(&str) + Send + Sync>;
/// `resolvePaseoToolPolicy(provider)`: a `ProviderPaseoToolsPolicy`.
pub type PaseoToolPolicyResolver = Arc<dyn Fn(&str) -> Option<JsValue> + Send + Sync>;

/// `validateOptions(options)`; it may throw.
pub type ValidateProviderOptions =
    Arc<dyn Fn(Option<&JsValue>) -> Result<Option<JsValue>, AgentError> + Send + Sync>;
/// `applyOptions(config, options)` and `applyToolPolicy(config, toolPolicy)`.
pub type ApplyProviderConfig = Arc<dyn Fn(&JsValue, Option<&JsValue>) -> JsValue + Send + Sync>;

/// `ProviderEnabledFlag`.
#[derive(Clone, Default)]
pub struct ProviderDefinition {
    pub enabled: bool,
    pub derived_from_provider_id: Option<String>,
    pub validate_options: Option<ValidateProviderOptions>,
    pub apply_options: Option<ApplyProviderConfig>,
    pub apply_tool_policy: Option<ApplyProviderConfig>,
}

/// `AgentManagerOptions` for the ported members. Clients and definitions are
/// in object key order.
#[derive(Clone, Default)]
pub struct AgentManagerOptions {
    pub clients: Vec<(String, Arc<dyn AgentClient>)>,
    pub provider_definitions: Vec<(String, ProviderDefinition)>,
    /// `idFactory`; `randomUUID` by default.
    pub id_factory: Option<IdFactory>,
    pub registry: Option<AgentStorage>,
    pub on_agent_attention: Option<AttentionCallback>,
    pub on_workspace_state_may_have_changed: Option<WorkspaceStateCallback>,
    pub mcp_base_url: Option<String>,
    pub mcp_auth_token: Option<String>,
    /// `paseoToolsEnabled ?? true`.
    pub paseo_tools_enabled: Option<bool>,
    pub resolve_paseo_tool_policy: Option<PaseoToolPolicyResolver>,
    pub append_system_prompt: Option<String>,
    pub agent_stream_coalesce_window_ms: Option<f64>,
    /// `rescueTimeouts.interruptSessionMs` (default 2000).
    pub rescue_interrupt_session_ms: Option<u64>,
}

/// A live agent: the snapshot fields plus what never leaves the manager.
pub(crate) struct ManagedAgent {
    pub(crate) snapshot: ManagedAgentSnapshot,
    pub(crate) session: Option<Arc<dyn AgentSession>>,
    /// `bufferedPermissionResolutions`, in map order.
    pub(crate) buffered_permission_resolutions: Vec<(String, JsValue)>,
    pub(crate) in_flight_permission_responses: Vec<String>,
    pub(crate) foreground_turn_waiters: Vec<run::ForegroundTurnWaiter>,
    /// `finalizedForegroundTurnIds`, oldest first, at most 50.
    pub(crate) finalized_foreground_turn_ids: Vec<String>,
    pub(crate) unsubscribe_session: Option<Unsubscribe>,
}

struct SubscriptionRecord {
    id: u64,
    callback: AgentSubscriber,
    agent_id: Option<String>,
}

/// Each agent's queue of provider events waiting to be handled.
#[derive(Default)]
pub(crate) struct SessionEventQueue {
    pub(crate) events: std::collections::VecDeque<JsValue>,
    pub(crate) draining: bool,
}

pub(crate) struct State {
    pub(crate) clients: Vec<(String, Arc<dyn AgentClient>)>,
    pub(crate) provider_enabled: Vec<(String, bool)>,
    pub(crate) provider_definitions: Vec<(String, ProviderDefinition)>,
    /// `agents`, in map insertion order.
    pub(crate) agents: Vec<(String, ManagedAgent)>,
    pub(crate) timeline: TimelineStore,
    pub(crate) coalescer: AgentStreamCoalescer,
    pub(crate) runs: HashMap<String, run::TrackedRun>,
    subscribers: Vec<SubscriptionRecord>,
    next_subscriber_id: u64,
    pub(crate) previous_statuses: HashMap<String, AgentLifecycle>,
    pub(crate) session_queues: HashMap<String, SessionEventQueue>,
    pub(crate) paseo_tool_policies: HashMap<String, Option<JsValue>>,
    pub(crate) accepting_agent_registrations: bool,
    /// Ids for run tokens and turn waiters.
    pub(crate) next_token: u64,
    /// `lifecycleMutationTails`: one first-in first-out lane per agent.
    pub(crate) lifecycle_lanes: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// `foregroundMutationTails`.
    pub(crate) foreground_lanes: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// `inFlightAgentCloses`.
    pub(crate) inflight_closes: HashMap<String, lifecycle::SharedClose>,
    pub(crate) mcp_base_url: Option<String>,
    pub(crate) paseo_tools_enabled: bool,
    pub(crate) append_system_prompt: String,
}

impl State {
    pub(crate) fn agent(&self, agent_id: &str) -> Option<&ManagedAgent> {
        self.agents
            .iter()
            .find(|(id, _)| id == agent_id)
            .map(|(_, agent)| agent)
    }

    pub(crate) fn agent_mut(&mut self, agent_id: &str) -> Option<&mut ManagedAgent> {
        self.agents
            .iter_mut()
            .find(|(id, _)| id == agent_id)
            .map(|(_, agent)| agent)
    }

    pub(crate) fn client(&self, provider: &str) -> Option<Arc<dyn AgentClient>> {
        self.clients
            .iter()
            .find(|(id, _)| id == provider)
            .map(|(_, client)| Arc::clone(client))
    }
}

/// Work for the dispatcher task, run in the order it was sent.
enum DispatchBatch {
    /// Subscriber callbacks for one event.
    Event(Vec<AgentSubscriber>, Arc<AgentManagerEvent>),
    /// A user callback the baseline calls synchronously (`onAgentAttention`,
    /// `onWorkspaceStateMayHaveChanged`), run outside the manager lock.
    Call(Box<dyn FnOnce() + Send>),
}

pub(crate) struct Inner {
    pub(crate) state: Mutex<State>,
    dispatch_tx: mpsc::UnboundedSender<DispatchBatch>,
    /// Batches sent to the dispatcher and not yet run.
    dispatch_pending: Arc<AtomicUsize>,
    dispatch_idle: Arc<Notify>,
    pub(crate) id_factory: IdFactory,
    pub(crate) registry: Option<AgentStorage>,
    pub(crate) on_agent_attention: Option<AttentionCallback>,
    pub(crate) on_workspace_state_may_have_changed: Option<WorkspaceStateCallback>,
    pub(crate) mcp_auth_token: Option<String>,
    pub(crate) resolve_paseo_tool_policy: Option<PaseoToolPolicyResolver>,
    background_tasks: AtomicUsize,
    background_idle: Notify,
    /// Signalled when an agent's session event queue empties.
    pub(crate) drain_idle: Notify,
    pub(crate) interrupt_session_ms: u64,
}

/// `AgentManager`. Cloning shares the manager.
#[derive(Clone)]
pub struct AgentManager {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `AgentIdSchema = z.guid()`: 8-4-4-4-12 hex digits.
fn is_guid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// `validateAgentId(agentId, source)`.
///
/// # Errors
///
/// `<source>: agentId must be a UUID`.
pub fn validate_agent_id(agent_id: &str, source: &str) -> Result<String, AgentError> {
    if is_guid(agent_id) {
        Ok(agent_id.to_owned())
    } else {
        Err(AgentError::new(format!("{source}: agentId must be a UUID")))
    }
}

impl AgentManager {
    /// `new AgentManager(options)`. Must run inside a Tokio runtime: the
    /// subscriber dispatcher is a task.
    #[must_use]
    pub fn new(options: AgentManagerOptions) -> Self {
        let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel::<DispatchBatch>();
        let dispatch_pending = Arc::new(AtomicUsize::new(0));
        let dispatch_idle = Arc::new(Notify::new());
        let pending = Arc::clone(&dispatch_pending);
        let idle = Arc::clone(&dispatch_idle);
        tokio::spawn(async move {
            while let Some(batch) = dispatch_rx.recv().await {
                match batch {
                    DispatchBatch::Event(callbacks, event) => {
                        for callback in callbacks {
                            callback(&event);
                        }
                    }
                    DispatchBatch::Call(call) => call(),
                }
                if pending.fetch_sub(1, Ordering::SeqCst) == 1 {
                    idle.notify_waiters();
                }
            }
        });
        let mut provider_enabled = Vec::new();
        for (provider, definition) in &options.provider_definitions {
            provider_enabled.push((provider.clone(), definition.enabled));
        }
        let state = State {
            clients: options.clients,
            provider_enabled,
            provider_definitions: options.provider_definitions,
            agents: Vec::new(),
            timeline: TimelineStore::default(),
            coalescer: AgentStreamCoalescer::new(
                options
                    .agent_stream_coalesce_window_ms
                    .unwrap_or(AGENT_STREAM_COALESCE_DEFAULT_WINDOW_MS),
            ),
            runs: HashMap::new(),
            subscribers: Vec::new(),
            next_subscriber_id: 0,
            previous_statuses: HashMap::new(),
            session_queues: HashMap::new(),
            paseo_tool_policies: HashMap::new(),
            accepting_agent_registrations: true,
            next_token: 0,
            lifecycle_lanes: HashMap::new(),
            foreground_lanes: HashMap::new(),
            inflight_closes: HashMap::new(),
            mcp_base_url: options.mcp_base_url,
            paseo_tools_enabled: options.paseo_tools_enabled.unwrap_or(true),
            append_system_prompt: options.append_system_prompt.unwrap_or_default(),
        };
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                dispatch_tx,
                dispatch_pending,
                dispatch_idle,
                id_factory: options
                    .id_factory
                    .unwrap_or_else(|| Arc::new(crate::clock::random_uuid)),
                registry: options.registry,
                on_agent_attention: options.on_agent_attention,
                on_workspace_state_may_have_changed: options.on_workspace_state_may_have_changed,
                mcp_auth_token: options.mcp_auth_token,
                resolve_paseo_tool_policy: options.resolve_paseo_tool_policy,
                background_tasks: AtomicUsize::new(0),
                background_idle: Notify::new(),
                drain_idle: Notify::new(),
                interrupt_session_ms: options
                    .rescue_interrupt_session_ms
                    .unwrap_or(lifecycle::INTERRUPT_SESSION_TIMEOUT_MS),
            }),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    /// `prepareForShutdown()`.
    pub fn prepare_for_shutdown(&self) {
        self.lock().accepting_agent_registrations = false;
    }

    /// `setMcpBaseUrl(url)`.
    pub fn set_mcp_base_url(&self, url: Option<String>) {
        self.lock().mcp_base_url = url;
    }

    /// `setPaseoToolsEnabled(enabled)`.
    pub fn set_paseo_tools_enabled(&self, enabled: bool) {
        self.lock().paseo_tools_enabled = enabled;
    }

    /// `setAppendSystemPrompt(prompt)`.
    pub fn set_append_system_prompt(&self, prompt: Option<String>) {
        self.lock().append_system_prompt = prompt.unwrap_or_default();
    }

    /// `getRegisteredProviderIds()`.
    #[must_use]
    pub fn registered_provider_ids(&self) -> Vec<String> {
        self.lock()
            .clients
            .iter()
            .map(|(provider, _)| provider.clone())
            .collect()
    }

    /// `dispatch(event)`: picks the subscribers this event reaches now and
    /// hands them to the dispatcher in order.
    pub(crate) fn dispatch(&self, state: &State, event: AgentManagerEvent) {
        let belongs_to_internal = match &event {
            AgentManagerEvent::AgentState(agent) => agent.internal,
            AgentManagerEvent::AgentStream { agent_id, .. } => state
                .agent(agent_id)
                .is_some_and(|agent| agent.snapshot.internal),
            AgentManagerEvent::TimelineReplacement { .. } => false,
        };
        let callbacks: Vec<AgentSubscriber> = state
            .subscribers
            .iter()
            .filter(|subscriber| match &subscriber.agent_id {
                Some(target) => match &event {
                    AgentManagerEvent::AgentStream { agent_id, .. } => agent_id == target,
                    AgentManagerEvent::AgentState(agent) => agent.id == *target,
                    AgentManagerEvent::TimelineReplacement { .. } => true,
                },
                None => !belongs_to_internal,
            })
            .map(|subscriber| Arc::clone(&subscriber.callback))
            .collect();
        if !callbacks.is_empty() {
            self.send_batch(DispatchBatch::Event(callbacks, Arc::new(event)));
        }
    }

    fn send_batch(&self, batch: DispatchBatch) {
        self.inner.dispatch_pending.fetch_add(1, Ordering::SeqCst);
        if self.inner.dispatch_tx.send(batch).is_err() {
            self.inner.dispatch_pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Resolves once every subscriber callback dispatched so far has run.
    pub async fn dispatched(&self) {
        loop {
            let idle = self.inner.dispatch_idle.notified();
            if self.inner.dispatch_pending.load(Ordering::SeqCst) == 0 {
                return;
            }
            idle.await;
        }
    }

    /// Runs `call` on the dispatcher, after every event dispatched so far.
    pub(crate) fn call_in_order(&self, call: impl FnOnce() + Send + 'static) {
        let _ = self
            .inner
            .dispatch_tx
            .send(DispatchBatch::Call(Box::new(call)));
    }

    /// `subscribe(callback, options)`: returns the unsubscribe function.
    ///
    /// # Errors
    ///
    /// `subscribe: agentId must be a UUID` for a malformed agent id.
    pub fn subscribe(
        &self,
        callback: AgentSubscriber,
        options: SubscribeOptions,
    ) -> Result<impl FnOnce() + Send + 'static, AgentError> {
        let SubscribeOptions {
            agent_id,
            replay_state,
        } = options;
        let target = agent_id
            .as_deref()
            .map(|agent_id| validate_agent_id(agent_id, "subscribe"))
            .transpose()?;
        let mut state = self.lock();
        state.next_subscriber_id += 1;
        let id = state.next_subscriber_id;
        if replay_state != Some(false) {
            let replay: Vec<AgentManagerEvent> = match &target {
                Some(agent_id) => state
                    .agent(agent_id)
                    .map(|agent| AgentManagerEvent::AgentState(Box::new(agent.snapshot.clone())))
                    .into_iter()
                    .collect(),
                // Global subscribers skip internal agents during replay.
                None => state
                    .agents
                    .iter()
                    .filter(|(_, agent)| !agent.snapshot.internal)
                    .map(|(_, agent)| {
                        AgentManagerEvent::AgentState(Box::new(agent.snapshot.clone()))
                    })
                    .collect(),
            };
            for event in replay {
                self.send_batch(DispatchBatch::Event(
                    vec![Arc::clone(&callback)],
                    Arc::new(event),
                ));
            }
        }
        state.subscribers.push(SubscriptionRecord {
            id,
            callback,
            agent_id: target,
        });
        drop(state);
        let inner = Arc::clone(&self.inner);
        Ok(move || {
            lock(&inner.state)
                .subscribers
                .retain(|subscriber| subscriber.id != id);
        })
    }

    /// `subscriptionCount()`.
    #[must_use]
    pub fn subscription_count(&self) -> usize {
        self.lock().subscribers.len()
    }

    /// `listAgents()`: public agents in registration order.
    #[must_use]
    pub fn list_agents(&self) -> Vec<ManagedAgentSnapshot> {
        self.lock()
            .agents
            .iter()
            .filter(|(_, agent)| !agent.snapshot.internal)
            .map(|(_, agent)| agent.snapshot.clone())
            .collect()
    }

    /// `getAgent(id)`.
    #[must_use]
    pub fn get_agent(&self, agent_id: &str) -> Option<ManagedAgentSnapshot> {
        self.lock()
            .agent(agent_id)
            .map(|agent| agent.snapshot.clone())
    }

    /// `requireAgent(id)`.
    pub(crate) fn require_agent<'a>(
        state: &'a State,
        agent_id: &str,
    ) -> Result<&'a ManagedAgent, AgentError> {
        let normalized = validate_agent_id(agent_id, "requireAgent")?;
        state
            .agent(&normalized)
            .ok_or_else(|| AgentError::new(format!("Unknown agent '{normalized}'")))
    }

    /// `getTimeline(id)`: the projected items.
    ///
    /// # Errors
    ///
    /// The baseline's unknown or malformed agent errors.
    pub fn get_timeline(&self, agent_id: &str) -> Result<Vec<JsValue>, AgentError> {
        let state = self.lock();
        Self::require_agent(&state, agent_id)?;
        Ok(state
            .timeline
            .rows(agent_id)
            .map(|rows| rows.iter().map(|row| row.item.clone()).collect())
            .unwrap_or_default())
    }

    /// `getTimelineRows(id)` without a durable store: the projected rows.
    ///
    /// # Errors
    ///
    /// The baseline's unknown or malformed agent errors.
    pub fn get_timeline_rows(&self, agent_id: &str) -> Result<Vec<JsValue>, AgentError> {
        let state = self.lock();
        Self::require_agent(&state, agent_id)?;
        state
            .timeline
            .rows(agent_id)
            .map(|rows| rows.iter().map(|row| row.to_js(false)).collect())
            .map_err(|error| AgentError::new(error.to_string()))
    }

    /// `fetchTimeline(id, options)`.
    ///
    /// # Errors
    ///
    /// The baseline's unknown or malformed agent errors, or a timeline
    /// `TypeError`.
    pub fn fetch_timeline(
        &self,
        agent_id: &str,
        direction: crate::timeline::FetchDirection,
        cursor: Option<&crate::timeline::TimelineCursor>,
        limit: Option<usize>,
    ) -> Result<crate::timeline::TimelineFetch, AgentError> {
        let state = self.lock();
        Self::require_agent(&state, agent_id)?;
        state
            .timeline
            .fetch(agent_id, direction, cursor, limit)
            .map_err(|error| match error {
                crate::timeline::TimelineError::Type(error) => AgentError {
                    name: "TypeError".to_owned(),
                    message: error.0,
                },
                crate::timeline::TimelineError::UnknownAgent(error) => {
                    AgentError::new(error.to_string())
                }
            })
    }

    /// Runs `task` in the background, tracked for [`Self::flush`]
    /// (`trackBackgroundTask`).
    pub(crate) fn track_background_task(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        self.inner.background_tasks.fetch_add(1, Ordering::SeqCst);
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            task.await;
            if inner.background_tasks.fetch_sub(1, Ordering::SeqCst) == 1 {
                inner.background_idle.notify_waiters();
            }
        });
    }

    /// `flush()`: flushes coalesced stream chunks, then waits for background
    /// work, including work started while waiting, and for the subscriber
    /// callbacks dispatched so far (the baseline runs those synchronously).
    pub async fn flush(&self) {
        self.flush_coalescer_all();
        loop {
            let idle = self.inner.background_idle.notified();
            if self.inner.background_tasks.load(Ordering::SeqCst) == 0 {
                break;
            }
            idle.await;
        }
        self.dispatched().await;
    }
}
