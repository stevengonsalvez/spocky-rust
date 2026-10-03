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
//! Ported: the provider registry, `createAgent`, `registerSession`, resume,
//! import, reload, close, archive, steer, replace, cancel, rewind,
//! permission responses, the per-field setters, the session event pipeline
//! with stream coalescing, `streamAgent`, `runAgent`, `waitForAgentEvent`,
//! `subscribe`, the timeline queries and appends, provider sub-agents, the
//! importable and draft listings, and the metrics snapshot. Not ported yet
//! (each is absent here, not stubbed): the durable timeline store with its
//! `deleteCommittedTimeline` and four error logs, the Paseo tool catalog
//! factory with `setPaseoToolCatalogFactory`, and plugin lifecycle hooks
//! beyond validating a request.

mod archive;
mod create;
mod draft_listing;
mod events;
mod importable;
mod lifecycle;
mod log_error;
mod metrics;
mod provider_registry;
mod run;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use spocky_store::js_value::JsValue;
use tokio::sync::{Notify, mpsc};

pub use archive::{
    AgentArchivedCallback, AgentMetadataUpdates, DetachedAgent, LogWarn, UnarchiveUpdates,
};

/// `logger.info(bindings, message)`, also the shape of `logger.error`. A sink
/// must not call back into the manager, see [`LogWarn`].
pub type LogInfo = Arc<dyn Fn(JsValue, &str) + Send + Sync>;
pub use create::{
    CreateAgentOptions, ImportProviderSessionRequest, ReloadAgentOptions, ResumeAgentOptions,
};
pub use events::{AppendedTimelineItem, HydrateBroadcast, HydrateTimelineOptions};
pub use importable::{
    ImportablePersistedAgentQueryOptions, ImportableSessionProviderError,
    ManagedImportableProviderSession, ManagedImportableSessionsResult,
};
pub use lifecycle::AgentRunCancellationResult;
pub use metrics::AgentMetricsSnapshot;
pub use provider_registry::ProviderRegistryUpdate;
pub use run::{
    AgentRunResult, AgentSteerOptions, SteerDispatch, TurnEventStream, WaitForAgentOptions,
    WaitForAgentResult,
};

use crate::agent_projection::{AgentAttention, AgentPayloadView, ManagedAgentRecordView};
use crate::agent_sdk::{
    AgentClient, AgentError, AgentSession, BoxFuture, PaseoToolCatalog, Unsubscribe,
};
use crate::agent_storage::AgentStorage;
use crate::provider_subagents::ProviderSubagentStore;
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
    /// `{ type: "provider_subagent", event }`: a
    /// [`ProviderSubagentStore`] event.
    ProviderSubagent(JsValue),
}

/// The parent agent a `provider_subagent` store event belongs to.
fn subagent_parent(event: &JsValue) -> Option<&str> {
    let holder = if event.get("type").and_then(JsValue::as_str) == Some("upsert") {
        event.get("subagent")
    } else {
        Some(event)
    };
    holder
        .and_then(|holder| holder.get("parentAgentId"))
        .and_then(JsValue::as_str)
}

/// `beforeSteerUnavailableFallback`.
pub type SteerFallbackHook =
    Arc<dyn Fn(String, String) -> crate::agent_sdk::BoxFuture<'static, ()> + Send + Sync>;

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

/// `PaseoToolRuntimeContext` as the manager builds it: the calling agent and
/// its tool policy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaseoToolRuntimeContext {
    pub caller_agent_id: Option<String>,
    /// `ProviderPaseoToolsPolicy`.
    pub paseo_tool_policy: Option<JsValue>,
}

/// `PaseoToolCatalogFactory`: builds the tools one agent launches with.
pub type PaseoToolCatalogFactory = Arc<
    dyn Fn(
            PaseoToolRuntimeContext,
        ) -> BoxFuture<'static, Result<Arc<dyn PaseoToolCatalog>, AgentError>>
        + Send
        + Sync,
>;

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
    /// `logger.warn(bindings, message)`; warnings are dropped without it.
    pub log_warn: Option<LogWarn>,
    /// `logger.error(bindings, message)`; errors are dropped without it.
    pub log_error: Option<LogInfo>,
    /// `logger.info(bindings, message)`; the messages the manager logs at
    /// info level are dropped without it.
    pub log_info: Option<LogInfo>,
    pub on_workspace_state_may_have_changed: Option<WorkspaceStateCallback>,
    pub mcp_base_url: Option<String>,
    pub mcp_auth_token: Option<String>,
    /// `paseoToolsEnabled ?? true`.
    pub paseo_tools_enabled: Option<bool>,
    pub resolve_paseo_tool_policy: Option<PaseoToolPolicyResolver>,
    /// `paseoToolCatalogFactory`; none by default.
    pub paseo_tool_catalog_factory: Option<PaseoToolCatalogFactory>,
    pub append_system_prompt: Option<String>,
    pub agent_stream_coalesce_window_ms: Option<f64>,
    /// `pluginLifecycle` present with no plugin loaded, as the daemon runs
    /// it: before-hooks then only validate their request (see
    /// `create_agent`), and lifecycle events go nowhere.
    pub plugin_lifecycle: bool,
    /// `rescueTimeouts.interruptSessionMs` (default 2000).
    pub rescue_interrupt_session_ms: Option<u64>,
    /// `rescueTimeouts.reloadSessionCloseMs` (default 3000).
    pub rescue_reload_session_close_ms: Option<u64>,
    /// `beforeSteerUnavailableFallback`: awaited before an unavailable steer
    /// falls back to replacing the turn, with the agent id and the turn the
    /// steer was admitted for.
    pub before_steer_unavailable_fallback: Option<SteerFallbackHook>,
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
    /// `onAgentAttention`, replaceable through `setAgentAttentionCallback`.
    pub(crate) on_agent_attention: Option<AttentionCallback>,
    /// `onAgentArchived`, set through `setAgentArchivedCallback` (the pinned
    /// build has no constructor option for it).
    pub(crate) on_agent_archived: Option<AgentArchivedCallback>,
    pub(crate) clients: Vec<(String, Arc<dyn AgentClient>)>,
    pub(crate) provider_enabled: Vec<(String, bool)>,
    pub(crate) provider_definitions: Vec<(String, ProviderDefinition)>,
    /// `agents`, in map insertion order.
    pub(crate) agents: Vec<(String, ManagedAgent)>,
    pub(crate) timeline: TimelineStore,
    pub(crate) provider_subagents: ProviderSubagentStore,
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
    /// `steerEventBarriers`: provider events held while a steer is admitted.
    pub(crate) steer_event_barriers: HashMap<String, Vec<JsValue>>,
    /// `reloadedSessionCloses`: the close of each session a reload replaced.
    pub(crate) reloaded_session_closes: Vec<create::ReloadedClose>,
    pub(crate) mcp_base_url: Option<String>,
    pub(crate) paseo_tools_enabled: bool,
    /// `paseoToolCatalogFactory`, replaceable through
    /// `setPaseoToolCatalogFactory`.
    pub(crate) paseo_tool_catalog_factory: Option<PaseoToolCatalogFactory>,
    pub(crate) append_system_prompt: String,
    pub(crate) plugin_lifecycle: bool,
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
    pub(crate) log_warn: Option<LogWarn>,
    pub(crate) log_error: Option<LogInfo>,
    pub(crate) log_info: Option<LogInfo>,
    pub(crate) on_workspace_state_may_have_changed: Option<WorkspaceStateCallback>,
    pub(crate) mcp_auth_token: Option<String>,
    pub(crate) resolve_paseo_tool_policy: Option<PaseoToolPolicyResolver>,
    background_tasks: AtomicUsize,
    /// `agentRegistrationTasks`: in-flight `createAgent` and
    /// `resumeAgentFromPersistence` registrations.
    registration_tasks: AtomicUsize,
    /// Signalled when background work or registrations may have drained.
    background_idle: Notify,
    /// Signalled when an agent's session event queue empties.
    pub(crate) drain_idle: Notify,
    pub(crate) interrupt_session_ms: u64,
    pub(crate) reload_session_close_ms: u64,
    pub(crate) before_steer_unavailable_fallback: Option<SteerFallbackHook>,
    /// `waitForAgentRunStart` subscribers, settled as each `agent_state`
    /// is dispatched.
    pub(crate) run_start_waiters: Mutex<Vec<run::RunStartWaiter>>,
}

/// An in-flight agent registration; see
/// [`AgentManager::track_agent_registration`].
pub(crate) struct RegistrationGuard(Arc<Inner>);

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        if self.0.registration_tasks.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.background_idle.notify_waiters();
        }
    }
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
            on_agent_attention: options.on_agent_attention,
            on_agent_archived: None,
            clients: options.clients,
            provider_enabled,
            provider_definitions: options.provider_definitions,
            agents: Vec::new(),
            timeline: TimelineStore::default(),
            provider_subagents: ProviderSubagentStore::default(),
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
            reloaded_session_closes: Vec::new(),
            steer_event_barriers: HashMap::new(),
            mcp_base_url: options.mcp_base_url,
            paseo_tools_enabled: options.paseo_tools_enabled.unwrap_or(true),
            paseo_tool_catalog_factory: options.paseo_tool_catalog_factory,
            append_system_prompt: options.append_system_prompt.unwrap_or_default(),
            plugin_lifecycle: options.plugin_lifecycle,
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
                log_warn: options.log_warn,
                log_error: options.log_error,
                log_info: options.log_info,
                on_workspace_state_may_have_changed: options.on_workspace_state_may_have_changed,
                mcp_auth_token: options.mcp_auth_token,
                resolve_paseo_tool_policy: options.resolve_paseo_tool_policy,
                background_tasks: AtomicUsize::new(0),
                registration_tasks: AtomicUsize::new(0),
                background_idle: Notify::new(),
                drain_idle: Notify::new(),
                interrupt_session_ms: options
                    .rescue_interrupt_session_ms
                    .unwrap_or(lifecycle::INTERRUPT_SESSION_TIMEOUT_MS),
                before_steer_unavailable_fallback: options.before_steer_unavailable_fallback,
                reload_session_close_ms: options
                    .rescue_reload_session_close_ms
                    .unwrap_or(create::RELOAD_SESSION_CLOSE_TIMEOUT_MS),
                run_start_waiters: Mutex::new(Vec::new()),
            }),
        }
    }

    /// `logger.warn(bindings, message)`.
    pub(crate) fn emit_warn(&self, bindings: JsValue, message: &str) {
        if let Some(warn) = &self.inner.log_warn {
            warn(bindings, message);
        }
    }

    /// `logger.error(bindings, message)`.
    pub(crate) fn emit_error(&self, bindings: JsValue, message: &str) {
        if let Some(error) = &self.inner.log_error {
            error(bindings, message);
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    /// `setAgentAttentionCallback(callback)`.
    pub fn set_agent_attention_callback(&self, callback: AttentionCallback) {
        self.lock().on_agent_attention = Some(callback);
    }

    /// `setAgentArchivedCallback(callback)`.
    pub fn set_agent_archived_callback(&self, callback: AgentArchivedCallback) {
        self.lock().on_agent_archived = Some(callback);
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

    /// `setPaseoToolCatalogFactory(factory)`.
    pub fn set_paseo_tool_catalog_factory(&self, factory: Option<PaseoToolCatalogFactory>) {
        self.lock().paseo_tool_catalog_factory = factory;
    }

    /// `setAppendSystemPrompt(prompt)`.
    pub fn set_append_system_prompt(&self, prompt: Option<String>) {
        self.lock().append_system_prompt = prompt.unwrap_or_default();
    }

    /// `getPaseoToolPolicy(agentId)`.
    #[must_use]
    pub fn paseo_tool_policy(&self, agent_id: &str) -> Option<JsValue> {
        self.lock()
            .paseo_tool_policies
            .get(agent_id)
            .cloned()
            .flatten()
    }

    /// `getMcpAuthToken()`: the capability token the daemon's own MCP
    /// clients present, kept in the daemon.
    #[must_use]
    pub fn mcp_auth_token(&self) -> Option<&str> {
        self.inner.mcp_auth_token.as_deref()
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
            AgentManagerEvent::ProviderSubagent(event) => subagent_parent(event)
                .and_then(|parent| state.agent(parent))
                .is_some_and(|agent| agent.snapshot.internal),
        };
        let callbacks: Vec<AgentSubscriber> = state
            .subscribers
            .iter()
            .filter(|subscriber| match &subscriber.agent_id {
                Some(target) => match &event {
                    AgentManagerEvent::AgentStream { agent_id, .. } => agent_id == target,
                    AgentManagerEvent::AgentState(agent) => agent.id == *target,
                    AgentManagerEvent::TimelineReplacement { .. } => true,
                    AgentManagerEvent::ProviderSubagent(event) => {
                        subagent_parent(event) == Some(target.as_str())
                    }
                },
                None => !belongs_to_internal,
            })
            .map(|subscriber| Arc::clone(&subscriber.callback))
            .collect();
        if let AgentManagerEvent::AgentState(agent) = &event {
            self.settle_run_start_waiters(state, &agent.id);
        }
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
        self.send_batch(DispatchBatch::Call(Box::new(call)));
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

    /// `hasInFlightRun(agentId)`: running, a foreground turn, or a tracked
    /// run.
    #[must_use]
    pub fn has_in_flight_run(&self, agent_id: &str) -> bool {
        let state = self.lock();
        state.agent(agent_id).is_some_and(|agent| {
            agent.snapshot.lifecycle == AgentLifecycle::Running
                || agent
                    .snapshot
                    .active_foreground_turn_id
                    .as_deref()
                    .is_some_and(|turn| !turn.is_empty())
                || state.runs.contains_key(agent_id)
        })
    }

    /// `requirePublicAgent(id)`.
    fn require_public_agent<'a>(
        state: &'a State,
        agent_id: &str,
    ) -> Result<&'a ManagedAgent, AgentError> {
        let agent = Self::require_agent(state, agent_id)?;
        if agent.snapshot.internal {
            return Err(AgentError::new(format!(
                "Unknown agent '{}'",
                agent.snapshot.id
            )));
        }
        Ok(agent)
    }

    /// `listProviderSubagents(parentAgentId)`.
    ///
    /// # Errors
    ///
    /// `Unknown agent` for a missing or internal parent.
    pub fn list_provider_subagents(
        &self,
        parent_agent_id: &str,
    ) -> Result<Vec<JsValue>, AgentError> {
        let state = self.lock();
        Self::require_public_agent(&state, parent_agent_id)?;
        Ok(state.provider_subagents.list(parent_agent_id))
    }

    /// `listProviderSubagentActivity()`: every child of a public agent.
    #[must_use]
    pub fn list_provider_subagent_activity(&self) -> Vec<JsValue> {
        let state = self.lock();
        state
            .provider_subagents
            .list_all()
            .into_iter()
            .filter(|subagent| {
                subagent
                    .get("parentAgentId")
                    .and_then(JsValue::as_str)
                    .and_then(|parent| state.agent(parent))
                    .is_some_and(|agent| !agent.snapshot.internal)
            })
            .collect()
    }

    /// `getProviderSubagent(parentAgentId, subagentId)`.
    ///
    /// # Errors
    ///
    /// `Unknown agent` for a missing or internal parent.
    pub fn get_provider_subagent(
        &self,
        parent_agent_id: &str,
        subagent_id: &str,
    ) -> Result<Option<JsValue>, AgentError> {
        let state = self.lock();
        Self::require_public_agent(&state, parent_agent_id)?;
        Ok(state.provider_subagents.get(parent_agent_id, subagent_id))
    }

    /// `fetchProviderSubagentTimeline(parentAgentId, subagentId, options)`.
    ///
    /// # Errors
    ///
    /// `Unknown agent` for a missing or internal parent, and the timeline
    /// store's errors for an unknown child.
    pub fn fetch_provider_subagent_timeline(
        &self,
        parent_agent_id: &str,
        subagent_id: &str,
        direction: crate::timeline::FetchDirection,
        cursor: Option<&crate::timeline::TimelineCursor>,
        limit: Option<usize>,
    ) -> Result<crate::timeline::TimelineFetch, AgentError> {
        let state = self.lock();
        Self::require_public_agent(&state, parent_agent_id)?;
        state
            .provider_subagents
            .fetch_timeline(parent_agent_id, subagent_id, direction, cursor, limit)
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

    /// `trackAgentRegistrationOperation`: the registration counts as in
    /// flight until the returned guard drops.
    ///
    /// Ceiling: the guard lives inside the `create_agent` and
    /// `resume_agent_from_persistence` futures, so a registration counts only
    /// while its future is polled. A caller that drops one of those futures
    /// before it settles (a request handler aborted by a closed socket, a
    /// `tokio::select!` or timeout around the call, a task cancelled at
    /// shutdown) ends the count early, where the baseline's promise keeps
    /// running and `flushForShutdown` waits for it. Run registrations in
    /// their own spawned task, as the daemon's create path does, to keep the
    /// baseline's behavior.
    pub(crate) fn track_agent_registration(&self) -> RegistrationGuard {
        self.inner.registration_tasks.fetch_add(1, Ordering::SeqCst);
        RegistrationGuard(Arc::clone(&self.inner))
    }

    /// `flushForShutdown()`: as [`Self::flush`], and also waits for agent
    /// registrations that crossed the shutdown barrier, which own provider
    /// sessions until they install or close them.
    pub async fn flush_for_shutdown(&self) {
        self.flush_coalescer_all();
        loop {
            let idle = self.inner.background_idle.notified();
            if self.inner.background_tasks.load(Ordering::SeqCst) == 0
                && self.inner.registration_tasks.load(Ordering::SeqCst) == 0
            {
                break;
            }
            idle.await;
        }
        self.dispatched().await;
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
