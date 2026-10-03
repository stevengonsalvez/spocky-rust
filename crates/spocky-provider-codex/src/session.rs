//! Codex app-server agent session.
//!
//! Port of `CodexAppServerAgentSession` and `CodexAppServerAgentClient` from
//! pinned Paseo `codex-app-server-agent.ts` for the vertical slice: connect
//! (`initialize`, `initialized`, `config/read`, `collaborationMode/list`,
//! `skills/list`), thread creation (`getUserSavedConfig`, `config/read`,
//! `model/list`, `thread/start`), `turn/start`, `turn/interrupt`, close, and
//! root-thread notifications mapped to stream events.
//!
//! Stream events are JSON objects built in Paseo's key order. Events are
//! tagged with `turnId` at emission time exactly as `notifySubscribers` does.
//!
//! Anything Paseo would render through a path not yet ported (tool items,
//! approvals, sub-agent threads, slash commands, non-text prompt blocks,
//! resume history, plan mode) is recorded in [`CodexSession::unported`] so a
//! caller can fail loudly instead of diverging silently.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::catalog::{self, normalize_thinking, read_configured_defaults};
use crate::history::{HistoryEntry, project_thread_history};
use crate::items::{
    self, AsyncQuestionItem, AsyncQuestionResolution, ThreadItemMapping, async_question_permission,
    async_question_record, async_question_timeline, item_type, non_empty_string, plan_tool_call,
    plan_update_to_todo, thread_item_to_timeline, to_agent_usage,
};
use crate::launch::{self, CODEX_PROVIDER, CodexGates, CustomProvider, ProviderRuntimeSettings};
use crate::notification::{ItemSource, ParsedNotification, parse_notification};
use crate::options::parse_provider_options;
use spocky_contracts::js::js_string as contracts_js_string;
use spocky_contracts::js_value::js_text_to_utf8;
use spocky_contracts::text::{is_js_whitespace, js_trim};

use crate::tools::{
    ExecNotification, PatchNotification, ToolMapping, decode_output_delta_chunk,
    exec_notification_to_tool_call, map_patch_notification,
};
use crate::transport::{
    AppServerClient, ClientError, DEFAULT_REQUEST_TIMEOUT, Responder, js_truthy, to_js_value,
};

const TURN_START_TIMEOUT: Duration = Duration::from_millis(90 * 1000);
const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(2_000);
const ASSISTANT_MESSAGE_BOUNDARY_MARKDOWN: &str = "\n\n---\n\n";
const DEFAULT_CODEX_MODE_ID: &str = "auto";
const CLOSED_MESSAGE: &str = "Codex app-server session is closed";
const CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(2_000);
/// Returned when `fetch_catalog` hits its deadline; the caller reports its
/// own timeout, as Paseo's `runProviderRefreshWithDeadline` does.
pub const CATALOG_DEADLINE_MESSAGE: &str = "Codex catalog refresh aborted at its deadline";

/// Server request methods Paseo answers through flows not yet ported.
const UNPORTED_REQUEST_METHODS: [&str; 3] = [
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
    "tool/requestUserInput",
];

/// `AgentSessionConfig` fields the Codex provider reads. Optional fields
/// keep Paseo's missing-versus-present distinction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionConfig {
    pub cwd: String,
    pub system_prompt: Option<String>,
    pub daemon_append_system_prompt: Option<String>,
    pub mode_id: Option<String>,
    pub model: Option<String>,
    pub thinking_option_id: Option<String>,
    pub feature_values: Option<Map<String, Value>>,
    /// `title?: string | null`, as given.
    pub title: Option<Value>,
    /// `providerOptions?: unknown`, as given: the session constructor parses
    /// it (`null` and absent read as `{}`; any other non-object is a `ZodError`).
    pub provider_options: Option<Value>,
    /// `ToolPolicy` (`{ preapproved: [{ server, tool }] }`).
    pub tool_policy: Option<Value>,
    /// `Record<string, McpServerConfig>`.
    pub mcp_servers: Option<Map<String, Value>>,
}

impl SessionConfig {
    /// A config from stored agent metadata merged with overrides, as Paseo
    /// spreads them into `AgentSessionConfig`: strings and objects are taken
    /// as given, `null` and other types read as absent. `providerOptions` is the
    /// exception: it is kept whatever it holds, for the constructor to parse.
    #[must_use]
    pub fn from_json(record: &Map<String, Value>) -> Self {
        let text = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
        let object = |key: &str| record.get(key).and_then(Value::as_object).cloned();
        Self {
            cwd: text("cwd").unwrap_or_default(),
            system_prompt: text("systemPrompt"),
            daemon_append_system_prompt: text("daemonAppendSystemPrompt"),
            mode_id: text("modeId"),
            model: text("model"),
            thinking_option_id: text("thinkingOptionId"),
            feature_values: object("featureValues"),
            title: record.get("title").cloned(),
            provider_options: record.get("providerOptions").cloned(),
            tool_policy: record
                .get("toolPolicy")
                .filter(|policy| !policy.is_null())
                .cloned(),
            mcp_servers: object("mcpServers"),
        }
    }
}

/// User prompt for one turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    Text(String),
    /// `AgentPromptContentBlock[]`.
    Blocks(Vec<Value>),
}

/// `AgentRunOptions` subset used by `startTurn`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOptions {
    pub client_message_id: Option<String>,
}

pub type Subscriber = Arc<dyn Fn(&Value) + Send + Sync>;
pub type SpawnAppServer = Box<dyn Fn() -> Result<Child, String> + Send + Sync>;

struct ModePreset {
    approval_policy: &'static str,
    sandbox: &'static str,
    approvals_reviewer: &'static str,
}

const MODE_PRESET_IDS: [&str; 4] = ["read-only", "auto", "auto-review", "full-access"];

fn mode_preset(mode_id: &str) -> Option<ModePreset> {
    let (approval_policy, sandbox, approvals_reviewer) = match mode_id {
        "read-only" => ("on-request", "read-only", "user"),
        "auto" => ("on-request", "workspace-write", "user"),
        "auto-review" => ("on-request", "workspace-write", "auto_review"),
        "full-access" => ("never", "danger-full-access", "user"),
        _ => return None,
    };
    Some(ModePreset {
        approval_policy,
        sandbox,
        approvals_reviewer,
    })
}

/// `validateCodexMode`.
///
/// # Errors
/// Returns Paseo's `Invalid Codex mode` message for an unknown mode.
pub fn validate_mode(mode_id: &str) -> Result<(), String> {
    if mode_preset(mode_id).is_some() {
        return Ok(());
    }
    Err(format!(
        "Invalid Codex mode \"{mode_id}\". Valid modes are: {}",
        MODE_PRESET_IDS.join(", ")
    ))
}

/// `CODEX_MODES`, filtered by the auto-review gate as `getAvailableModes`.
#[must_use]
pub fn available_modes(auto_review_enabled: bool) -> Value {
    let modes = [
        json!({
            "id": "auto",
            "label": "Default Permissions",
            "description": "Edit files and run commands with Codex's default approval flow.",
        }),
        json!({
            "id": "auto-review",
            "label": "Auto-review",
            "description": "Same workspace-write permissions as Default, but eligible `on-request` approvals are routed through the auto-reviewer subagent.",
        }),
        json!({
            "id": "full-access",
            "label": "Full Access",
            "description": "Edit files, run commands, and access the network without additional prompts.",
        }),
    ];
    Value::Array(
        modes
            .into_iter()
            .filter(|mode| auto_review_enabled || mode["id"] != "auto-review")
            .collect(),
    )
}

const FAST_MODE_MODELS: [&str; 9] = [
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
];

/// `codexModelSupportsFastMode`.
#[must_use]
pub fn model_supports_fast_mode(model: Option<&str>) -> bool {
    model
        .map(js_trim)
        .is_some_and(|model| !model.is_empty() && FAST_MODE_MODELS.contains(&model))
}

/// `composeSystemPromptParts`.
#[must_use]
pub fn compose_system_prompt_parts(parts: &[Option<&str>]) -> Option<String> {
    let joined = parts
        .iter()
        .flatten()
        .map(|part| js_trim(part))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn string_field<'a>(record: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    record
        .and_then(|record| record.get(key))
        .and_then(Value::as_str)
}

#[derive(Debug, Clone, PartialEq)]
struct CollaborationMode {
    name: String,
    mode: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    developer_instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedCollaborationMode {
    mode: String,
    settings: Map<String, Value>,
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Disconnected,
    /// Archived history was read through a short-lived app-server; no live
    /// client exists (`"history-ready"`).
    HistoryReady,
    Connected,
}

/// A persisted Codex session to resume (`AgentPersistenceHandle` subset).
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeHandle {
    pub session_id: String,
    pub metadata: Option<Map<String, Value>>,
}

/// A one-shot value other threads can wait for (a resolved JS promise).
struct Slot<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T: Clone> Slot<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn resolve(&self, value: T) {
        let mut slot = lock(&self.value);
        if slot.is_none() {
            *slot = Some(value);
            self.ready.notify_all();
        }
    }

    fn wait(&self) -> T {
        let mut slot = lock(&self.value);
        loop {
            if let Some(value) = slot.as_ref() {
                return value.clone();
            }
            slot = self
                .ready
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

struct PendingIdentification {
    foreground_turn_id: String,
    slot: Arc<Slot<Option<String>>>,
}

struct PendingStart {
    cancel_requested: bool,
    done: Arc<Slot<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionKind {
    Command,
    File,
}

/// A permission request waiting on the user (`pendingPermissions` plus
/// `pendingPermissionHandlers`).
struct PendingPermission {
    id: String,
    request: Value,
    kind: PermissionKind,
    responder: Responder,
}

struct AsyncQuestionRecord {
    item: AsyncQuestionItem,
    resolution: Option<AsyncQuestionResolution>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// Mirrors the independent flags of Paseo's session class one to one.
#[allow(clippy::struct_excessive_bools)]
struct State {
    config: SessionConfig,
    /// `this.providerOptions`: the parsed `config.providerOptions`, in the
    /// schema's key order. `config.provider_options` stays as given.
    provider_options: Map<String, Value>,
    current_mode: String,
    has_workflow_mode_override: bool,
    service_tier_fast: bool,
    plan_mode_enabled: bool,
    resolved_workspace_write: Option<Map<String, Value>>,
    resolved_sandbox_policy: Option<Map<String, Value>>,
    current_thread_id: Option<String>,
    current_turn_id: Option<String>,
    pending_identification: Option<PendingIdentification>,
    pending_start: Option<PendingStart>,
    client: Option<AppServerClient>,
    next_turn_ordinal: u64,
    active_foreground_turn_id: Option<String>,
    active_client_message_id: Option<String>,
    cached_runtime_info: Option<Value>,
    pending_agent_messages: HashMap<String, String>,
    pending_reasoning: HashMap<String, Vec<String>>,
    pending_assistant_message_boundary: bool,
    emitted_item_started_ids: HashSet<String>,
    emitted_item_completed_ids: HashSet<String>,
    latest_usage: Option<Value>,
    latest_plan_result: Option<String>,
    user_message_turn_ids: Vec<String>,
    user_message_provider_turn_ids: HashMap<String, String>,
    compaction_trigger_by_item_id: HashMap<String, &'static str>,
    pending_root_compaction_item_ids: Vec<String>,
    pending_anonymous_root_compactions: u64,
    unpaired_compaction_notification_completions: u64,
    unpaired_compaction_item_completions: u64,
    async_questions: Vec<AsyncQuestionRecord>,
    pending_permissions: Vec<PendingPermission>,
    emitted_exec_started_call_ids: HashSet<String>,
    emitted_exec_completed_call_ids: HashSet<String>,
    pending_command_output_deltas: HashMap<String, Vec<String>>,
    pending_file_change_output_deltas: HashMap<String, Vec<String>>,
    connection: ConnectionState,
    connecting: bool,
    connect_error: Option<String>,
    closed: bool,
    collaboration_modes: Vec<CollaborationMode>,
    resolved_collaboration_mode: Option<ResolvedCollaborationMode>,
    unported: UnportedLog,
    history_only: bool,
    history_pending: bool,
    persisted_history: Vec<HistoryEntry>,
}

/// Unported paths seen so far, each recorded once in first-seen order. Every
/// record is a fixed kind plus, at most, a detail drawn from a finite set
/// (a parsed notification kind, a known item type, a server request method
/// Paseo handles), never a raw value from Codex, so the log is bounded.
#[derive(Default)]
struct UnportedLog(Vec<String>);

impl UnportedLog {
    fn push(&mut self, what: String) {
        if !self.0.contains(&what) {
            self.0.push(what);
        }
    }
}

/// Work for the event dispatch thread. The queue is unbounded on purpose:
/// publishers hold the state lock, so backpressure would deadlock a
/// subscriber that calls back into the session while the queue is full.
enum Dispatch {
    Events(Vec<Value>),
    /// Answered once every earlier event was delivered.
    Barrier(mpsc::Sender<()>),
}

type Subscribers = Arc<Mutex<Vec<(u64, Subscriber)>>>;

struct Inner {
    state: Mutex<State>,
    state_changed: Condvar,
    subscribers: Subscribers,
    /// Events go to subscribers on a dedicated thread, in production order,
    /// so a subscriber may call back into the session (`start_turn`,
    /// `interrupt`) without blocking the stdout reader it would wait on.
    dispatch: Mutex<Option<mpsc::Sender<Dispatch>>>,
    dispatch_thread: thread::ThreadId,
    next_subscriber: AtomicU64,
    spawn: SpawnAppServer,
    custom_codex_config: Option<Map<String, Value>>,
    ephemeral: bool,
    gates: CodexGates,
}

impl Drop for Inner {
    /// A session dropped without `close()` still stops its app-server, so a
    /// panicking caller never leaks a Codex process.
    fn drop(&mut self) {
        self.dispatch
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let client = self
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .client
            .take();
        if let Some(client) = client {
            let _ = client.dispose();
        }
    }
}

/// One Codex agent session over a `codex app-server` child.
#[derive(Clone)]
pub struct CodexSession {
    inner: Arc<Inner>,
}

/// Construction inputs for [`CodexSession::new`].
pub struct SessionOptions {
    pub config: SessionConfig,
    pub spawn: SpawnAppServer,
    pub custom_codex_config: Option<Map<String, Value>>,
    pub ephemeral: bool,
    pub gates: CodexGates,
}

impl CodexSession {
    /// The session constructor: validates the mode and derives the fast
    /// service tier and plan mode from feature values.
    ///
    /// # Errors
    /// Returns the `Invalid Codex mode` message for an unknown `mode_id`, or
    /// the `ZodError` message for `provider_options` outside the pinned schema.
    pub fn new(options: SessionOptions) -> Result<Self, String> {
        let mut config = options.config;
        if let Some(mode_id) = &config.mode_id {
            validate_mode(mode_id)?;
        }
        let provider_options = parse_provider_options(config.provider_options.as_ref())?;
        let has_workflow_mode_override = config.mode_id.is_some();
        let current_mode = config
            .mode_id
            .clone()
            .unwrap_or_else(|| DEFAULT_CODEX_MODE_ID.to_owned());
        config.thinking_option_id = normalize_thinking(config.thinking_option_id.as_deref());
        let feature = |key: &str| {
            config
                .feature_values
                .as_ref()
                .and_then(|values| values.get(key))
                .is_some_and(js_truthy)
        };
        let service_tier_fast =
            feature("fast_mode") && model_supports_fast_mode(config.model.as_deref());
        let plan_mode_enabled = feature("plan_mode");
        let mut unported = UnportedLog::default();
        if plan_mode_enabled {
            unported.push("plan_mode feature".to_owned());
        }
        let state = State {
            config,
            provider_options,
            current_mode,
            has_workflow_mode_override,
            service_tier_fast,
            plan_mode_enabled,
            resolved_workspace_write: None,
            resolved_sandbox_policy: None,
            current_thread_id: None,
            current_turn_id: None,
            pending_identification: None,
            pending_start: None,
            client: None,
            next_turn_ordinal: 0,
            active_foreground_turn_id: None,
            active_client_message_id: None,
            cached_runtime_info: None,
            pending_agent_messages: HashMap::new(),
            pending_reasoning: HashMap::new(),
            pending_assistant_message_boundary: false,
            emitted_item_started_ids: HashSet::new(),
            emitted_item_completed_ids: HashSet::new(),
            latest_usage: None,
            latest_plan_result: None,
            user_message_turn_ids: Vec::new(),
            user_message_provider_turn_ids: HashMap::new(),
            compaction_trigger_by_item_id: HashMap::new(),
            pending_root_compaction_item_ids: Vec::new(),
            pending_anonymous_root_compactions: 0,
            unpaired_compaction_notification_completions: 0,
            unpaired_compaction_item_completions: 0,
            async_questions: Vec::new(),
            pending_permissions: Vec::new(),
            emitted_exec_started_call_ids: HashSet::new(),
            emitted_exec_completed_call_ids: HashSet::new(),
            pending_command_output_deltas: HashMap::new(),
            pending_file_change_output_deltas: HashMap::new(),
            connection: ConnectionState::Disconnected,
            connecting: false,
            connect_error: None,
            closed: false,
            collaboration_modes: Vec::new(),
            resolved_collaboration_mode: None,
            unported,
            history_only: false,
            history_pending: false,
            persisted_history: Vec::new(),
        };
        let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
        let (dispatch, dispatch_thread) = spawn_dispatcher(Arc::clone(&subscribers));
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                state_changed: Condvar::new(),
                subscribers,
                dispatch: Mutex::new(Some(dispatch)),
                dispatch_thread,
                next_subscriber: AtomicU64::new(0),
                spawn: options.spawn,
                custom_codex_config: options.custom_codex_config,
                ephemeral: options.ephemeral,
                gates: options.gates,
            }),
        })
    }

    /// The session constructor for `resumeSession`: starts on the persisted
    /// thread with its history pending, restoring saved async questions.
    /// `history_only` is Paseo's `purpose: "history"`, which reads archived
    /// history without resuming the native thread.
    ///
    /// # Errors
    /// Returns the `Invalid Codex mode` message for an unknown `mode_id`.
    pub fn resumed(
        options: SessionOptions,
        handle: &ResumeHandle,
        history_only: bool,
    ) -> Result<Self, String> {
        let session = Self::new(options)?;
        {
            let mut state = lock(&session.inner.state);
            // `if (this.resumeHandle?.sessionId)`: an empty id is falsy.
            if !handle.session_id.is_empty() {
                state.current_thread_id = Some(handle.session_id.clone());
                state.history_pending = true;
            }
            state.history_only = history_only;
            state.async_questions = saved_async_questions(
                handle
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("asyncQuestions")),
            );
        }
        Ok(session)
    }

    /// `streamHistory()`: the replayed timeline once, as timeline events
    /// without a turn id.
    #[must_use]
    pub fn stream_history(&self) -> Vec<Value> {
        let mut state = lock(&self.inner.state);
        if !state.history_pending || state.persisted_history.is_empty() {
            return Vec::new();
        }
        state.history_pending = false;
        std::mem::take(&mut state.persisted_history)
            .into_iter()
            .map(|entry| {
                let mut event = timeline_event(entry.item);
                if let Some(timestamp) = entry.timestamp {
                    event.insert("timestamp".to_owned(), json!(timestamp));
                }
                Value::Object(event)
            })
            .collect()
    }

    /// Registers a stream event subscriber; returns its id. A subscriber that
    /// captures a clone of this session keeps it alive until `unsubscribe` or
    /// `close`, which clears every subscriber and so breaks the cycle.
    pub fn subscribe(&self, subscriber: Subscriber) -> u64 {
        let id = self.inner.next_subscriber.fetch_add(1, Ordering::SeqCst);
        lock(&self.inner.subscribers).push((id, subscriber));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        lock(&self.inner.subscribers).retain(|(existing, _)| *existing != id);
    }

    /// The native Codex thread id (`session.id`).
    #[must_use]
    pub fn id(&self) -> Option<String> {
        lock(&self.inner.state).current_thread_id.clone()
    }

    /// Paths Paseo renders that this port does not, in arrival order.
    #[must_use]
    pub fn unported(&self) -> Vec<String> {
        lock(&self.inner.state).unported.0.clone()
    }

    #[must_use]
    pub fn current_mode(&self) -> String {
        lock(&self.inner.state).current_mode.clone()
    }

    #[must_use]
    pub fn available_modes(&self) -> Value {
        available_modes(self.inner.gates.auto_review_enabled)
    }

    /// `get features()` via `buildCodexFeatures`.
    #[must_use]
    pub fn features(&self) -> Value {
        let state = lock(&self.inner.state);
        let mut features = Vec::new();
        if model_supports_fast_mode(state.config.model.as_deref()) {
            features.push(json!({
                "type": "toggle",
                "id": "fast_mode",
                "label": "Fast",
                "description": "Priority inference at increased usage",
                "tooltip": "Toggle fast mode",
                "icon": "zap",
                "value": state.service_tier_fast,
            }));
        }
        if find_collaboration_mode(&state.collaboration_modes, true).is_some() {
            features.push(json!({
                "type": "toggle",
                "id": "plan_mode",
                "label": "Plan",
                "description": "Switch Codex into planning-only collaboration mode",
                "tooltip": "Toggle plan mode",
                "icon": "list-todo",
                "value": state.plan_mode_enabled,
            }));
        }
        Value::Array(features)
    }

    /// `getPendingPermissions()`: tool requests, then pending async questions.
    #[must_use]
    pub fn pending_permissions(&self) -> Vec<Value> {
        let state = lock(&self.inner.state);
        state
            .pending_permissions
            .iter()
            .map(|pending| pending.request.clone())
            .chain(
                state
                    .async_questions
                    .iter()
                    .filter(|record| record.resolution.is_none())
                    .map(|record| async_question_permission(&record.item)),
            )
            .collect()
    }

    /// `respondToPermission(requestId, response)` for command and file
    /// change approvals. `response` is an `AgentPermissionResponse`.
    ///
    /// # Errors
    /// Returns Paseo's `No pending Codex app-server permission request` error
    /// for an unknown id; async question answers are not ported.
    pub fn respond_to_permission(&self, request_id: &str, response: &Value) -> Result<(), String> {
        let mut events = Vec::new();
        let pending = {
            let mut state = lock(&self.inner.state);
            if state.async_questions.iter().any(|record| {
                record.resolution.is_none()
                    && format!("permission-{}", record.item.id) == request_id
            }) {
                state.unported.push("async question response".to_owned());
                return Err("Codex async question responses are not ported".to_owned());
            }
            let Some(position) = state
                .pending_permissions
                .iter()
                .position(|pending| pending.id == request_id)
            else {
                return Err(format!(
                    "No pending Codex app-server permission request with id '{request_id}'"
                ));
            };
            let pending = state.pending_permissions.remove(position);
            let denied = response.get("behavior").and_then(Value::as_str) == Some("deny");
            if denied && pending.request.get("kind").and_then(Value::as_str) == Some("tool") {
                let item = denied_tool_call_item(request_id, response, &pending.request);
                emit(&state, &mut events, timeline_event(item));
            }
            emit(
                &state,
                &mut events,
                event(&[
                    ("type", json!("permission_resolved")),
                    ("requestId", json!(request_id)),
                    ("resolution", response.clone()),
                ]),
            );
            self.publish(&events);
            pending
        };
        match pending.kind {
            PermissionKind::Command | PermissionKind::File => pending
                .responder
                .respond(Ok(Some(json!({"decision": permission_decision(response)})))),
        }
        Ok(())
    }

    /// `describePersistence()`.
    #[must_use]
    pub fn describe_persistence(&self) -> Option<Value> {
        let state = lock(&self.inner.state);
        let thread_id = state.current_thread_id.clone()?;
        let config = &state.config;
        let mut metadata = Map::new();
        metadata.insert("provider".to_owned(), json!(CODEX_PROVIDER));
        metadata.insert("cwd".to_owned(), json!(config.cwd));
        metadata.insert(
            "title".to_owned(),
            config.title.clone().unwrap_or(Value::Null),
        );
        metadata.insert("threadId".to_owned(), json!(thread_id));
        if let Some(mode_id) = &config.mode_id {
            metadata.insert("modeId".to_owned(), json!(mode_id));
        }
        metadata.insert("model".to_owned(), json!(config.model));
        metadata.insert(
            "thinkingOptionId".to_owned(),
            json!(normalize_thinking(config.thinking_option_id.as_deref())),
        );
        if let Some(options) = &config.provider_options {
            metadata.insert("providerOptions".to_owned(), options.clone());
        }
        if let Some(policy) = &config.tool_policy {
            metadata.insert("toolPolicy".to_owned(), policy.clone());
        }
        if let Some(prompt) = &config.system_prompt {
            metadata.insert("systemPrompt".to_owned(), json!(prompt));
        }
        if let Some(servers) = &config.mcp_servers {
            metadata.insert("mcpServers".to_owned(), Value::Object(servers.clone()));
        }
        let questions: Vec<Value> = state
            .async_questions
            .iter()
            .map(|record| async_question_record(&record.item, record.resolution.as_ref()))
            .collect();
        metadata.insert("asyncQuestions".to_owned(), Value::Array(questions));
        Some(json!({
            "provider": CODEX_PROVIDER,
            "sessionId": thread_id,
            "nativeHandle": thread_id,
            "metadata": metadata,
        }))
    }

    /// `connect()`: spawn and initialize the app-server once.
    ///
    /// # Errors
    /// Returns the closed-session error or the first connection failure.
    pub fn connect(&self) -> Result<(), String> {
        {
            let mut state = lock(&self.inner.state);
            if state.closed {
                return Err(CLOSED_MESSAGE.to_owned());
            }
            if state.connection != ConnectionState::Disconnected {
                return Ok(());
            }
            if state.connecting {
                while state.connecting {
                    state = self.wait_state(state);
                }
                if state.closed {
                    return Err(CLOSED_MESSAGE.to_owned());
                }
                return match &state.connect_error {
                    Some(error) if state.connection == ConnectionState::Disconnected => {
                        Err(error.clone())
                    }
                    _ => Ok(()),
                };
            }
            state.connecting = true;
            state.connect_error = None;
        }
        let outcome = self.establish_connection();
        let mut state = lock(&self.inner.state);
        state.connecting = false;
        if let Err(error) = &outcome {
            state.connect_error = Some(error.clone());
        }
        self.inner.state_changed.notify_all();
        outcome?;
        if state.closed {
            return Err(CLOSED_MESSAGE.to_owned());
        }
        Ok(())
    }

    fn wait_state<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.inner
            .state_changed
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn establish_connection(&self) -> Result<(), String> {
        if lock(&self.inner.state).history_only {
            self.read_archived_history()?;
            lock(&self.inner.state).connection = ConnectionState::HistoryReady;
            return Ok(());
        }
        let child = (self.inner.spawn)()?;
        let client = AppServerClient::new(child).map_err(|error| error.message)?;
        if lock(&self.inner.state).closed {
            let _ = client.dispose();
            return Err(CLOSED_MESSAGE.to_owned());
        }
        lock(&self.inner.state).client = Some(client.clone());
        let weak = Arc::downgrade(&self.inner);
        client.set_termination_handler(Box::new(move |error| {
            if let Some(session) = upgrade(&weak) {
                session.handle_unexpected_termination(&error);
            }
        }));
        let weak = Arc::downgrade(&self.inner);
        client.set_notification_handler(Arc::new(move |method, params| {
            if let Some(session) = upgrade(&weak) {
                session.handle_notification(method, params.as_ref());
            }
        }));
        self.register_request_handlers(&client);

        let outcome = self.initialize(&client);
        if let Err(error) = outcome {
            let is_current = lock(&self.inner.state)
                .client
                .as_ref()
                .is_some_and(|current| current.pid() == client.pid());
            if is_current {
                let _ = self.dispose_client();
            } else {
                let _ = client.dispose();
            }
            return Err(error);
        }
        Ok(())
    }

    fn initialize(&self, client: &AppServerClient) -> Result<(), String> {
        client
            .request(
                "initialize",
                Some(launch::initialize_params()),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map_err(|error| error.message)?;
        client.notify("initialized", Some(json!({})));
        self.load_resolved_workspace_write(client);
        self.load_collaboration_modes(client);
        self.load_skills(client);
        if lock(&self.inner.state).current_thread_id.is_some() {
            self.ensure_thread_loaded(client)?;
            self.load_persisted_history(client)?;
            self.apply_default_model_and_thinking(client)?;
        }
        let mut state = lock(&self.inner.state);
        if state.closed {
            return Err(CLOSED_MESSAGE.to_owned());
        }
        state.connection = ConnectionState::Connected;
        Ok(())
    }

    /// `readArchivedHistory()`: a short-lived app-server reads the thread and
    /// is disposed whatever the outcome.
    fn read_archived_history(&self) -> Result<(), String> {
        let child = (self.inner.spawn)()?;
        let client = AppServerClient::new(child).map_err(|error| error.message)?;
        let outcome = client
            .request(
                "initialize",
                Some(launch::initialize_params()),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map_err(|error| error.message)
            .and_then(|_| {
                client.notify("initialized", Some(json!({})));
                self.load_persisted_history(&client)
            });
        let disposed = client.dispose().map_err(|error| error.message);
        outcome?;
        disposed
    }

    /// `loadPersistedHistory(client)`: `thread/read` with turns, projected to
    /// the timeline the session replays.
    fn load_persisted_history(&self, client: &AppServerClient) -> Result<(), String> {
        let Some(thread_id) = lock(&self.inner.state).current_thread_id.clone() else {
            return Ok(());
        };
        let response = client
            .request(
                "thread/read",
                Some(json!({"threadId": thread_id, "includeTurns": true})),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map_err(|error| error.message)?;
        let projection = project_thread_history(&response)?;
        let mut state = lock(&self.inner.state);
        for what in projection.unported {
            state.unported.push(what);
        }
        state.user_message_turn_ids.clear();
        state.user_message_provider_turn_ids.clear();
        let mut timeline = projection.timeline;
        for entry in &mut timeline {
            if entry.item["type"] == "tool_call" && entry.item["name"] == "request_user_input_async"
            {
                let call_id = entry.item["callId"].as_str().unwrap_or_default().to_owned();
                if let Some(record) = state
                    .async_questions
                    .iter()
                    .find(|record| record.item.id == call_id)
                {
                    entry.item = async_question_timeline(&record.item, record.resolution.as_ref());
                }
            }
            if entry.item["type"] == "user_message" {
                let message_id = entry.item["messageId"].as_str().map(str::to_owned);
                remember_user_message_turn(
                    &mut state,
                    message_id.as_deref(),
                    entry.provider_turn_id.as_deref(),
                );
            }
        }
        state.history_pending = !timeline.is_empty();
        state.persisted_history = timeline;
        Ok(())
    }

    fn register_request_handlers(&self, client: &AppServerClient) {
        for (method, kind) in [
            (
                "item/commandExecution/requestApproval",
                PermissionKind::Command,
            ),
            ("item/fileChange/requestApproval", PermissionKind::File),
        ] {
            let weak = Arc::downgrade(&self.inner);
            client.set_request_handler(
                method,
                Arc::new(move |params, _id, responder| {
                    if let Some(session) = upgrade(&weak) {
                        session.handle_approval_request(kind, params.as_ref(), responder);
                    }
                }),
            );
        }
        for method in UNPORTED_REQUEST_METHODS {
            let weak = Arc::downgrade(&self.inner);
            client.set_request_handler(
                method,
                Arc::new(move |params, _id, responder| {
                    if method == "mcpServer/elicitation/request"
                        && let Some(reply) = paseo_declined_elicitation(params.as_ref())
                    {
                        // Paseo itself declines these at once: parity.
                        responder.respond(Ok(Some(reply)));
                        return;
                    }
                    // TEMPORARY guard, not parity: Paseo shows these as
                    // questions or MCP approvals and waits for the user. Until
                    // that flow is ported, decline at once so Codex never waits
                    // forever, and record the divergence.
                    responder.respond(Ok(Some(unported_request_reply(method))));
                    if let Some(session) = upgrade(&weak) {
                        session.record_unported(format!("server request {method}"));
                    }
                }),
            );
        }
    }

    /// `handleCommandApprovalRequest` and `handleFileChangeApprovalRequest`.
    fn handle_approval_request(
        &self,
        kind: PermissionKind,
        params: Option<&Value>,
        responder: Responder,
    ) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            let Some(request) = approval_request(kind, params, &state.config.cwd) else {
                state
                    .unported
                    .push("invalid approval request params".to_owned());
                drop(state);
                responder.respond(Err("Invalid Codex approval request params".to_owned()));
                return;
            };
            let id = request["id"].as_str().unwrap_or_default().to_owned();
            emit(
                &state,
                &mut events,
                event(&[
                    ("type", json!("permission_requested")),
                    ("request", request.clone()),
                ]),
            );
            let pending = PendingPermission {
                id,
                request,
                kind,
                responder,
            };
            // Paseo keys pending permissions by id in a Map: a repeated id
            // keeps its position and takes the new request and handler. The
            // replaced responder answers Codex with an error when dropped.
            match state
                .pending_permissions
                .iter_mut()
                .find(|known| known.id == pending.id)
            {
                Some(known) => *known = pending,
                None => state.pending_permissions.push(pending),
            }
            self.publish(&events);
        }
    }

    fn record_unported(&self, what: String) {
        lock(&self.inner.state).unported.push(what);
    }

    fn load_resolved_workspace_write(&self, client: &AppServerClient) {
        let cwd = json!(lock(&self.inner.state).config.cwd);
        let Ok(response) = client.request(
            "config/read",
            Some(json!({"cwd": cwd})),
            DEFAULT_REQUEST_TIMEOUT,
        ) else {
            return;
        };
        let config = object(&response).and_then(|response| response.get("config"));
        let workspace_write = config
            .and_then(object)
            .and_then(|config| config.get("sandbox_workspace_write"))
            .and_then(read_sandbox_workspace_write);
        lock(&self.inner.state).resolved_workspace_write = workspace_write;
    }

    fn load_collaboration_modes(&self, client: &AppServerClient) {
        let modes = client
            .request(
                "collaborationMode/list",
                Some(json!({})),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .ok()
            .map(|response| parse_collaboration_modes(&response))
            .unwrap_or_default();
        let mut state = lock(&self.inner.state);
        state.collaboration_modes = modes;
        refresh_collaboration_mode(&mut state);
    }

    fn load_skills(&self, client: &AppServerClient) {
        let cwd = lock(&self.inner.state).config.cwd.clone();
        // Skills only feed slash-command resolution, which is not ported; the
        // request is still made so Codex sees the same sequence.
        let _ = client.request(
            "skills/list",
            Some(json!({"cwds": [cwd]})),
            DEFAULT_REQUEST_TIMEOUT,
        );
    }

    fn client(&self) -> Result<AppServerClient, String> {
        lock(&self.inner.state)
            .client
            .clone()
            .ok_or_else(|| "Codex client not initialized".to_owned())
    }

    /// `startTurn(prompt, options)`: returns the foreground turn id.
    ///
    /// # Errors
    /// Returns Paseo's errors for an active turn, connect failure, an
    /// interrupted start, or a rejected `turn/start`.
    pub fn start_turn(&self, prompt: &Prompt, options: &RunOptions) -> Result<String, String> {
        let done = Slot::new();
        {
            let mut state = lock(&self.inner.state);
            if state.active_foreground_turn_id.is_some() || state.pending_start.is_some() {
                return Err("A foreground turn is already active".to_owned());
            }
            state.pending_start = Some(PendingStart {
                cancel_requested: false,
                done: Arc::clone(&done),
            });
        }
        let outcome = self.start_turn_inner(prompt, options);
        let mut state = lock(&self.inner.state);
        if outcome.is_err() {
            if let Some(pending) = state.pending_identification.take() {
                pending.slot.resolve(None);
            }
            state.active_foreground_turn_id = None;
            state.active_client_message_id = None;
        }
        if state
            .pending_start
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.done, &done))
        {
            state.pending_start = None;
        }
        drop(state);
        done.resolve(());
        outcome
    }

    /// `steerActiveTurn(prompt, options)`: adds input to the running
    /// foreground turn (`turn/steer`). `Unavailable` when no turn matching
    /// `options.expected_turn_id` is running, or when Codex definitively
    /// rejects the steer ([`is_definitive_steer_rejection`]); any other
    /// failure is an error.
    ///
    /// # Errors
    /// Returns an unported prompt, an invalid acknowledgement, a failure to
    /// answer a pending approval, or an ambiguous `turn/steer` failure
    /// (timeout, disconnect, other Codex error).
    pub fn steer_active_turn(
        &self,
        prompt: &Prompt,
        options: &SteerOptions,
    ) -> Result<SteerResult, String> {
        let Some(admission) = self.steer_admission(options) else {
            return Ok(SteerResult::Unavailable);
        };
        reject_slash_command(prompt)?;
        if !self.matches_steer_admission(&admission) {
            return Ok(SteerResult::Unavailable);
        }
        let input = build_user_input(prompt)?;
        if !self.matches_steer_admission(&admission) {
            return Ok(SteerResult::Unavailable);
        }
        let mut params = Map::new();
        params.insert("threadId".to_owned(), json!(admission.thread_id));
        params.insert("expectedTurnId".to_owned(), json!(admission.native_turn_id));
        params.insert("input".to_owned(), input);
        if let Some(id) = options
            .client_message_id
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            params.insert("clientUserMessageId".to_owned(), json!(id));
        }
        let response = match admission.client.request(
            "turn/steer",
            Some(Value::Object(params)),
            TURN_START_TIMEOUT,
        ) {
            Ok(response) => response,
            Err(error) if is_definitive_steer_rejection(&error) => {
                return Ok(SteerResult::Unavailable);
            }
            Err(error) => return Err(error.message),
        };
        let record = object(&response);
        let turn = record
            .and_then(|record| record.get("turn"))
            .and_then(object);
        let acknowledged = non_empty_string(record.and_then(|record| record.get("turnId")))
            .or_else(|| non_empty_string(turn.and_then(|turn| turn.get("id"))));
        if acknowledged != Some(admission.native_turn_id.as_str()) {
            return Err("Codex returned an invalid steer acknowledgement".to_owned());
        }
        if options.clear_pending_permissions {
            self.clear_pending_permissions_for_steer()?;
        }
        Ok(SteerResult::Accepted)
    }

    /// The client, thread, native turn, and foreground turn a steer is
    /// admitted against, or `None` when no turn matching `expected_turn_id`
    /// is running.
    fn steer_admission(&self, options: &SteerOptions) -> Option<SteerAdmission> {
        let state = lock(&self.inner.state);
        let client = state.client.clone()?;
        let thread_id = state
            .current_thread_id
            .clone()
            .filter(|id| !id.is_empty())?;
        let native_turn_id = state.current_turn_id.clone().filter(|id| !id.is_empty())?;
        let foreground = state.active_foreground_turn_id.clone()?;
        (foreground == options.expected_turn_id).then_some(SteerAdmission {
            client,
            thread_id,
            native_turn_id,
            foreground,
        })
    }

    /// `matchesSteerAdmission`: the same client (by its child's pid), thread,
    /// native turn, and foreground turn.
    fn matches_steer_admission(&self, admission: &SteerAdmission) -> bool {
        let state = lock(&self.inner.state);
        state
            .client
            .as_ref()
            .is_some_and(|client| client.pid() == admission.client.pid())
            && state.current_thread_id.as_deref() == Some(admission.thread_id.as_str())
            && state.current_turn_id.as_deref() == Some(admission.native_turn_id.as_str())
            && state.active_foreground_turn_id.as_deref() == Some(admission.foreground.as_str())
    }

    /// `clearPendingPermissionsForSteer()`: denies every pending approval
    /// with the message of a user who answered with a message instead.
    fn clear_pending_permissions_for_steer(&self) -> Result<(), String> {
        let ids: Vec<String> = lock(&self.inner.state)
            .pending_permissions
            .iter()
            .map(|pending| pending.id.clone())
            .collect();
        for id in ids {
            let still_pending = lock(&self.inner.state)
                .pending_permissions
                .iter()
                .any(|pending| pending.id == id);
            if !still_pending {
                continue;
            }
            self.respond_to_permission(
                &id,
                &json!({
                    "behavior": "deny",
                    "message": "The user answered with a message instead of approving. Their message follows.",
                }),
            )?;
        }
        Ok(())
    }

    fn start_turn_inner(&self, prompt: &Prompt, options: &RunOptions) -> Result<String, String> {
        self.connect()?;
        let client = self.client()?;
        reject_slash_command(prompt)?;
        if lock(&self.inner.state).current_thread_id.is_some() {
            self.ensure_thread_loaded(&client)?;
        } else {
            self.ensure_thread(&client)?;
        }
        let input = build_user_input(prompt)?;
        let params;
        let turn_id;
        {
            let mut state = lock(&self.inner.state);
            params = self.build_turn_start_params(&state, input);
            turn_id = format!("codex-turn-{}", state.next_turn_ordinal);
            state.next_turn_ordinal += 1;
            state.active_foreground_turn_id = Some(turn_id.clone());
            state
                .active_client_message_id
                .clone_from(&options.client_message_id);
            state.current_turn_id = None;
            if let Some(previous) = state.pending_identification.take() {
                previous.slot.resolve(None);
            }
            state.pending_identification = Some(PendingIdentification {
                foreground_turn_id: turn_id.clone(),
                slot: Slot::new(),
            });
            if state
                .pending_start
                .as_ref()
                .is_some_and(|pending| pending.cancel_requested)
            {
                return Err("Codex turn start was interrupted before reaching Codex".to_owned());
            }
        }
        client
            .request("turn/start", Some(params), TURN_START_TIMEOUT)
            .map_err(|error| error.message)?;
        Ok(turn_id)
    }

    /// `ensureThreadLoaded()`: confirm the thread is loaded in this
    /// app-server (`thread/loaded/list`), else `thread/resume` it, unarchiving
    /// an archived thread first.
    fn ensure_thread_loaded(&self, client: &AppServerClient) -> Result<(), String> {
        let (thread_id, params) = {
            let state = lock(&self.inner.state);
            let Some(thread_id) = state.current_thread_id.clone() else {
                return Ok(());
            };
            let mut params = Map::new();
            params.insert("threadId".to_owned(), json!(thread_id));
            if let Some(instructions) = Self::developer_instructions(&state) {
                params.insert("developerInstructions".to_owned(), json!(instructions));
            }
            if let Some(config) = self.build_codex_inner_config(&state) {
                params.insert("config".to_owned(), Value::Object(config));
            }
            (thread_id, Value::Object(params))
        };
        let attempt = (|| -> Result<(), ClientError> {
            let loaded = client.request(
                "thread/loaded/list",
                Some(json!({})),
                DEFAULT_REQUEST_TIMEOUT,
            )?;
            let is_loaded = object(&loaded)
                .and_then(|loaded| loaded.get("data"))
                .and_then(Value::as_array)
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(thread_id.as_str())));
            if is_loaded {
                return Ok(());
            }
            let response = client.request(
                "thread/resume",
                Some(params.clone()),
                DEFAULT_REQUEST_TIMEOUT,
            )?;
            remember_resolved_sandbox_policy(&mut lock(&self.inner.state), &response);
            Ok(())
        })();
        let Err(error) = attempt else {
            return Ok(());
        };
        let archived = format!(
            "session {thread_id} is archived. Run `codex unarchive {thread_id}` to unarchive it first."
        );
        if error.message != archived {
            return Err(format!(
                "Failed to resume Codex thread {thread_id}: {}",
                error.message
            ));
        }
        if let Err(unarchive_error) = client.request(
            "thread/unarchive",
            Some(json!({"threadId": thread_id})),
            DEFAULT_REQUEST_TIMEOUT,
        ) && !unarchive_error.message.contains(&format!(
            "no archived rollout found for thread id {thread_id}"
        )) {
            return Err(unarchive_error.message);
        }
        let response = client
            .request("thread/resume", Some(params), DEFAULT_REQUEST_TIMEOUT)
            .map_err(|error| error.message)?;
        remember_resolved_sandbox_policy(&mut lock(&self.inner.state), &response);
        Ok(())
    }

    /// `ensureThread()`: resolve model defaults and send `thread/start`.
    fn ensure_thread(&self, client: &AppServerClient) -> Result<(), String> {
        if lock(&self.inner.state).current_thread_id.is_some() {
            return Ok(());
        }
        let model = self.apply_default_model_and_thinking(client)?;
        let (params, approval_policy, sandbox) = {
            let state = lock(&self.inner.state);
            self.build_thread_start_request(&state, &model)
        };
        let raw = client
            .request("thread/start", Some(params), DEFAULT_REQUEST_TIMEOUT)
            .map_err(|error| error.message)?;
        let mut state = lock(&self.inner.state);
        remember_resolved_sandbox_policy(&mut state, &raw);
        let response = object(&raw);
        let thread_id = response
            .and_then(|response| response.get("thread"))
            .and_then(object)
            .and_then(|thread| thread.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Codex app-server did not return thread id".to_owned())?;
        let reviewer = string_field(response, "approvalsReviewer");
        let provider_option = |key: &str| state.provider_options.get(key);
        // `approvalPolicy ?? String(providerOptions.approval_policy ?? "")`:
        // the policy fallback is wrapped in `String()`, the sandbox is not.
        let approval_policy = approval_policy.unwrap_or_else(|| {
            provider_option("approval_policy")
                .map(js_string)
                .unwrap_or_default()
        });
        // `sandbox ?? providerOptions.sandbox_mode ?? ""` is compared raw
        // (`=== "workspace-write"`), so only the exact string matches.
        let sandbox_is_workspace_write =
            sandbox_is_workspace_write(sandbox.as_deref(), provider_option("sandbox_mode"));
        if should_promote_thread_response_to_auto_review(
            reviewer,
            &approval_policy,
            sandbox_is_workspace_write,
        ) {
            "auto-review".clone_into(&mut state.current_mode);
            state.cached_runtime_info = None;
        }
        state.current_thread_id = Some(thread_id);
        Ok(())
    }

    fn apply_default_model_and_thinking(&self, client: &AppServerClient) -> Result<String, String> {
        let (model, thinking) = {
            let state = lock(&self.inner.state);
            (
                state.config.model.clone(),
                normalize_thinking(state.config.thinking_option_id.as_deref()),
            )
        };
        let (model, thinking) = resolve_model_and_thinking(client, model, thinking)?;
        let mut state = lock(&self.inner.state);
        state.config.model = Some(model.clone());
        state.config.thinking_option_id = thinking;
        Ok(model)
    }

    fn build_codex_inner_config(&self, state: &State) -> Option<Map<String, Value>> {
        let mut inner = Map::new();
        for (key, value) in &state.provider_options {
            inner.insert(key.clone(), value.clone());
        }
        if let Some(custom) = &self.inner.custom_codex_config {
            for (key, value) in custom {
                inner.insert(key.clone(), value.clone());
            }
        }
        if let Some(servers) = &state.config.mcp_servers {
            let mut mcp = Map::new();
            for (name, server) in servers {
                mcp.insert(name.clone(), to_codex_mcp_config(server));
            }
            inner.insert("mcp_servers".to_owned(), Value::Object(mcp));
        }
        let configured = apply_codex_tool_policy(inner, state.config.tool_policy.as_ref());
        (!configured.is_empty()).then_some(configured)
    }

    fn developer_instructions(state: &State) -> Option<String> {
        compose_system_prompt_parts(&[
            state.config.system_prompt.as_deref(),
            state.config.daemon_append_system_prompt.as_deref(),
        ])
    }

    /// `buildThreadStartRequest(model)`.
    fn build_thread_start_request(
        &self,
        state: &State,
        model: &str,
    ) -> (Value, Option<String>, Option<String>) {
        let preset = mode_preset(&state.current_mode)
            .or_else(|| mode_preset(DEFAULT_CODEX_MODE_ID))
            .unwrap_or(ModePreset {
                approval_policy: "on-request",
                sandbox: "workspace-write",
                approvals_reviewer: "user",
            });
        let approval_policy = state
            .has_workflow_mode_override
            .then(|| preset.approval_policy.to_owned());
        let sandbox = state
            .has_workflow_mode_override
            .then(|| preset.sandbox.to_owned());
        let provider_has = |key: &str| state.provider_options.contains_key(key);
        let mut params = Map::new();
        params.insert("model".to_owned(), json!(model));
        params.insert("cwd".to_owned(), json!(state.config.cwd));
        if let Some(policy) = &approval_policy
            && !provider_has("approval_policy")
        {
            params.insert("approvalPolicy".to_owned(), json!(policy));
        }
        if let Some(sandbox) = &sandbox
            && !provider_has("sandbox_mode")
        {
            params.insert("sandbox".to_owned(), json!(sandbox));
        }
        if let Some(instructions) = Self::developer_instructions(state) {
            params.insert("developerInstructions".to_owned(), json!(instructions));
        }
        if let Some(config) = self.build_codex_inner_config(state) {
            params.insert("config".to_owned(), Value::Object(config));
        }
        if self.inner.ephemeral {
            params.insert("ephemeral".to_owned(), json!(true));
        }
        if state.has_workflow_mode_override {
            params.insert(
                "approvalsReviewer".to_owned(),
                json!(preset.approvals_reviewer),
            );
        }
        (Value::Object(params), approval_policy, sandbox)
    }

    /// `buildTurnStartParams(prompt)`.
    fn build_turn_start_params(&self, state: &State, input: Value) -> Value {
        let preset = mode_preset(&state.current_mode)
            .or_else(|| mode_preset(DEFAULT_CODEX_MODE_ID))
            .unwrap_or(ModePreset {
                approval_policy: "on-request",
                sandbox: "workspace-write",
                approvals_reviewer: "user",
            });
        let mut params = Map::new();
        params.insert(
            "threadId".to_owned(),
            json!(state.current_thread_id.clone()),
        );
        params.insert("input".to_owned(), input);
        let provider_option = |key: &str| state.provider_options.get(key);
        let approval_policy = state
            .has_workflow_mode_override
            .then_some(preset.approval_policy);
        let sandbox_type = provider_option("sandbox_mode")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                state
                    .has_workflow_mode_override
                    .then(|| preset.sandbox.to_owned())
            });
        if let Some(policy) = approval_policy
            && provider_option("approval_policy").is_none()
        {
            params.insert("approvalPolicy".to_owned(), json!(policy));
        }
        if let Some(sandbox_type) = &sandbox_type {
            let native = to_codex_sandbox_policy_type(sandbox_type);
            let policy = match &state.resolved_sandbox_policy {
                Some(resolved) if resolved.get("type").and_then(Value::as_str) == Some(native) => {
                    Value::Object(resolved.clone())
                }
                _ => {
                    let mut workspace_write =
                        state.resolved_workspace_write.clone().unwrap_or_default();
                    if let Some(Value::Object(overrides)) =
                        provider_option("sandbox_workspace_write")
                    {
                        for (key, value) in overrides {
                            workspace_write.insert(key.clone(), value.clone());
                        }
                    }
                    to_sandbox_policy(sandbox_type, &workspace_write)
                }
            };
            params.insert("sandboxPolicy".to_owned(), policy);
        }
        if state.has_workflow_mode_override {
            params.insert(
                "approvalsReviewer".to_owned(),
                json!(preset.approvals_reviewer),
            );
        }
        if let Some(model) = &state.config.model {
            params.insert("model".to_owned(), json!(model));
        }
        if let Some(effort) = normalize_thinking(state.config.thinking_option_id.as_deref()) {
            params.insert("effort".to_owned(), json!(effort));
        }
        if state.service_tier_fast {
            params.insert("serviceTier".to_owned(), json!("fast"));
        }
        if let Some(mode) = &state.resolved_collaboration_mode {
            params.insert(
                "collaborationMode".to_owned(),
                json!({"mode": mode.mode, "settings": mode.settings}),
            );
        }
        if !state.config.cwd.is_empty() {
            params.insert("cwd".to_owned(), json!(state.config.cwd));
        }
        if let Some(instructions) = Self::developer_instructions(state) {
            params.insert("developerInstructions".to_owned(), json!(instructions));
        }
        if let Some(config) = self.build_codex_inner_config(state) {
            params.insert("config".to_owned(), Value::Object(config));
        }
        Value::Object(params)
    }

    /// `getRuntimeInfo()`.
    ///
    /// # Errors
    /// Returns connect or thread creation failures.
    pub fn runtime_info(&self) -> Result<Value, String> {
        if let Some(info) = lock(&self.inner.state).cached_runtime_info.clone() {
            return Ok(info);
        }
        if lock(&self.inner.state).connection == ConnectionState::Disconnected {
            self.connect()?;
        }
        if lock(&self.inner.state).current_thread_id.is_none() {
            let client = self.client()?;
            self.ensure_thread(&client)?;
        }
        let mut state = lock(&self.inner.state);
        let mut info = Map::new();
        info.insert("provider".to_owned(), json!(CODEX_PROVIDER));
        info.insert("sessionId".to_owned(), json!(state.current_thread_id));
        info.insert("model".to_owned(), json!(state.config.model));
        info.insert(
            "thinkingOptionId".to_owned(),
            json!(normalize_thinking(
                state.config.thinking_option_id.as_deref()
            )),
        );
        info.insert("modeId".to_owned(), json!(state.current_mode));
        if let Some(mode) = &state.resolved_collaboration_mode {
            info.insert("extra".to_owned(), json!({"collaborationMode": mode.name}));
        }
        let info = Value::Object(info);
        state.cached_runtime_info = Some(info.clone());
        Ok(info)
    }

    /// `interrupt()`.
    ///
    /// # Errors
    /// Returns Paseo's errors when the turn cannot be identified or Codex
    /// rejects `turn/interrupt` for a reason other than an idle thread.
    pub fn interrupt(&self) -> Result<(), String> {
        let pending_start = {
            let mut state = lock(&self.inner.state);
            state.pending_start.as_mut().map(|pending| {
                pending.cancel_requested = true;
                Arc::clone(&pending.done)
            })
        };
        if let Some(done) = pending_start {
            done.wait();
        }
        let (client, thread_id, mut turn_id, foreground, identification) = {
            let state = lock(&self.inner.state);
            let (Some(client), Some(thread_id)) =
                (state.client.clone(), state.current_thread_id.clone())
            else {
                if state.active_foreground_turn_id.is_none()
                    && state.current_turn_id.is_none()
                    && state.pending_identification.is_none()
                {
                    return Ok(());
                }
                return Err(
                    "Cannot interrupt Codex before the active thread is initialized".to_owned(),
                );
            };
            let identification = state
                .pending_identification
                .as_ref()
                .filter(|pending| {
                    Some(&pending.foreground_turn_id) == state.active_foreground_turn_id.as_ref()
                })
                .map(|pending| Arc::clone(&pending.slot));
            (
                client,
                thread_id,
                state.current_turn_id.clone(),
                state.active_foreground_turn_id.clone(),
                identification,
            )
        };
        if turn_id.is_none()
            && foreground.is_some()
            && let Some(slot) = identification
        {
            turn_id = slot.wait();
        }
        {
            let state = lock(&self.inner.state);
            if turn_id.is_none()
                && state.active_foreground_turn_id.is_none()
                && state.current_turn_id.is_none()
            {
                return Ok(());
            }
            if turn_id.is_none()
                || (foreground.is_some() && state.active_foreground_turn_id != foreground)
            {
                return Err(
                    "Cannot interrupt Codex before turn/started identifies the active turn"
                        .to_owned(),
                );
            }
        }
        match client.request(
            "turn/interrupt",
            Some(json!({"threadId": thread_id, "turnId": turn_id})),
            INTERRUPT_TIMEOUT,
        ) {
            Ok(_) => Ok(()),
            Err(error) if is_already_idle_interrupt(&error) => {
                let mut state = lock(&self.inner.state);
                state.active_foreground_turn_id = None;
                state.active_client_message_id = None;
                state.current_turn_id = None;
                if let Some(pending) = state.pending_identification.take() {
                    pending.slot.resolve(None);
                }
                Ok(())
            }
            Err(error) => Err(error.message),
        }
    }

    /// `close()`.
    ///
    /// # Errors
    /// Returns the dispose failure when Codex survives SIGKILL.
    pub fn close(&self) -> Result<(), String> {
        let open_approvals = {
            let mut state = lock(&self.inner.state);
            state.closed = true;
            let open_approvals: Vec<PendingPermission> =
                state.pending_permissions.drain(..).collect();
            state.active_foreground_turn_id = None;
            state.active_client_message_id = None;
            if let Some(pending) = state.pending_identification.take() {
                pending.slot.resolve(None);
            }
            open_approvals
        };
        // Paseo delivered every earlier event synchronously before close. A
        // subscriber blocked in a session call (for example `start_turn`)
        // must not hold close up, so the wait is bounded.
        self.flush_dispatch(Some(CLOSE_FLUSH_TIMEOUT));
        lock(&self.inner.subscribers).clear();
        let outcome = self.dispose_client();
        // `clearPendingPermissions()` resolves each open approval with
        // `cancel` before `disposeClient()`, but the transport writes a
        // handler's reply only after an `await`, and by then `dispose()` has
        // marked the client disposed, so Codex never receives it. Answering
        // after the dispose reproduces that: the write is skipped.
        for pending in open_approvals {
            pending
                .responder
                .respond(Ok(Some(json!({"decision": "cancel"}))));
        }
        lock(&self.inner.state).current_thread_id = None;
        self.inner.state_changed.notify_all();
        outcome
    }

    fn dispose_client(&self) -> Result<(), String> {
        let client = {
            let mut state = lock(&self.inner.state);
            state.connection = ConnectionState::Disconnected;
            state.current_turn_id = None;
            state.client.take()
        };
        match client {
            Some(client) => client.dispose().map_err(|error| error.message),
            None => Ok(()),
        }
    }

    fn handle_unexpected_termination(&self, error: &ClientError) {
        let mut events = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            state.connection = ConnectionState::Disconnected;
            clear_pending_permissions(&mut state);
            let has_active_root_turn =
                state.active_foreground_turn_id.is_some() || state.current_turn_id.is_some();
            if has_active_root_turn {
                emit(
                    &state,
                    &mut events,
                    event(&[
                        ("type", json!("turn_failed")),
                        ("error", json!(error.message)),
                    ]),
                );
            }
            state.active_foreground_turn_id = None;
            state.active_client_message_id = None;
            state.current_turn_id = None;
            if let Some(pending) = state.pending_identification.take() {
                pending.slot.resolve(None);
            }
            self.publish(&events);
        }
    }

    /// Queues events for the dispatch thread. Callers publish while holding
    /// the state lock so events reach subscribers in production order.
    fn publish(&self, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        if let Some(dispatch) = lock(&self.inner.dispatch).as_ref() {
            let _ = dispatch.send(Dispatch::Events(events.to_vec()));
        }
    }

    /// Waits until every event published so far reached the subscribers.
    /// Returns at once on the dispatch thread itself, which cannot wait for
    /// its own queue.
    fn flush_dispatch(&self, timeout: Option<Duration>) {
        if thread::current().id() == self.inner.dispatch_thread {
            return;
        }
        let (done, delivered) = mpsc::channel();
        let sent = lock(&self.inner.dispatch)
            .as_ref()
            .is_some_and(|dispatch| dispatch.send(Dispatch::Barrier(done)).is_ok());
        if sent {
            match timeout {
                Some(timeout) => {
                    let _ = delivered.recv_timeout(timeout);
                }
                None => {
                    let _ = delivered.recv();
                }
            }
        }
    }

    /// `handleNotification(method, params)`.
    fn handle_notification(&self, method: &str, params: Option<&Value>) {
        if method == "serverRequest/resolved"
            && params
                .and_then(|params| params.get("requestId"))
                .is_some_and(Value::is_number)
        {
            // MCP elicitation permissions are not ported, so no request id
            // can match; Paseo returns here either way.
            return;
        }
        let parsed = parse_notification(method, params);
        let mut events = Vec::new();
        {
            let mut state = lock(&self.inner.state);
            let thread_id = parsed
                .thread_id()
                .filter(|thread| !thread.is_empty())
                .map(str::to_owned);
            let is_root = match (&thread_id, &state.current_thread_id) {
                (None, _) | (_, None) => true,
                (Some(thread), Some(current)) => thread == current,
            };
            if is_root {
                dispatch(&mut state, &mut events, parsed);
            } else {
                state.unported.push(format!(
                    "sub-agent thread notification {}",
                    parsed_kind_name(&parsed)
                ));
            }
            self.publish(&events);
        }
    }

    /// Feeds one notification through the session as the transport would,
    /// then waits for its events to reach subscribers.
    #[cfg(feature = "test-hooks")]
    pub fn receive_notification(&self, method: &str, params: Option<&Value>) {
        self.handle_notification(method, params);
        self.flush_dispatch(None);
    }

    /// Waits until every event published so far reached the subscribers.
    #[cfg(feature = "test-hooks")]
    pub fn flush_events(&self) {
        self.flush_dispatch(None);
    }

    /// Puts the session in the state Paseo's `createSession()` test fixture
    /// builds: connected, on `thread_id`, with an optional foreground turn.
    #[cfg(feature = "test-hooks")]
    pub fn prime_for_notification_test(&self, thread_id: &str, foreground_turn_id: Option<&str>) {
        let mut state = lock(&self.inner.state);
        state.connection = ConnectionState::Connected;
        state.current_thread_id = Some(thread_id.to_owned());
        state.active_foreground_turn_id = foreground_turn_id.map(str::to_owned);
    }

    /// Whether `item/fileChange/outputDelta` text is buffered for `call_id`.
    #[cfg(feature = "test-hooks")]
    #[must_use]
    pub fn has_buffered_file_change_output(&self, call_id: &str) -> bool {
        lock(&self.inner.state)
            .pending_file_change_output_deltas
            .contains_key(call_id)
    }

    /// Pid of the running `codex app-server` child, for process tests.
    #[cfg(feature = "test-hooks")]
    #[must_use]
    pub fn app_server_pid(&self) -> Option<u32> {
        lock(&self.inner.state)
            .client
            .as_ref()
            .map(AppServerClient::pid)
    }
}

/// Delivers queued events to subscribers until the session is dropped. A
/// panicking subscriber is isolated: other subscribers and later events
/// still run, as Paseo's `notifySubscribers` catches a throwing callback.
fn spawn_dispatcher(subscribers: Subscribers) -> (mpsc::Sender<Dispatch>, thread::ThreadId) {
    let (sender, receiver) = mpsc::channel::<Dispatch>();
    let handle = thread::spawn(move || {
        for work in receiver {
            match work {
                Dispatch::Events(events) => {
                    for event in &events {
                        let current: Vec<Subscriber> = lock(&subscribers)
                            .iter()
                            .map(|(_, subscriber)| Arc::clone(subscriber))
                            .collect();
                        for subscriber in current {
                            let _ = catch_unwind(AssertUnwindSafe(|| subscriber(event)));
                        }
                    }
                }
                Dispatch::Barrier(done) => {
                    let _ = done.send(());
                }
            }
        }
    });
    (sender, handle.thread().id())
}

/// The `ParsedCodexNotification` kind, a finite name for any notification.
fn parsed_kind_name(parsed: &ParsedNotification) -> &'static str {
    match parsed {
        ParsedNotification::ThreadStarted { .. } => "thread_started",
        ParsedNotification::TurnStarted { .. } => "turn_started",
        ParsedNotification::TurnCompleted { .. } => "turn_completed",
        ParsedNotification::PlanUpdated { .. } => "plan_updated",
        ParsedNotification::DiffUpdated { .. } => "diff_updated",
        ParsedNotification::TokenUsageUpdated { .. } => "token_usage_updated",
        ParsedNotification::AgentMessageDelta { .. } => "agent_message_delta",
        ParsedNotification::ReasoningDelta { .. } => "reasoning_delta",
        ParsedNotification::ItemCompleted { .. } => "item_completed",
        ParsedNotification::ItemStarted { .. } => "item_started",
        ParsedNotification::ExecCommandStarted { .. } => "exec_command_started",
        ParsedNotification::ExecCommandCompleted { .. } => "exec_command_completed",
        ParsedNotification::ExecCommandOutputDelta { .. } => "exec_command_output_delta",
        ParsedNotification::TerminalInteraction { .. } => "terminal_interaction",
        ParsedNotification::PatchApplyStarted { .. } => "patch_apply_started",
        ParsedNotification::PatchApplyCompleted { .. } => "patch_apply_completed",
        ParsedNotification::FileChangeOutputDelta { .. } => "file_change_output_delta",
        ParsedNotification::ThreadRolledBack { .. } => "thread_rolled_back",
        ParsedNotification::ContextCompacted { .. } => "context_compacted",
        ParsedNotification::InvalidPayload { .. } => "invalid_payload",
        ParsedNotification::UnknownMethod { .. } => "unknown_method",
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<CodexSession> {
    weak.upgrade().map(|inner| CodexSession { inner })
}

/// `(sandbox ?? providerOptions.sandbox_mode ?? "") === "workspace-write"`:
/// the preset sandbox if there is one, else the option, compared raw. Only
/// the exact string matches; an array, object, number or `null` never does
/// (no `String()` is applied here, unlike the approval policy).
fn sandbox_is_workspace_write(preset: Option<&str>, option: Option<&Value>) -> bool {
    match preset {
        Some(preset) => preset == "workspace-write",
        None => option.and_then(Value::as_str) == Some("workspace-write"),
    }
}

/// `shouldPromoteThreadResponseToAutoReview`: an auto-review reviewer on an
/// on-request policy in a workspace-write sandbox.
fn should_promote_thread_response_to_auto_review(
    reviewer: Option<&str>,
    approval_policy: &str,
    sandbox_is_workspace_write: bool,
) -> bool {
    matches!(reviewer, Some("auto_review" | "guardian_subagent"))
        && approval_policy == "on-request"
        && sandbox_is_workspace_write
}

/// `String(value ?? "")`, as the `approval_policy` fallback is written: `null`
/// is empty, anything else is `String(value)` (`spocky_contracts::js`), so an
/// array joins with commas and a number prints as JavaScript does. Only use
/// this where Paseo calls `String()`; other fallbacks are used raw. The text
/// leaves the value domain here (`js_text_to_utf8`).
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => js_text_to_utf8(&contracts_js_string(Some(&to_js_value(other)))),
    }
}

fn is_already_idle_interrupt(error: &ClientError) -> bool {
    error.rpc.is_some()
        && error.rpc_code_is(-32600)
        && error.message == "no active turn to interrupt"
}

/// Builds `{type, provider, ...rest}` in Paseo's literal order.
fn event(fields: &[(&str, Value)]) -> Map<String, Value> {
    let mut map = Map::new();
    let mut fields = fields.iter();
    if let Some((key, value)) = fields.next() {
        map.insert((*key).to_owned(), value.clone());
    }
    map.insert("provider".to_owned(), json!(CODEX_PROVIDER));
    for (key, value) in fields {
        map.insert((*key).to_owned(), value.clone());
    }
    map
}

fn timeline_event(item: Value) -> Map<String, Value> {
    event(&[("type", json!("timeline")), ("item", item)])
}

/// `notifySubscribers` tagging: the event's own `turnId`, else the active
/// foreground turn id, appended as `{ ...event, turnId }`.
fn emit(state: &State, events: &mut Vec<Value>, mut event: Map<String, Value>) {
    let turn_id = event
        .get("turnId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| state.active_foreground_turn_id.clone());
    if let Some(turn_id) = turn_id {
        event.insert("turnId".to_owned(), json!(turn_id));
    }
    events.push(Value::Object(event));
}

fn dispatch(state: &mut State, events: &mut Vec<Value>, parsed: ParsedNotification) {
    match parsed {
        ParsedNotification::AgentMessageDelta { item_id, delta, .. } => {
            handle_agent_message_delta(state, events, &item_id, delta);
        }
        ParsedNotification::ReasoningDelta { item_id, delta, .. } => {
            state
                .pending_reasoning
                .entry(item_id)
                .or_default()
                .push(delta.clone());
            emit(
                state,
                events,
                timeline_event(json!({"type": "reasoning", "text": delta})),
            );
        }
        ParsedNotification::ContextCompacted { thread_id, turn_id } => {
            handle_context_compacted(state, events, &thread_id, turn_id.as_deref());
        }
        ParsedNotification::ThreadRolledBack { num_turns, .. } => {
            truncate_user_message_turns(state, num_turns);
        }
        ParsedNotification::ThreadStarted { thread_id } => {
            state.current_thread_id = Some(thread_id.clone());
            emit(
                state,
                events,
                event(&[
                    ("type", json!("thread_started")),
                    ("sessionId", json!(thread_id)),
                ]),
            );
        }
        ParsedNotification::TurnStarted { turn_id, .. } => {
            state.current_turn_id = Some(turn_id.clone());
            if state
                .pending_identification
                .as_ref()
                .is_some_and(|pending| {
                    Some(&pending.foreground_turn_id) == state.active_foreground_turn_id.as_ref()
                })
                && let Some(pending) = state.pending_identification.take()
            {
                pending.slot.resolve(Some(turn_id));
            }
            reset_turn_tracking_state(state);
            emit(state, events, event(&[("type", json!("turn_started"))]));
        }
        ParsedNotification::TurnCompleted {
            status,
            error_message,
            ..
        } => handle_turn_completed(state, events, &status, error_message),
        ParsedNotification::PlanUpdated { plan, .. } => handle_plan_updated(state, events, &plan),
        ParsedNotification::TokenUsageUpdated { token_usage, .. } => {
            state.latest_usage = to_agent_usage(token_usage.as_ref());
            if let Some(usage) = state.latest_usage.clone() {
                emit(
                    state,
                    events,
                    event(&[("type", json!("usage_updated")), ("usage", usage)]),
                );
            }
        }
        ParsedNotification::ExecCommandOutputDelta { .. }
        | ParsedNotification::FileChangeOutputDelta { .. }
        | ParsedNotification::ExecCommandStarted { .. }
        | ParsedNotification::ExecCommandCompleted { .. }
        | ParsedNotification::TerminalInteraction { .. }
        | ParsedNotification::PatchApplyStarted { .. }
        | ParsedNotification::PatchApplyCompleted { .. } => {
            dispatch_tool_notification(state, events, parsed);
        }
        ParsedNotification::ItemCompleted {
            source,
            thread_id,
            turn_id,
            item,
        } => handle_item_completed(
            state,
            events,
            source,
            thread_id.as_deref(),
            turn_id.as_deref(),
            item,
        ),
        ParsedNotification::ItemStarted {
            source,
            turn_id,
            item,
            ..
        } => handle_item_started(state, events, source, turn_id.as_deref(), &item),
        // `turn/diff/updated` is whole-turn progress telemetry; invalid and
        // unknown notifications are only logged by Paseo.
        ParsedNotification::DiffUpdated { .. }
        | ParsedNotification::InvalidPayload { .. }
        | ParsedNotification::UnknownMethod { .. } => {}
    }
}

/// Legacy `codex/event/*` tool notifications and output deltas.
fn dispatch_tool_notification(
    state: &mut State,
    events: &mut Vec<Value>,
    parsed: ParsedNotification,
) {
    let parsed_kind = notification_kind(&parsed);
    match parsed {
        ParsedNotification::ExecCommandOutputDelta { call_id, chunk, .. } => {
            if let (Some(call_id), Some(chunk)) = (call_id, chunk) {
                append_output_delta(
                    &mut state.pending_command_output_deltas,
                    &call_id,
                    decode_output_delta_chunk(&chunk),
                );
            }
        }
        ParsedNotification::FileChangeOutputDelta {
            item_id,
            delta: Some(delta),
            ..
        } => {
            append_output_delta(
                &mut state.pending_file_change_output_deltas,
                &item_id,
                delta,
            );
        }
        ParsedNotification::ExecCommandStarted {
            call_id,
            command,
            cwd,
            ..
        } => {
            handle_exec_command_started(
                state,
                events,
                call_id.as_deref(),
                &command,
                cwd.as_deref(),
            );
        }
        ParsedNotification::ExecCommandCompleted {
            call_id,
            command,
            cwd,
            output,
            exit_code,
            success,
            stderr,
            ..
        } => handle_exec_command_completed(
            state,
            events,
            &ExecCompletion {
                call_id: call_id.as_deref(),
                command: &command,
                cwd: cwd.as_deref(),
                output,
                exit_code: exit_code.as_ref(),
                success,
                stderr: stderr.as_deref(),
            },
        ),
        ParsedNotification::PatchApplyStarted {
            call_id, changes, ..
        } => handle_patch_apply_started(state, events, call_id.as_deref(), &changes),
        ParsedNotification::PatchApplyCompleted {
            call_id,
            changes,
            stdout,
            stderr,
            success,
            ..
        } => handle_patch_apply_completed(
            state,
            events,
            &PatchCompletion {
                call_id: call_id.as_deref(),
                changes: &changes,
                stdout: stdout.as_deref(),
                stderr: stderr.as_deref(),
                success,
            },
        ),
        ParsedNotification::TerminalInteraction { .. } => {
            state
                .unported
                .push(format!("tool notification {parsed_kind}"));
        }
        _ => {}
    }
}

/// `handlePatchApplyStartedNotification` for the root thread: drops the
/// call's buffered file-change output, then emits the running `apply_patch`
/// item. The item's detail is the shared edit-detail branch, which is not
/// ported, so the mapping is recorded as unported (a missing call id emits
/// nothing, as Paseo's `null`).
fn handle_patch_apply_started(
    state: &mut State,
    events: &mut Vec<Value>,
    call_id: Option<&str>,
    changes: &Value,
) {
    if let Some(id) = call_id.filter(|id| !id.is_empty()) {
        state.pending_file_change_output_deltas.remove(id);
    }
    let config_cwd = state.config.cwd.clone();
    let mapped = map_patch_notification(&PatchNotification {
        call_id,
        changes,
        cwd: Some(config_cwd.as_str()),
        stdout: None,
        stderr: None,
        success: None,
        running: true,
    });
    if let Some(item) = tool_item_or_record(state, mapped) {
        emit(state, events, timeline_event(item));
    }
}

/// Fields of a legacy `patch_apply_end` notification.
struct PatchCompletion<'a> {
    call_id: Option<&'a str>,
    changes: &'a Value,
    stdout: Option<&'a str>,
    stderr: Option<&'a str>,
    success: Option<bool>,
}

/// `handlePatchApplyCompletedNotification` for the root thread: the call's
/// buffered file-change output stands in for a missing `stdout`
/// (`consumeOutputDelta`, which is also consumed when `stdout` is present).
fn handle_patch_apply_completed(
    state: &mut State,
    events: &mut Vec<Value>,
    completion: &PatchCompletion<'_>,
) {
    let buffered = completion
        .call_id
        .filter(|id| !id.is_empty())
        .and_then(|id| state.pending_file_change_output_deltas.remove(id))
        .map(|chunks| chunks.concat())
        .filter(|text| !text.is_empty());
    let config_cwd = state.config.cwd.clone();
    let mapped = map_patch_notification(&PatchNotification {
        call_id: completion.call_id,
        changes: completion.changes,
        cwd: Some(config_cwd.as_str()),
        stdout: completion.stdout.or(buffered.as_deref()),
        stderr: completion.stderr,
        success: completion.success,
        running: false,
    });
    if let Some(item) = tool_item_or_record(state, mapped) {
        emit(state, events, timeline_event(item));
    }
}

fn notification_kind(parsed: &ParsedNotification) -> &'static str {
    match parsed {
        ParsedNotification::TerminalInteraction { .. } => "terminal_interaction",
        ParsedNotification::PatchApplyStarted { .. } => "patch_apply_started",
        ParsedNotification::PatchApplyCompleted { .. } => "patch_apply_completed",
        _ => "other",
    }
}

/// `new CodexAsyncQuestions(saved)`: the saved records when the whole array
/// parses, keyed by request id with the last record winning; otherwise none.
fn saved_async_questions(saved: Option<&Value>) -> Vec<AsyncQuestionRecord> {
    let Some(Value::Array(entries)) = saved else {
        return Vec::new();
    };
    let mut records: Vec<AsyncQuestionRecord> = Vec::new();
    for entry in entries {
        let Some(record) = entry.as_object() else {
            return Vec::new();
        };
        let Some(item) = record
            .get("item")
            .and_then(Value::as_object)
            .and_then(items::parse_async_question)
        else {
            return Vec::new();
        };
        let resolution = match record.get("resolution") {
            None => None,
            Some(Value::String(text)) if text == "dismissed" => {
                Some(AsyncQuestionResolution::Dismissed)
            }
            Some(Value::Array(answers)) if answers.iter().all(Value::is_string) => {
                Some(AsyncQuestionResolution::Answers(
                    answers
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                ))
            }
            Some(_) => return Vec::new(),
        };
        let parsed = AsyncQuestionRecord { item, resolution };
        match records
            .iter_mut()
            .find(|known| known.item.id == parsed.item.id)
        {
            Some(known) => *known = parsed,
            None => records.push(parsed),
        }
    }
    records
}

/// `handleMcpElicitationRequest` short-circuits: a `url` mode request or a
/// schema with required fields is declined without asking the user. `None`
/// for anything else, including params that fail Paseo's schema.
fn paseo_declined_elicitation(params: Option<&Value>) -> Option<Value> {
    let record = params?.as_object()?;
    let string = |key: &str| matches!(record.get(key), Some(Value::String(_)));
    let optional_string = |key: &str| matches!(record.get(key), None | Some(Value::String(_)));
    let schema_ok = string("threadId")
        && matches!(
            record.get("turnId"),
            None | Some(Value::Null | Value::String(_))
        )
        && string("serverName")
        && matches!(
            record.get("mode").and_then(Value::as_str),
            Some("form" | "openai/form" | "url")
        )
        && string("message")
        && optional_string("url")
        && optional_string("elicitationId");
    if !schema_ok {
        return None;
    }
    let required = record
        .get("requestedSchema")
        .and_then(Value::as_object)
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array);
    let declined = record.get("mode").and_then(Value::as_str) == Some("url")
        || required.is_some_and(|required| !required.is_empty());
    declined.then(|| json!({"action": "decline", "content": null, "_meta": null}))
}

/// The reply Paseo itself sends when it dismisses each of these requests:
/// an MCP elicitation declined, a question answered with no answers.
fn unported_request_reply(method: &str) -> Value {
    if method == "mcpServer/elicitation/request" {
        json!({"action": "decline", "content": null, "_meta": null})
    } else {
        json!({"answers": {}})
    }
}

/// `appendOutputDeltaChunk`: empty ids and chunks are dropped.
fn append_output_delta(store: &mut HashMap<String, Vec<String>>, id: &str, chunk: String) {
    if id.is_empty() || chunk.is_empty() {
        return;
    }
    store.entry(id.to_owned()).or_default().push(chunk);
}

fn tool_item_or_record(state: &mut State, mapped: ToolMapping) -> Option<Value> {
    match mapped {
        ToolMapping::Item(item) => Some(item),
        ToolMapping::Skip => None,
        ToolMapping::Unported(what) => {
            state.unported.push(what);
            None
        }
    }
}

/// `handleExecCommandStartedNotification` for the root thread.
fn handle_exec_command_started(
    state: &mut State,
    events: &mut Vec<Value>,
    call_id: Option<&str>,
    command: &Value,
    cwd: Option<&str>,
) {
    if let Some(id) = call_id.filter(|id| !id.is_empty()) {
        state.emitted_exec_started_call_ids.insert(id.to_owned());
        state.pending_command_output_deltas.remove(id);
    }
    let config_cwd = state.config.cwd.clone();
    let mapped = exec_notification_to_tool_call(&ExecNotification {
        call_id,
        command,
        cwd: cwd.or(Some(config_cwd.as_str())),
        output: None,
        exit_code: None,
        success: None,
        stderr: None,
        running: true,
    });
    if let Some(item) = tool_item_or_record(state, mapped) {
        emit(state, events, timeline_event(item));
    }
}

/// Fields of a legacy `exec_command_end` notification.
struct ExecCompletion<'a> {
    call_id: Option<&'a str>,
    command: &'a Value,
    cwd: Option<&'a str>,
    output: Option<String>,
    exit_code: Option<&'a Value>,
    success: Option<bool>,
    stderr: Option<&'a str>,
}

/// `handleExecCommandCompletedNotification` for the root thread. Terminal
/// process tracking only feeds terminal interactions, which are not ported.
fn handle_exec_command_completed(
    state: &mut State,
    events: &mut Vec<Value>,
    completion: &ExecCompletion<'_>,
) {
    let buffered = completion
        .call_id
        .filter(|id| !id.is_empty())
        .and_then(|id| state.pending_command_output_deltas.remove(id))
        .map(|chunks| chunks.concat())
        .filter(|text| !text.is_empty());
    let resolved = completion.output.clone().or(buffered);
    let config_cwd = state.config.cwd.clone();
    let mapped = exec_notification_to_tool_call(&ExecNotification {
        call_id: completion.call_id,
        command: completion.command,
        cwd: completion.cwd.or(Some(config_cwd.as_str())),
        output: resolved.as_deref(),
        exit_code: completion.exit_code,
        success: completion.success,
        stderr: completion.stderr,
        running: false,
    });
    if let Some(item) = tool_item_or_record(state, mapped) {
        if let Some(id) = item["callId"].as_str() {
            state.emitted_exec_completed_call_ids.insert(id.to_owned());
        }
        emit(state, events, timeline_event(item));
    }
}

/// `clearPendingPermissions()`: every open approval answers `cancel`.
fn clear_pending_permissions(state: &mut State) {
    for pending in state.pending_permissions.drain(..) {
        pending
            .responder
            .respond(Ok(Some(json!({"decision": "cancel"}))));
    }
}

/// `resolvePermissionDecision(response)`.
fn permission_decision(response: &Value) -> &'static str {
    if response.get("behavior").and_then(Value::as_str) == Some("allow") {
        "accept"
    } else if response.get("interrupt").is_some_and(js_truthy) {
        "cancel"
    } else {
        "decline"
    }
}

/// `emitDeniedToolCallTimelineEvent` item.
fn denied_tool_call_item(request_id: &str, response: &Value, request: &Value) -> Value {
    let name = match request["name"].as_str() {
        Some("CodexBash") => "shell".to_owned(),
        Some("CodexFileChange") => "apply_patch".to_owned(),
        other => other.unwrap_or_default().to_owned(),
    };
    let message = match response.get("message") {
        None | Some(Value::Null) => json!("Permission denied"),
        Some(message) => message.clone(),
    };
    let detail = match request.get("detail") {
        None | Some(Value::Null) => json!({
            "type": "unknown",
            "input": request.get("input").cloned().unwrap_or(Value::Null),
            "output": null,
        }),
        Some(detail) => detail.clone(),
    };
    json!({
        "type": "tool_call",
        "callId": request_id,
        "name": name,
        "status": "failed",
        "error": {"message": message},
        "detail": detail,
        "metadata": {"permissionRequestId": request_id, "denied": true},
    })
}

/// zod `z.string().nullable().optional()` read: `Err` on a wrong type.
fn nullable_string_field(record: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match record.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    }
}

/// The `AgentPermissionRequest` Paseo builds for a command or file change
/// approval; `None` when the params fail Paseo's zod schema.
fn approval_request(
    kind: PermissionKind,
    params: Option<&Value>,
    config_cwd: &str,
) -> Option<Value> {
    let record = params?.as_object()?;
    let required = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
    let item_id = required("itemId")?;
    let thread_id = required("threadId")?;
    let turn_id = required("turnId")?;
    let reason = nullable_string_field(record, "reason").ok()?;
    let mut request = Map::new();
    request.insert("id".to_owned(), json!(format!("permission-{item_id}")));
    request.insert("provider".to_owned(), json!(CODEX_PROVIDER));
    match kind {
        PermissionKind::Command => {
            let command = nullable_string_field(record, "command").ok()?;
            let cwd = nullable_string_field(record, "cwd").ok()?;
            let command_value = command.clone().map_or(Value::Null, Value::String);
            let preview = exec_notification_to_tool_call(&ExecNotification {
                call_id: Some(&item_id),
                command: &command_value,
                cwd: cwd.as_deref().or(Some(config_cwd)),
                output: None,
                exit_code: None,
                success: None,
                stderr: None,
                running: true,
            });
            request.insert("name".to_owned(), json!("CodexBash"));
            request.insert("kind".to_owned(), json!("tool"));
            let title = match command.as_deref().filter(|command| !command.is_empty()) {
                Some(command) => format!("Run command: {command}"),
                None => "Run command".to_owned(),
            };
            request.insert("title".to_owned(), json!(title));
            if let Some(reason) = reason {
                request.insert("description".to_owned(), json!(reason));
            }
            let mut input = Map::new();
            if let Some(command) = &command {
                input.insert("command".to_owned(), json!(command));
            }
            if let Some(cwd) = &cwd {
                input.insert("cwd".to_owned(), json!(cwd));
            }
            request.insert("input".to_owned(), Value::Object(input));
            let detail = match preview {
                ToolMapping::Item(item) => item["detail"].clone(),
                ToolMapping::Skip | ToolMapping::Unported(_) => json!({
                    "type": "unknown",
                    "input": {"command": command, "cwd": cwd},
                    "output": null,
                }),
            };
            request.insert("detail".to_owned(), detail);
        }
        PermissionKind::File => {
            request.insert("name".to_owned(), json!("CodexFileChange"));
            request.insert("kind".to_owned(), json!("tool"));
            request.insert("title".to_owned(), json!("Apply file changes"));
            if let Some(reason) = &reason {
                request.insert("description".to_owned(), json!(reason));
            }
            request.insert(
                "detail".to_owned(),
                json!({"type": "unknown", "input": {"reason": reason}, "output": null}),
            );
        }
    }
    request.insert(
        "metadata".to_owned(),
        json!({"itemId": item_id, "threadId": thread_id, "turnId": turn_id}),
    );
    Some(Value::Object(request))
}

fn handle_agent_message_delta(
    state: &mut State,
    events: &mut Vec<Value>,
    item_id: &str,
    delta: String,
) {
    let previous = state
        .pending_agent_messages
        .get(item_id)
        .cloned()
        .unwrap_or_default();
    let is_first = previous.is_empty();
    state
        .pending_agent_messages
        .insert(item_id.to_owned(), format!("{previous}{delta}"));
    let text = if is_first && state.pending_assistant_message_boundary {
        format!("{ASSISTANT_MESSAGE_BOUNDARY_MARKDOWN}{delta}")
    } else {
        delta
    };
    emit(
        state,
        events,
        timeline_event(json!({
            "type": "assistant_message",
            "messageId": item_id,
            "text": text,
        })),
    );
    if is_first {
        state.pending_assistant_message_boundary = false;
    }
}

fn handle_plan_updated(
    state: &mut State,
    events: &mut Vec<Value>,
    plan: &[crate::notification::PlanEntry],
) {
    if state.plan_mode_enabled {
        let steps: Vec<String> = plan
            .iter()
            .map(|entry| entry.step.clone().unwrap_or_default())
            .collect();
        if let Some(item) = plan_tool_call("plan", &plan_steps_to_markdown(&steps)) {
            state.latest_plan_result = item["detail"]["text"].as_str().map(str::to_owned);
        }
        return;
    }
    emit(state, events, timeline_event(plan_update_to_todo(plan)));
}

/// `planStepsToMarkdown`.
fn plan_steps_to_markdown(steps: &[String]) -> String {
    let lines: Vec<String> = steps
        .iter()
        .map(|step| js_trim(step))
        .filter(|step| !step.is_empty())
        .map(|step| {
            if is_markdown_list_or_heading(step) {
                step.to_owned()
            } else {
                format!("- {step}")
            }
        })
        .collect();
    items::normalize_plan_markdown(&lines.join("\n"))
}

/// `/^(#{1,6}\s|[-*+]\s|\d+\.\s)/`.
fn is_markdown_list_or_heading(step: &str) -> bool {
    let mut chars = step.chars();
    let hashes = step.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        return step.chars().nth(hashes).is_some_and(is_js_whitespace);
    }
    if let Some(first) = chars.next()
        && matches!(first, '-' | '*' | '+')
    {
        return chars.next().is_some_and(is_js_whitespace);
    }
    let digits = step.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && step[digits..].starts_with('.')
        && step[digits + 1..]
            .chars()
            .next()
            .is_some_and(is_js_whitespace)
}

fn reset_turn_tracking_state(state: &mut State) {
    state.latest_plan_result = None;
    state.emitted_item_started_ids.clear();
    state.emitted_item_completed_ids.clear();
    state.emitted_exec_started_call_ids.clear();
    state.emitted_exec_completed_call_ids.clear();
    state.pending_command_output_deltas.clear();
    state.pending_file_change_output_deltas.clear();
    state.pending_agent_messages.clear();
    state.pending_reasoning.clear();
    state.pending_assistant_message_boundary = false;
    state.pending_root_compaction_item_ids.clear();
    state.pending_anonymous_root_compactions = 0;
    state.unpaired_compaction_notification_completions = 0;
    state.unpaired_compaction_item_completions = 0;
}

fn handle_turn_completed(
    state: &mut State,
    events: &mut Vec<Value>,
    status: &str,
    error_message: Option<String>,
) {
    complete_pending_root_compactions(state, events);
    match status {
        "failed" => emit(
            state,
            events,
            event(&[
                ("type", json!("turn_failed")),
                (
                    "error",
                    json!(error_message.unwrap_or_else(|| "Codex turn failed".to_owned())),
                ),
            ]),
        ),
        "interrupted" => {
            dismiss_interrupted_async_questions(state, events);
            emit(
                state,
                events,
                event(&[
                    ("type", json!("turn_canceled")),
                    ("reason", json!("interrupted")),
                ]),
            );
        }
        _ => {
            if state.plan_mode_enabled && state.latest_plan_result.is_some() {
                state.unported.push("synthetic plan approval".to_owned());
            }
            let mut completed = event(&[("type", json!("turn_completed"))]);
            if let Some(usage) = &state.latest_usage {
                completed.insert("usage".to_owned(), usage.clone());
            }
            emit(state, events, completed);
        }
    }
    state.active_foreground_turn_id = None;
    state.active_client_message_id = None;
    state.current_turn_id = None;
    if let Some(pending) = state.pending_identification.take() {
        pending.slot.resolve(None);
    }
    reset_turn_tracking_state(state);
}

fn dismiss_interrupted_async_questions(state: &mut State, events: &mut Vec<Value>) {
    let pending: Vec<usize> = state
        .async_questions
        .iter()
        .enumerate()
        .filter(|(_, record)| record.resolution.is_none())
        .map(|(index, _)| index)
        .collect();
    for index in pending {
        state.async_questions[index].resolution = Some(AsyncQuestionResolution::Dismissed);
        let record = &state.async_questions[index];
        let timeline = async_question_timeline(&record.item, record.resolution.as_ref());
        let request_id = format!("permission-{}", record.item.id);
        emit(state, events, timeline_event(timeline));
        emit(
            state,
            events,
            event(&[
                ("type", json!("permission_resolved")),
                ("requestId", json!(request_id)),
                (
                    "resolution",
                    json!({"behavior": "deny", "message": "Interrupted"}),
                ),
            ]),
        );
    }
}

fn receive_async_question(
    state: &mut State,
    events: &mut Vec<Value>,
    thread_id: Option<&str>,
    item: &Map<String, Value>,
) {
    if thread_id != state.current_thread_id.as_deref() {
        return;
    }
    let Some(question) = items::parse_async_question(item) else {
        return;
    };
    if state
        .async_questions
        .iter()
        .any(|record| record.item.id == question.id)
    {
        return;
    }
    let request = async_question_permission(&question);
    state.async_questions.push(AsyncQuestionRecord {
        item: question,
        resolution: None,
    });
    emit(
        state,
        events,
        event(&[
            ("type", json!("permission_requested")),
            ("request", request),
        ]),
    );
}

/// `rememberCodexUserMessageTurn`: true when the message id is new.
fn remember_user_message_turn(
    state: &mut State,
    message_id: Option<&str>,
    provider_turn_id: Option<&str>,
) -> bool {
    let Some(message_id) = message_id.filter(|id| !id.is_empty()) else {
        return false;
    };
    let known = state
        .user_message_turn_ids
        .iter()
        .any(|id| id == message_id);
    if let Some(turn_id) = provider_turn_id.filter(|id| !id.is_empty()) {
        state
            .user_message_provider_turn_ids
            .insert(message_id.to_owned(), turn_id.to_owned());
    }
    if known {
        return false;
    }
    state.user_message_turn_ids.push(message_id.to_owned());
    true
}

fn truncate_user_message_turns(state: &mut State, num_turns: u64) {
    if num_turns == 0 {
        return;
    }
    let retained = state
        .user_message_turn_ids
        .len()
        .saturating_sub(usize::try_from(num_turns).unwrap_or(usize::MAX));
    for message_id in state.user_message_turn_ids.split_off(retained) {
        state.user_message_provider_turn_ids.remove(&message_id);
    }
}

fn handle_user_message_item(
    state: &mut State,
    events: &mut Vec<Value>,
    turn_id: Option<&str>,
    item: &Map<String, Value>,
) {
    let ThreadItemMapping::Item(Value::Object(mut timeline_item)) =
        thread_item_to_timeline(&Value::Object(item.clone()), true)
    else {
        return;
    };
    let message_id = timeline_item
        .get("messageId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if !remember_user_message_turn(state, message_id.as_deref(), turn_id) {
        return;
    }
    let client_message_id = timeline_item
        .get("clientMessageId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| state.active_client_message_id.clone());
    if let Some(client_message_id) = client_message_id {
        timeline_item.insert("clientMessageId".to_owned(), json!(client_message_id));
    }
    state.active_client_message_id = None;
    emit(state, events, timeline_event(Value::Object(timeline_item)));
}

fn is_mirrored_legacy_item(source: ItemSource, item: &Map<String, Value>) -> bool {
    source == ItemSource::CodexEvent && item_type(item) != Some("subAgentActivity")
}

fn handle_item_completed(
    state: &mut State,
    events: &mut Vec<Value>,
    source: ItemSource,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
    item: Map<String, Value>,
) {
    if is_mirrored_legacy_item(source, &item) {
        return;
    }
    receive_async_question(state, events, thread_id, &item);
    let normalized = item_type(&item).map(str::to_owned);
    if normalized.as_deref() == Some("userMessage") {
        handle_user_message_item(state, events, turn_id, &item);
        return;
    }
    if normalized.as_deref() == Some("contextCompaction") {
        handle_completed_context_compaction_item(state, events, item_id(&item));
        return;
    }
    let item_id = item_id(&item)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let mut timeline_item = match thread_item_to_timeline(&Value::Object(item), false) {
        ThreadItemMapping::Item(timeline_item) => timeline_item,
        ThreadItemMapping::Skip => return,
        ThreadItemMapping::Unported { item_type } => {
            state.unported.push(format!("item/completed {item_type}"));
            return;
        }
    };
    if should_skip_completed_thread_item(
        state,
        &timeline_item,
        normalized.as_deref(),
        item_id.as_deref(),
    ) {
        return;
    }
    if consume_streamed_text_completion(state, events, &timeline_item, item_id.as_deref()) {
        if timeline_item["type"] == "assistant_message" {
            state.pending_assistant_message_boundary = true;
        }
        if let Some(id) = &item_id {
            state.emitted_item_completed_ids.insert(id.clone());
            state.emitted_item_started_ids.remove(id);
        }
        return;
    }
    apply_buffered_delta_text(state, &mut timeline_item, item_id.as_deref());
    if timeline_item["type"] == "tool_call" && timeline_item["detail"]["type"] == "plan" {
        state.latest_plan_result = timeline_item["detail"]["text"].as_str().map(str::to_owned);
        if state.plan_mode_enabled {
            return;
        }
    }
    let is_assistant = timeline_item["type"] == "assistant_message";
    emit(state, events, timeline_event(timeline_item));
    if is_assistant {
        state.pending_assistant_message_boundary = true;
    }
    if let Some(id) = item_id {
        state.emitted_item_started_ids.remove(&id);
        state.pending_command_output_deltas.remove(&id);
        state.pending_file_change_output_deltas.remove(&id);
        state.emitted_item_completed_ids.insert(id);
    }
}

/// `shouldSkipCompletedThreadItem`: legacy `exec_command_end` is
/// authoritative for command items.
fn should_skip_completed_thread_item(
    state: &State,
    timeline_item: &Value,
    normalized_type: Option<&str>,
    item_id: Option<&str>,
) -> bool {
    if timeline_item["type"] == "tool_call" && normalized_type == Some("commandExecution") {
        let call_id = timeline_item["callId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or(item_id);
        return call_id.is_some_and(|id| state.emitted_exec_completed_call_ids.contains(id));
    }
    item_id.is_some_and(|id| state.emitted_item_completed_ids.contains(id))
}

fn item_id(item: &Map<String, Value>) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

fn consume_streamed_text_completion(
    state: &mut State,
    events: &mut Vec<Value>,
    timeline_item: &Value,
    item_id: Option<&str>,
) -> bool {
    let Some(item_id) = item_id else {
        return false;
    };
    let streamed = match timeline_item["type"].as_str() {
        Some("assistant_message") => state.pending_agent_messages.remove(item_id),
        Some("reasoning") => state
            .pending_reasoning
            .remove(item_id)
            .map(|parts| parts.concat()),
        _ => None,
    };
    let Some(streamed) = streamed else {
        return false;
    };
    if let Some(suffix) = missing_final_text_suffix(timeline_item, &streamed) {
        emit(state, events, timeline_event(suffix));
    }
    true
}

/// `buildMissingFinalTextSuffix`.
fn missing_final_text_suffix(timeline_item: &Value, streamed: &str) -> Option<Value> {
    let text = timeline_item["text"].as_str().unwrap_or("");
    let Some(suffix) = text.strip_prefix(streamed) else {
        return Some(timeline_item.clone());
    };
    if suffix.is_empty() {
        return None;
    }
    if timeline_item["type"] == "assistant_message" {
        let mut item = Map::new();
        item.insert("type".to_owned(), json!("assistant_message"));
        item.insert("text".to_owned(), json!(suffix));
        if let Some(message_id) = non_empty_string(timeline_item.get("messageId")) {
            item.insert("messageId".to_owned(), json!(message_id));
        }
        return Some(Value::Object(item));
    }
    Some(json!({"type": "reasoning", "text": suffix}))
}

fn apply_buffered_delta_text(state: &State, timeline_item: &mut Value, item_id: Option<&str>) {
    let Some(item_id) = item_id else {
        return;
    };
    let buffered = match timeline_item["type"].as_str() {
        Some("assistant_message") => state.pending_agent_messages.get(item_id).cloned(),
        Some("reasoning") => state
            .pending_reasoning
            .get(item_id)
            .map(|parts| parts.concat()),
        _ => None,
    };
    if let Some(buffered) = buffered.filter(|buffered| !buffered.is_empty()) {
        let text = timeline_item["text"].as_str().unwrap_or("");
        if !text.starts_with(&buffered) {
            timeline_item["text"] = json!(buffered);
        }
    }
}

fn handle_item_started(
    state: &mut State,
    events: &mut Vec<Value>,
    source: ItemSource,
    turn_id: Option<&str>,
    item: &Map<String, Value>,
) {
    if is_mirrored_legacy_item(source, item) {
        return;
    }
    let normalized = item_type(item);
    if normalized == Some("userMessage") {
        handle_user_message_item(state, events, turn_id, item);
        return;
    }
    if normalized == Some("contextCompaction") {
        let id = item_id(item).map(str::to_owned);
        track_pending_root_compaction(state, id.as_deref());
        let compaction = compaction_item(state, "loading", id.as_deref());
        emit(state, events, timeline_event(compaction));
        return;
    }
    let timeline_item = match thread_item_to_timeline(&Value::Object(item.clone()), false) {
        ThreadItemMapping::Item(timeline_item) => timeline_item,
        ThreadItemMapping::Skip => return,
        ThreadItemMapping::Unported { item_type } => {
            state.unported.push(format!("item/started {item_type}"));
            return;
        }
    };
    if timeline_item["type"] != "tool_call" {
        return;
    }
    let id = item_id(item).filter(|id| !id.is_empty()).map(str::to_owned);
    if normalized == Some("commandExecution") {
        let call_id = timeline_item["callId"]
            .as_str()
            .filter(|call| !call.is_empty())
            .or(id.as_deref());
        if call_id.is_some_and(|call| state.emitted_exec_started_call_ids.contains(call)) {
            return;
        }
    }
    if id
        .as_ref()
        .is_some_and(|id| state.emitted_item_started_ids.contains(id))
    {
        return;
    }
    emit(state, events, timeline_event(timeline_item));
    if let Some(id) = id {
        state.pending_command_output_deltas.remove(&id);
        state.pending_file_change_output_deltas.remove(&id);
        state.emitted_item_started_ids.insert(id);
    }
}

fn resolve_compaction_trigger(state: &mut State, item_id: Option<&str>) -> Option<&'static str> {
    if let Some(known) = item_id.and_then(|id| state.compaction_trigger_by_item_id.get(id)) {
        return Some(known);
    }
    // Manual compaction starts come from the `/compact` slash command, which
    // is not ported, so no pending manual start exists.
    None
}

fn track_pending_root_compaction(state: &mut State, item_id: Option<&str>) {
    match item_id.filter(|id| !id.is_empty()) {
        Some(id) => {
            if !state
                .pending_root_compaction_item_ids
                .iter()
                .any(|known| known == id)
            {
                state.pending_root_compaction_item_ids.push(id.to_owned());
            }
        }
        None => state.pending_anonymous_root_compactions += 1,
    }
}

/// What `consumePendingRootCompaction` consumed.
enum ConsumedCompaction {
    Item(String),
    Anonymous,
}

impl ConsumedCompaction {
    fn item_id(self) -> Option<String> {
        match self {
            Self::Item(id) => Some(id),
            Self::Anonymous => None,
        }
    }
}

/// `consumePendingRootCompaction`: `None` when nothing was pending.
fn consume_pending_root_compaction(
    state: &mut State,
    item_id: Option<&str>,
) -> Option<ConsumedCompaction> {
    if let Some(id) = item_id.filter(|id| !id.is_empty()) {
        if let Some(position) = state
            .pending_root_compaction_item_ids
            .iter()
            .position(|known| known == id)
        {
            state.pending_root_compaction_item_ids.remove(position);
            return Some(ConsumedCompaction::Item(id.to_owned()));
        }
        if state.pending_root_compaction_item_ids.is_empty()
            && state.pending_anonymous_root_compactions > 0
        {
            state.pending_anonymous_root_compactions -= 1;
            return Some(ConsumedCompaction::Anonymous);
        }
        return None;
    }
    if !state.pending_root_compaction_item_ids.is_empty() {
        return Some(ConsumedCompaction::Item(
            state.pending_root_compaction_item_ids.remove(0),
        ));
    }
    if state.pending_anonymous_root_compactions > 0 {
        state.pending_anonymous_root_compactions -= 1;
        return Some(ConsumedCompaction::Anonymous);
    }
    None
}

fn compaction_item(state: &mut State, status: &str, item_id: Option<&str>) -> Value {
    let trigger = resolve_compaction_trigger(state, item_id);
    if let (Some(id), Some(trigger)) = (item_id.filter(|id| !id.is_empty()), trigger) {
        if status == "loading" {
            state
                .compaction_trigger_by_item_id
                .insert(id.to_owned(), trigger);
        } else {
            state.compaction_trigger_by_item_id.remove(id);
        }
    }
    let mut item = Map::new();
    item.insert("type".to_owned(), json!("compaction"));
    item.insert("status".to_owned(), json!(status));
    if let Some(trigger) = trigger {
        item.insert("trigger".to_owned(), json!(trigger));
    }
    Value::Object(item)
}

fn handle_completed_context_compaction_item(
    state: &mut State,
    events: &mut Vec<Value>,
    item_id: Option<&str>,
) {
    let consumed = consume_pending_root_compaction(state, item_id);
    let has_other_pending = !state.pending_root_compaction_item_ids.is_empty()
        || state.pending_anonymous_root_compactions > 0;
    if item_id.is_some() && consumed.is_none() && has_other_pending {
        return;
    }
    if state.unpaired_compaction_notification_completions > 0 {
        state.unpaired_compaction_notification_completions -= 1;
        return;
    }
    let item = compaction_item(state, "completed", item_id);
    emit(state, events, timeline_event(item));
    state.unpaired_compaction_item_completions += 1;
}

fn complete_pending_root_compactions(state: &mut State, events: &mut Vec<Value>) {
    let ids = std::mem::take(&mut state.pending_root_compaction_item_ids);
    for id in ids {
        let item = compaction_item(state, "completed", Some(&id));
        emit(state, events, timeline_event(item));
    }
    for _ in 0..state.pending_anonymous_root_compactions {
        let item = compaction_item(state, "completed", None);
        emit(state, events, timeline_event(item));
    }
    state.pending_anonymous_root_compactions = 0;
}

fn handle_context_compacted(
    state: &mut State,
    events: &mut Vec<Value>,
    thread_id: &str,
    turn_id: Option<&str>,
) {
    if Some(thread_id) != state.current_thread_id.as_deref() {
        return;
    }
    if state.unpaired_compaction_item_completions > 0 {
        state.unpaired_compaction_item_completions -= 1;
        return;
    }
    let pending_item_id =
        consume_pending_root_compaction(state, None).and_then(ConsumedCompaction::item_id);
    state.unpaired_compaction_notification_completions += 1;
    let item = compaction_item(state, "completed", pending_item_id.as_deref());
    let mut compacted = timeline_event(item);
    if let Some(turn_id) = turn_id.filter(|id| !id.is_empty()) {
        compacted.insert("turnId".to_owned(), json!(turn_id));
    }
    emit(state, events, compacted);
}

/// Paseo resolves `/name` prompts against `listCommands()` (custom prompts,
/// skills, `/compact`, `/goal`); that resolution is not ported, so such a
/// prompt fails loudly instead of being sent as plain text.
fn reject_slash_command(prompt: &Prompt) -> Result<(), String> {
    if let Prompt::Text(text) = prompt
        && let Some(command) = parse_slash_command(text)
    {
        return Err(format!(
            "Codex slash command resolution is not ported: /{command}"
        ));
    }
    Ok(())
}

/// `toCodexTextInput` and the string or text-block prompt paths of
/// `buildUserInput`.
fn build_user_input(prompt: &Prompt) -> Result<Value, String> {
    match prompt {
        Prompt::Text(text) => Ok(json!([text_input(text)])),
        Prompt::Blocks(blocks) => {
            let mut output = Vec::with_capacity(blocks.len());
            for block in blocks {
                match (block.get("type").and_then(Value::as_str), block.get("text")) {
                    (Some("text"), Some(Value::String(text))) => output.push(text_input(text)),
                    (kind, _) => {
                        return Err(format!(
                            "Codex prompt block is not ported: {}",
                            kind.unwrap_or("unknown")
                        ));
                    }
                }
            }
            Ok(Value::Array(output))
        }
    }
}

fn text_input(text: &str) -> Value {
    json!({"type": "text", "text": text, "text_elements": []})
}

/// `parseSlashCommandInput`: the command name when the prompt is `/name`.
fn parse_slash_command(text: &str) -> Option<String> {
    let trimmed = js_trim(text);
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let end = rest.find(is_js_whitespace).unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty() && !name.contains('/')).then(|| name.to_owned())
}

fn parse_collaboration_modes(response: &Value) -> Vec<CollaborationMode> {
    let Some(Value::Array(data)) = object(response).and_then(|response| response.get("data"))
    else {
        return Vec::new();
    };
    data.iter()
        .map(|entry| {
            let record = entry.as_object();
            let text = |key: &str| string_field(record, key).map(str::to_owned);
            CollaborationMode {
                name: text("name").unwrap_or_default(),
                mode: text("mode"),
                model: text("model"),
                reasoning_effort: text("reasoning_effort"),
                developer_instructions: text("developer_instructions"),
            }
        })
        .collect()
}

/// `findCollaborationMode(target)`.
fn find_collaboration_mode(modes: &[CollaborationMode], plan: bool) -> Option<&CollaborationMode> {
    if modes.is_empty() {
        return None;
    }
    let find_by_name = |predicate: &dyn Fn(&str) -> bool| {
        modes
            .iter()
            .find(|mode| predicate(&mode.name.to_lowercase()))
    };
    if plan {
        return find_by_name(&|name| name.contains("plan") || name.contains("read"));
    }
    find_by_name(&|name| name.contains("auto") || name.contains("code"))
        .or_else(|| find_by_name(&|name| !name.contains("plan") && !name.contains("read")))
        .or_else(|| modes.first())
}

/// `refreshResolvedCollaborationMode` via `resolveCollaborationMode`.
fn refresh_collaboration_mode(state: &mut State) {
    let Some(found) = find_collaboration_mode(&state.collaboration_modes, state.plan_mode_enabled)
    else {
        state.resolved_collaboration_mode = None;
        return;
    };
    let mut settings = Map::new();
    if let Some(model) = found.model.as_deref().filter(|model| !model.is_empty()) {
        settings.insert("model".to_owned(), json!(model));
    }
    if let Some(effort) = found
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        settings.insert("reasoning_effort".to_owned(), json!(effort));
    }
    if let Some(instructions) = compose_system_prompt_parts(&[
        found.developer_instructions.as_deref(),
        state.config.system_prompt.as_deref(),
        state.config.daemon_append_system_prompt.as_deref(),
    ]) {
        settings.insert("developer_instructions".to_owned(), json!(instructions));
    }
    if let Some(model) = state
        .config
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
    {
        settings.insert("model".to_owned(), json!(model));
    }
    if let Some(effort) = normalize_thinking(state.config.thinking_option_id.as_deref()) {
        settings.insert("reasoning_effort".to_owned(), json!(effort));
    }
    state.resolved_collaboration_mode = Some(ResolvedCollaborationMode {
        mode: found.mode.clone().unwrap_or_else(|| "code".to_owned()),
        settings,
        name: found.name.clone(),
    });
}

/// `readSandboxWorkspaceWrite`.
fn read_sandbox_workspace_write(value: &Value) -> Option<Map<String, Value>> {
    let record = value.as_object()?;
    let pick = |snake: &str, camel: &str| -> Option<&Value> {
        match record.get(snake) {
            None | Some(Value::Null) => record.get(camel),
            some => some,
        }
    };
    let mut output = Map::new();
    if let Some(Value::Array(roots)) = pick("writable_roots", "writableRoots") {
        let roots: Vec<Value> = roots
            .iter()
            .filter(|root| root.is_string())
            .cloned()
            .collect();
        output.insert("writable_roots".to_owned(), Value::Array(roots));
    }
    for (snake, camel) in [
        ("network_access", "networkAccess"),
        ("exclude_slash_tmp", "excludeSlashTmp"),
        ("exclude_tmpdir_env_var", "excludeTmpdirEnvVar"),
    ] {
        if let Some(flag @ Value::Bool(_)) = pick(snake, camel) {
            output.insert(snake.to_owned(), flag.clone());
        }
    }
    Some(output)
}

/// `rememberResolvedSandboxPolicy(response)`.
fn remember_resolved_sandbox_policy(state: &mut State, response: &Value) {
    let sandbox = object(response)
        .and_then(|response| response.get("sandbox"))
        .and_then(object)
        .cloned();
    let is_workspace_write = sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.get("type"))
        .and_then(Value::as_str)
        == Some("workspaceWrite");
    if is_workspace_write {
        state.resolved_workspace_write = sandbox
            .as_ref()
            .and_then(|sandbox| read_sandbox_workspace_write(&Value::Object(sandbox.clone())));
    }
    state.resolved_sandbox_policy = sandbox;
}

/// `toCodexSandboxPolicyType`.
fn to_codex_sandbox_policy_type(kind: &str) -> &'static str {
    match kind {
        "workspace-write" => "workspaceWrite",
        "read-only" => "readOnly",
        _ => "dangerFullAccess",
    }
}

/// `toSandboxPolicy(type, workspaceWrite)`.
fn to_sandbox_policy(kind: &str, workspace_write: &Map<String, Value>) -> Value {
    let field = |key: &str, fallback: Value| match workspace_write.get(key) {
        None | Some(Value::Null) => fallback,
        Some(value) => value.clone(),
    };
    match kind {
        "read-only" => json!({"type": "readOnly"}),
        "workspace-write" => json!({
            "type": "workspaceWrite",
            "networkAccess": field("network_access", json!(false)),
            "writableRoots": field("writable_roots", json!([])),
            "excludeSlashTmp": field("exclude_slash_tmp", json!(false)),
            "excludeTmpdirEnvVar": field("exclude_tmpdir_env_var", json!(false)),
        }),
        "danger-full-access" => json!({"type": "dangerFullAccess"}),
        _ => json!({"type": "workspaceWrite", "networkAccess": false, "writableRoots": []}),
    }
}

/// `toCodexMcpConfig`.
fn to_codex_mcp_config(server: &Value) -> Value {
    let mut output = Map::new();
    let mut copy = |from: &str, to: &str| {
        if let Some(value) = server.get(from) {
            output.insert(to.to_owned(), value.clone());
        }
    };
    if server.get("type").and_then(Value::as_str) == Some("stdio") {
        copy("command", "command");
        copy("args", "args");
        copy("env", "env");
    } else {
        copy("url", "url");
        copy("headers", "http_headers");
    }
    Value::Object(output)
}

/// A value as a JavaScript property key (`String(value)`); a missing value is
/// `undefined`. The key leaves the value domain here (`js_text_to_utf8`), so it
/// equals the JSON key it names.
fn property_key(value: Option<&Value>) -> String {
    js_text_to_utf8(&contracts_js_string(value.map(to_js_value).as_ref()))
}

/// `applyCodexToolPolicy(config, toolPolicy)`.
fn apply_codex_tool_policy(
    mut config: Map<String, Value>,
    tool_policy: Option<&Value>,
) -> Map<String, Value> {
    let Some(policy) = tool_policy else {
        return config;
    };
    let mut servers = match config.get("mcp_servers") {
        Some(Value::Object(servers)) => servers.clone(),
        _ => Map::new(),
    };
    // `grantsByServer`: a `Map` keyed by the grant's own `server` value (a
    // missing one is `undefined`), each holding the grants' own `tool`
    // values. Nothing is stringified until a value becomes a property key.
    let mut grants: Vec<(Option<&Value>, Vec<Option<&Value>>)> = Vec::new();
    if let Some(Value::Array(preapproved)) = policy.get("preapproved") {
        for grant in preapproved {
            let (server, tool) = (grant.get("server"), grant.get("tool"));
            match grants.iter_mut().find(|(name, _)| *name == server) {
                Some((_, tools)) => tools.push(tool),
                None => grants.push((server, vec![tool])),
            }
        }
    }
    for (server, tools) in grants {
        let server_key = property_key(server);
        let mut server_config = match servers.get(&server_key) {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        let approvals: Map<String, Value> = tools
            .iter()
            .map(|tool| (property_key(*tool), json!({"approval_mode": "approve"})))
            .collect();
        let enabled: Vec<Value> = tools
            .iter()
            .map(|tool| tool.cloned().unwrap_or(Value::Null))
            .collect();
        server_config.insert("enabled_tools".to_owned(), Value::Array(enabled));
        server_config.insert("default_tools_approval_mode".to_owned(), json!("prompt"));
        server_config.insert("tools".to_owned(), Value::Object(approvals));
        servers.insert(server_key, Value::Object(server_config));
    }
    config.insert("mcp_servers".to_owned(), Value::Object(servers));
    config
}

/// Every use of the model in pinned `resolveModelAndThinking` is a JavaScript
/// truthiness check (`!model`, `model ? ... : ...`), so an empty string is
/// unset, not a model id.
fn unset_if_empty(model: Option<String>) -> Option<String> {
    model.filter(|model| !model.is_empty())
}

/// `resolveModelAndThinking()`.
fn resolve_model_and_thinking(
    client: &AppServerClient,
    model: Option<String>,
    thinking: Option<String>,
) -> Result<(String, Option<String>), String> {
    let mut model = unset_if_empty(model);
    let mut thinking = thinking;
    if model.is_none() || thinking.is_none() {
        let defaults = read_configured_defaults(client);
        model = model.or_else(|| unset_if_empty(defaults.model));
        thinking = thinking.or(defaults.thinking_option_id);
    }
    if model.is_none() || thinking.is_none() {
        let response = client
            .request("model/list", Some(json!({})), DEFAULT_REQUEST_TIMEOUT)
            .map_err(|error| error.message)?;
        let models: Vec<(String, bool, Option<String>)> = object(&response)
            .and_then(|response| response.get("data"))
            .and_then(Value::as_array)
            .map(|data| {
                data.iter()
                    .map(|entry| {
                        let record = entry.as_object();
                        (
                            string_field(record, "id").unwrap_or_default().to_owned(),
                            record
                                .and_then(|record| record.get("isDefault"))
                                .is_some_and(js_truthy),
                            string_field(record, "defaultReasoningEffort").map(str::to_owned),
                        )
                    })
                    .filter(|(id, _, _)| !id.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let default_model = models
            .iter()
            .find(|(_, is_default, _)| *is_default)
            .or_else(|| models.first())
            .ok_or_else(|| "No models available from Codex app-server".to_owned())?;
        let selected = model
            .as_ref()
            .and_then(|wanted| models.iter().find(|(id, _, _)| id == wanted))
            .unwrap_or(default_model);
        if model.is_none() {
            model = Some(selected.0.clone());
        }
        if thinking.is_none() {
            thinking = normalize_thinking(selected.2.as_deref());
        }
    }
    let model = model.ok_or_else(|| "Unable to resolve Codex model".to_owned())?;
    Ok((model, thinking))
}

/// The signal passed to a provider call aborted before it finished. Paseo
/// rethrows `signal.reason` here, so the caller raises that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The operation was aborted")
    }
}

/// What `steerActiveTurn` admits a steer against.
struct SteerAdmission {
    client: AppServerClient,
    thread_id: String,
    native_turn_id: String,
    foreground: String,
}

/// `SteerActiveTurnOptions` as `steerActiveTurn` reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteerOptions {
    pub expected_turn_id: String,
    pub client_message_id: Option<String>,
    pub clear_pending_permissions: bool,
}

/// `SteerResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerResult {
    Accepted,
    Unavailable,
}

/// `isDefinitiveCodexSteerRejection(error)`: a JSON-RPC error that shows the
/// steer could not have submitted input. A generic invalid request, timeout,
/// disconnect, or unknown error is ambiguous, so it is not definitive.
#[must_use]
pub fn is_definitive_steer_rejection(error: &ClientError) -> bool {
    let Some(rpc) = error.rpc.as_deref() else {
        return false;
    };
    let is_code = |expected: f64| matches!(&rpc.code, Some(Value::Number(code)) if code.as_f64() == Some(expected));
    if is_code(-32601.0) {
        return true;
    }
    if !is_code(-32600.0) {
        return false;
    }
    let not_steerable = rpc
        .data
        .as_ref()
        .and_then(object)
        .and_then(|data| data.get("codexErrorInfo"))
        .and_then(object)
        .and_then(|info| info.get("activeTurnNotSteerable"))
        .is_some_and(Value::is_object);
    not_steerable
        || error.message == "no active turn to steer"
        || is_expected_turn_mismatch(&error.message)
        || error.message == "active turn uses a different output schema"
}

/// ``/^expected active turn id `[^`]+` but found `[^`]+`$/``.
fn is_expected_turn_mismatch(message: &str) -> bool {
    let Some(rest) = message.strip_prefix("expected active turn id `") else {
        return false;
    };
    let Some((expected, rest)) = rest.split_once('`') else {
        return false;
    };
    let Some(found) = rest
        .strip_prefix(" but found `")
        .and_then(|found| found.strip_suffix('`'))
    else {
        return false;
    };
    !expected.is_empty() && !found.is_empty() && !found.contains('`')
}

/// The native archive state `updateNativeThreadArchiveState` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeArchiveState {
    Archive,
    Restore,
}

/// The requests of `updateNativeThreadArchiveState` after `initialized`.
fn archive_state_requests(
    client: &AppServerClient,
    thread_id: &str,
    state: NativeArchiveState,
) -> Result<(), String> {
    let params = || Some(json!({"threadId": thread_id}));
    if state == NativeArchiveState::Archive {
        return client
            .request("thread/archive", params(), DEFAULT_REQUEST_TIMEOUT)
            .map(|_| ())
            .map_err(|error| error.message);
    }
    let Err(error) = client.request("thread/unarchive", params(), DEFAULT_REQUEST_TIMEOUT) else {
        return Ok(());
    };
    // `isCodexAlreadyUnarchivedError(error, threadId)`.
    if !error.message.contains(&format!(
        "no archived rollout found for thread id {thread_id}"
    )) {
        return Err(error.message);
    }
    client
        .request("thread/read", params(), DEFAULT_REQUEST_TIMEOUT)
        .map(|_| ())
        .map_err(|_| error.message)
}

/// `CodexAppServerAgentClient`: owns launch settings and the version gates,
/// and creates sessions.
pub struct CodexProvider {
    runtime_settings: Option<ProviderRuntimeSettings>,
    custom_provider: Option<CustomProvider>,
    base_env: Vec<(OsString, OsString)>,
    /// `goalsEnabledPromise`.
    goals_enabled: OnceLock<bool>,
    /// `autoReviewEnabledPromise`, filled only by calls without a signal.
    auto_review_enabled: OnceLock<bool>,
}

impl CodexProvider {
    /// `base_env` is the daemon's process env (`process.env`).
    #[must_use]
    pub fn new(
        runtime_settings: Option<ProviderRuntimeSettings>,
        custom_provider: Option<CustomProvider>,
        base_env: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            runtime_settings,
            custom_provider,
            base_env,
            goals_enabled: OnceLock::new(),
            auto_review_enabled: OnceLock::new(),
        }
    }

    /// The gates `createSession` and `resumeSession` resolve:
    /// `resolveGoalsEnabled()` then `resolveAutoReviewEnabled()`, both
    /// without a signal, so both memoized.
    pub fn gates(&self) -> CodexGates {
        CodexGates {
            goals_enabled: self.resolve_goals_enabled(),
            auto_review_enabled: self.resolve_auto_review_enabled(None).unwrap_or(false),
        }
    }

    /// `isAvailable()`: whether launch resolution finds a runnable Codex. A
    /// failed lookup (other than not found) is an error.
    ///
    /// # Errors
    /// Returns the launch lookup failure.
    pub fn is_available(&self) -> Result<bool, String> {
        match launch::resolve_launch_prefix(self.runtime_settings.as_ref(), &self.base_env) {
            Ok(_) => Ok(true),
            Err(message) if message == launch::CODEX_NOT_FOUND_MESSAGE => Ok(false),
            Err(message) => Err(message),
        }
    }

    /// `resolveDefaultModeId(input)`: `auto-review` when
    /// `resolveAutoReviewEnabled(input.signal)` is true, else `auto`. With a
    /// signal (`abort` is its `aborted` check) the auto-review probe is fresh
    /// and, as in Paseo's `probeAutoReviewEnabled`, aborts after the launch
    /// prefix probe and after the version probe; without one the memo is
    /// used.
    ///
    /// # Errors
    /// Returns [`Aborted`] when the signal aborted; the caller raises
    /// `signal.reason`, as Paseo rethrows it.
    pub fn resolve_default_mode_id(
        &self,
        abort: Option<launch::AbortCheck<'_>>,
    ) -> Result<&'static str, Aborted> {
        let enabled = self.resolve_auto_review_enabled(abort).ok_or(Aborted)?;
        Ok(if enabled {
            "auto-review"
        } else {
            DEFAULT_CODEX_MODE_ID
        })
    }

    /// `resolveGoalsEnabled()`: probed on first use, then memoized.
    fn resolve_goals_enabled(&self) -> bool {
        *self.goals_enabled.get_or_init(|| {
            launch::probe_goals_enabled(self.runtime_settings.as_ref(), &self.base_env)
        })
    }

    /// `resolveAutoReviewEnabled(signal)`: memoized only without a signal;
    /// with one (`abort` is its `aborted` check), a fresh probe on every
    /// call, and `None` when the signal aborted.
    fn resolve_auto_review_enabled(&self, abort: Option<launch::AbortCheck<'_>>) -> Option<bool> {
        if abort.is_some() {
            return launch::probe_auto_review_abortable(
                self.runtime_settings.as_ref(),
                &self.base_env,
                abort,
            );
        }
        Some(
            *self
                .auto_review_enabled
                .get_or_init(|| self.probe_auto_review_enabled(None).unwrap_or(false)),
        )
    }

    fn probe_auto_review_enabled(&self, deadline: Option<Instant>) -> Option<bool> {
        launch::probe_auto_review_enabled(self.runtime_settings.as_ref(), &self.base_env, deadline)
    }

    /// [`Self::fetch_catalog_signalled`] where a deadline is the signal: a
    /// deadline means a signal is present.
    ///
    /// # Errors
    /// As [`Self::fetch_catalog_signalled`].
    pub fn fetch_catalog(&self, deadline: Option<Instant>) -> Result<Value, String> {
        self.fetch_catalog_signalled(deadline, deadline.is_some())
    }

    /// `fetchCatalog(options, context)`. Two branches run concurrently, as
    /// Paseo's `Promise.all` runs them: a short-lived app-server (no launch
    /// env, no `--enable goals`) for `model/list` and configured defaults,
    /// and `resolveAutoReviewEnabled(context?.signal)`. `signal_present` is
    /// whether the refresh context carries a signal: with one, auto-review is
    /// probed afresh; without, the memo is used and filled. With a deadline,
    /// the app-server is disposed when it passes, as Paseo disposes it on
    /// the signal's abort.
    ///
    /// # Errors
    /// Returns launch, initialize, or `model/list` failures, or
    /// [`CATALOG_DEADLINE_MESSAGE`] when the deadline passed first.
    pub fn fetch_catalog_signalled(
        &self,
        deadline: Option<Instant>,
        signal_present: bool,
    ) -> Result<Value, String> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(CATALOG_DEADLINE_MESSAGE.to_owned());
        }
        thread::scope(|scope| {
            let auto_review = scope.spawn(|| {
                if signal_present {
                    self.probe_auto_review_enabled(deadline)
                } else {
                    self.resolve_auto_review_enabled(None)
                }
            });
            let models = self.catalog_models(deadline);
            // `Promise.all` settles with the first rejection; the probe
            // still runs to its end, which the scope waits for.
            let auto_review = auto_review.join().unwrap_or(Some(false));
            let models = models?;
            let auto_review_enabled =
                auto_review.ok_or_else(|| CATALOG_DEADLINE_MESSAGE.to_owned())?;
            Ok(catalog::catalog(models, auto_review_enabled))
        })
    }

    /// `fetchModelsFromAppServer(context)`.
    fn catalog_models(&self, deadline: Option<Instant>) -> Result<Vec<Value>, String> {
        let prefix = launch::resolve_launch_prefix(self.runtime_settings.as_ref(), &self.base_env)?;
        let env = launch::provider_env(&self.base_env, self.runtime_settings.as_ref(), None);
        let child = launch::spawn_app_server(&prefix, false, &env)?;
        let client = AppServerClient::new(child).map_err(|error| error.message)?;
        let aborted = Arc::new(AtomicBool::new(false));
        let (finished, watch) = mpsc::channel::<()>();
        let watchdog = deadline.map(|deadline| {
            let client = client.clone();
            let aborted = Arc::clone(&aborted);
            thread::spawn(move || {
                let wait = deadline.saturating_duration_since(Instant::now());
                if matches!(
                    watch.recv_timeout(wait),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    aborted.store(true, Ordering::SeqCst);
                    let _ = client.dispose();
                }
            })
        });
        let models = client
            .request(
                "initialize",
                Some(launch::initialize_params()),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map_err(|error| error.message)
            .and_then(|_| {
                client.notify("initialized", Some(json!({})));
                catalog::models_from_app_server(&client)
            });
        drop(finished);
        if let Some(watchdog) = watchdog {
            let _ = watchdog.join();
        }
        if aborted.load(Ordering::SeqCst) {
            return Err(CATALOG_DEADLINE_MESSAGE.to_owned());
        }
        let disposed = client.dispose().map_err(|error| error.message);
        let models = models?;
        disposed?;
        Ok(models)
    }

    /// `updateNativeThreadArchiveState(handle, state)`: a short-lived
    /// app-server (as `spawnAppServer()` with no launch env and no goals),
    /// `initialize`, then `thread/archive`, or `thread/unarchive`. A restore
    /// of a thread that is not archived (`isCodexAlreadyUnarchivedError`)
    /// is settled by a `thread/read` of it: when that succeeds the first
    /// error is swallowed, else the first error is the result. The client is
    /// always disposed. An empty `thread_id` (`nativeHandle ?? sessionId`
    /// found none) returns without spawning.
    ///
    /// # Errors
    /// Returns launch, initialize, or request failures.
    pub fn update_native_thread_archive_state(
        &self,
        thread_id: &str,
        state: NativeArchiveState,
    ) -> Result<(), String> {
        if thread_id.is_empty() {
            return Ok(());
        }
        let prefix = launch::resolve_launch_prefix(self.runtime_settings.as_ref(), &self.base_env)?;
        let env = launch::provider_env(&self.base_env, self.runtime_settings.as_ref(), None);
        let child = launch::spawn_app_server(&prefix, false, &env)?;
        let client = AppServerClient::new(child).map_err(|error| error.message)?;
        let outcome = client
            .request(
                "initialize",
                Some(launch::initialize_params()),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map_err(|error| error.message)
            .and_then(|_| {
                client.notify("initialized", Some(json!({})));
                archive_state_requests(&client, thread_id, state)
            });
        let disposed = client.dispose().map_err(|error| error.message);
        outcome?;
        disposed
    }

    fn spawner(
        &self,
        launch_env: Option<BTreeMap<String, String>>,
        gates: CodexGates,
    ) -> SpawnAppServer {
        let settings = self.runtime_settings.clone();
        let base_env = self.base_env.clone();
        Box::new(move || {
            let prefix = launch::resolve_launch_prefix(settings.as_ref(), &base_env)?;
            let env = launch::provider_env(&base_env, settings.as_ref(), launch_env.as_ref());
            launch::spawn_app_server(&prefix, gates.goals_enabled, &env)
        })
    }

    /// `resumeSession(handle, overrides, launchContext, { purpose })`: the
    /// stored metadata overlaid with `overrides`, `cwd` falling back to the
    /// daemon's working directory, then connected (which resumes the native
    /// thread and replays its history).
    ///
    /// # Errors
    /// Returns construction, resume, or history failures.
    pub fn resume_session(
        &self,
        handle: &ResumeHandle,
        overrides: &Map<String, Value>,
        launch_env: Option<BTreeMap<String, String>>,
        history_only: bool,
    ) -> Result<CodexSession, String> {
        let mut merged = handle.metadata.clone().unwrap_or_default();
        for (key, value) in overrides {
            merged.insert(key.clone(), value.clone());
        }
        merged.insert("provider".to_owned(), json!(CODEX_PROVIDER));
        let cwd = overrides
            .get("cwd")
            .filter(|cwd| !cwd.is_null())
            .or_else(|| {
                handle
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("cwd"))
                    .filter(|cwd| !cwd.is_null())
            })
            .cloned()
            .unwrap_or_else(|| {
                json!(
                    std::env::current_dir()
                        .map(|dir| dir.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )
            });
        merged.insert("cwd".to_owned(), cwd);
        let gates = self.gates();
        let session = CodexSession::resumed(
            SessionOptions {
                config: SessionConfig::from_json(&merged),
                spawn: self.spawner(launch_env, gates),
                custom_codex_config: launch::custom_provider_config(
                    self.runtime_settings.as_ref(),
                    self.custom_provider.as_ref(),
                ),
                ephemeral: false,
                gates,
            },
            handle,
            history_only,
        )?;
        session.connect()?;
        Ok(session)
    }

    /// `createSession(config, launchContext, options)`: constructs and
    /// connects a session.
    ///
    /// # Errors
    /// Returns construction or connection failures.
    pub fn create_session(
        &self,
        config: SessionConfig,
        launch_env: Option<BTreeMap<String, String>>,
        ephemeral: bool,
    ) -> Result<CodexSession, String> {
        let gates = self.gates();
        let session = CodexSession::new(SessionOptions {
            config,
            spawn: self.spawner(launch_env, gates),
            custom_codex_config: launch::custom_provider_config(
                self.runtime_settings.as_ref(),
                self.custom_provider.as_ref(),
            ),
            ephemeral,
            gates,
        })?;
        session.connect()?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_session() -> CodexSession {
        let session = CodexSession::new(SessionOptions {
            config: SessionConfig {
                cwd: "/w".to_owned(),
                ..SessionConfig::default()
            },
            spawn: Box::new(|| Err("no app-server in unit tests".to_owned())),
            custom_codex_config: None,
            ephemeral: false,
            gates: CodexGates {
                goals_enabled: false,
                auto_review_enabled: false,
            },
        })
        .expect("session");
        {
            let mut state = lock(&session.inner.state);
            state.current_thread_id = Some("t".to_owned());
            state.active_foreground_turn_id = Some("codex-turn-0".to_owned());
        }
        session
    }

    #[test]
    fn subscribers_run_on_the_dispatch_thread_and_panics_are_isolated() {
        let session = bare_session();
        session.subscribe(Arc::new(|_| panic!("subscriber panic")));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let reentrant = session.clone();
        let sink = Arc::clone(&seen);
        let caller = thread::current().id();
        session.subscribe(Arc::new(move |event: &Value| {
            // Calling back into the session from a subscriber must not
            // deadlock: no lock is held while subscribers run.
            let _ = reentrant.pending_permissions();
            let _ = reentrant.interrupt();
            assert_ne!(thread::current().id(), caller);
            sink.lock().unwrap().push(event["type"].clone());
        }));
        session.handle_notification("turn/started", Some(&json!({"turn": {"id": "n1"}})));
        session.handle_notification(
            "turn/completed",
            Some(&json!({"turn": {"status": "completed"}})),
        );
        session.flush_dispatch(None);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![json!("turn_started"), json!("turn_completed")]
        );
        session.close().expect("close");
    }

    #[test]
    fn unported_paths_are_recorded_once() {
        let session = bare_session();
        for _ in 0..3 {
            session.handle_notification(
                "item/agentMessage/delta",
                Some(&json!({"threadId": "child", "itemId": "c", "delta": "z"})),
            );
        }
        session.handle_notification(
            "item/completed",
            Some(&json!({"threadId": "child", "item": {"type": "agentMessage"}})),
        );
        session.handle_notification(
            "codex/event/item_completed",
            Some(&json!({"thread_id": "child", "msg": {"type": "item_completed", "item": {}}})),
        );
        assert_eq!(
            session.unported(),
            [
                "sub-agent thread notification agent_message_delta",
                "sub-agent thread notification item_completed",
            ],
            "raw Codex method names never become separate records"
        );
        session.close().expect("close");
    }

    #[test]
    fn catalog_fetch_stops_at_its_deadline() {
        // An app-server that never answers `initialize`.
        let provider = CodexProvider::new(
            Some(ProviderRuntimeSettings {
                command: Some(crate::launch::ProviderCommand::Replace {
                    argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
                }),
                env: None,
            }),
            None,
            std::env::vars_os().collect(),
        );
        let started = Instant::now();
        assert_eq!(
            provider.fetch_catalog(Some(Instant::now() + Duration::from_millis(500))),
            Err(CATALOG_DEADLINE_MESSAGE.to_owned())
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            provider.fetch_catalog(Some(Instant::now())),
            Err(CATALOG_DEADLINE_MESSAGE.to_owned())
        );
    }

    #[test]
    fn a_repeated_approval_id_replaces_the_pending_request_in_place() {
        // A `cat` child echoes each client request back as a server request,
        // which hands this test real responders.
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec cat"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("cat");
        let client = AppServerClient::new(child).expect("client");
        let (captured, responders) = mpsc::channel();
        let captured = Mutex::new(captured);
        client.set_request_handler(
            "approval",
            Arc::new(move |_, _, responder| {
                let _ = captured.lock().unwrap().send(responder);
            }),
        );
        let first = {
            let client = client.clone();
            thread::spawn(move || client.request("approval", None, Duration::from_secs(10)))
        };
        let first_responder = responders.recv().expect("first responder");
        let session = bare_session();
        let params = json!({"itemId": "i", "threadId": "t", "turnId": "u", "command": "ls"});
        session.handle_approval_request(PermissionKind::Command, Some(&params), first_responder);
        let second = {
            let client = client.clone();
            thread::spawn(move || client.request("approval", None, Duration::from_secs(10)))
        };
        let second_responder = responders.recv().expect("second responder");
        let params = json!({"itemId": "i", "threadId": "t", "turnId": "u", "command": "pwd"});
        session.handle_approval_request(PermissionKind::Command, Some(&params), second_responder);

        let pending = session.pending_permissions();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["title"], json!("Run command: pwd"));
        assert_eq!(
            first.join().unwrap().map_err(|error| error.message),
            Err(crate::transport::DROPPED_REQUEST_MESSAGE.to_owned()),
            "the replaced responder answers instead of leaving Codex waiting"
        );
        session.close().expect("close");
        assert!(session.pending_permissions().is_empty());
        assert_eq!(
            second.join().unwrap(),
            Ok(json!({"decision": "cancel"})),
            "close answers open approvals with cancel"
        );
        client.dispose().expect("dispose");
    }

    #[test]
    fn an_empty_resume_session_id_starts_without_a_thread() {
        let options = SessionOptions {
            config: SessionConfig {
                cwd: "/w".to_owned(),
                ..SessionConfig::default()
            },
            spawn: Box::new(|| Err("no app-server in unit tests".to_owned())),
            custom_codex_config: None,
            ephemeral: false,
            gates: CodexGates {
                goals_enabled: false,
                auto_review_enabled: false,
            },
        };
        let handle = ResumeHandle {
            session_id: String::new(),
            metadata: None,
        };
        let session = CodexSession::resumed(options, &handle, false).expect("session");
        assert_eq!(session.id(), None);
        assert!(session.stream_history().is_empty());
    }

    #[test]
    fn elicitations_paseo_declines_are_declined_without_a_record() {
        let decline = json!({"action": "decline", "content": null, "_meta": null});
        let base = json!({"threadId": "t", "serverName": "s", "mode": "form", "message": "m"});
        let with = |key: &str, value: Value| {
            let mut params = base.clone();
            params[key] = value;
            params
        };
        assert_eq!(
            paseo_declined_elicitation(Some(&with("mode", json!("url")))),
            Some(decline.clone())
        );
        assert_eq!(
            paseo_declined_elicitation(Some(&with("requestedSchema", json!({"required": ["a"]})))),
            Some(decline)
        );
        assert_eq!(paseo_declined_elicitation(Some(&base)), None);
        assert_eq!(
            paseo_declined_elicitation(Some(&with("requestedSchema", json!({"required": []})))),
            None
        );
        assert_eq!(
            paseo_declined_elicitation(Some(
                &with("mode", json!("url"))
                    .as_object()
                    .map(|params| {
                        let mut params = params.clone();
                        params.remove("serverName");
                        Value::Object(params)
                    })
                    .unwrap()
            )),
            None,
            "params failing Paseo's schema are not a Paseo decline"
        );
    }

    #[test]
    fn unported_server_requests_are_answered_not_left_waiting() {
        assert_eq!(
            unported_request_reply("mcpServer/elicitation/request"),
            json!({"action": "decline", "content": null, "_meta": null})
        );
        assert_eq!(
            unported_request_reply("item/tool/requestUserInput"),
            json!({"answers": {}})
        );
        assert_eq!(
            unported_request_reply("tool/requestUserInput"),
            json!({"answers": {}})
        );
    }

    #[test]
    fn invalid_mode_message_lists_modes_in_paseo_order() {
        assert_eq!(
            validate_mode("yolo"),
            Err("Invalid Codex mode \"yolo\". Valid modes are: read-only, auto, auto-review, full-access".to_owned())
        );
    }

    #[test]
    fn slash_command_parsing() {
        assert_eq!(parse_slash_command("/compact"), Some("compact".to_owned()));
        assert_eq!(
            parse_slash_command("  /goal ship it"),
            Some("goal".to_owned())
        );
        assert_eq!(parse_slash_command("/"), None);
        assert_eq!(parse_slash_command("/a/b"), None);
        assert_eq!(parse_slash_command("hello /x"), None);
    }

    #[test]
    fn plan_markdown_prefixes() {
        assert_eq!(
            plan_steps_to_markdown(&[
                "Inspect".to_owned(),
                "- listed".to_owned(),
                "## heading".to_owned(),
                "2. numbered".to_owned(),
                "  ".to_owned(),
            ]),
            "- Inspect\n- listed\n## heading\n2. numbered"
        );
    }

    #[test]
    fn sandbox_policies_match_paseo_shapes() {
        assert_eq!(
            serde_json::to_string(&to_sandbox_policy("workspace-write", &Map::new())).unwrap(),
            r#"{"type":"workspaceWrite","networkAccess":false,"writableRoots":[],"excludeSlashTmp":false,"excludeTmpdirEnvVar":false}"#
        );
        assert_eq!(
            to_sandbox_policy("danger-full-access", &Map::new()),
            json!({"type": "dangerFullAccess"})
        );
    }

    // Paseo: "preapproves only granted tools on the injected Codex MCP server".
    #[test]
    fn tool_policy_preapproves_granted_tools() {
        let mut config = Map::new();
        config.insert(
            "mcp_servers".to_owned(),
            json!({"paseo": {"url": "http://127.0.0.1:1/mcp/agents"}}),
        );
        let configured = apply_codex_tool_policy(
            config,
            Some(&json!({"preapproved": [{"server": "paseo", "tool": "list_agents"}]})),
        );
        assert_eq!(
            Value::Object(configured),
            json!({"mcp_servers": {"paseo": {
                "url": "http://127.0.0.1:1/mcp/agents",
                "enabled_tools": ["list_agents"],
                "default_tools_approval_mode": "prompt",
                "tools": {"list_agents": {"approval_mode": "approve"}}
            }}})
        );
    }

    #[test]
    fn collaboration_mode_resolution_against_codex_0_159_modes() {
        let modes = parse_collaboration_modes(&json!({"data": [
            {"name": "Plan", "mode": "plan", "model": null, "reasoning_effort": "medium"},
            {"name": "Default", "mode": "default", "model": null, "reasoning_effort": null}
        ]}));
        assert_eq!(
            find_collaboration_mode(&modes, false).map(|m| m.name.as_str()),
            Some("Default")
        );
        assert_eq!(
            find_collaboration_mode(&modes, true).map(|m| m.name.as_str()),
            Some("Plan")
        );
    }

    /// A provider whose Codex is a script that logs its argv and prints
    /// `codex-cli 0.159.0`.
    fn recording_provider(dir: &std::path::Path) -> (CodexProvider, std::path::PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let log = dir.join("argv.log");
        let script = dir.join("codex");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\necho 'codex-cli 0.159.0'\n",
                log.display()
            ),
        )
        .unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&script)
            .status()
            .unwrap();
        let provider = CodexProvider::new(
            Some(ProviderRuntimeSettings {
                command: Some(launch::ProviderCommand::Replace {
                    argv: vec![script.to_string_lossy().into_owned()],
                }),
                env: None,
            }),
            None,
            std::env::vars_os().collect(),
        );
        (provider, log)
    }

    fn probes(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "--version")
            .count()
    }

    #[test]
    fn launch_gates_memoize_as_paseo_does() {
        let dir = std::env::temp_dir().join(format!("spocky-gate-memo-{}", std::process::id()));
        let (provider, log) = recording_provider(&dir);
        assert_eq!(provider.is_available(), Ok(true));
        assert_eq!(probes(&log), 1, "isAvailable: one prefix probe");
        // A signalled auto-review probe never fills the memo.
        assert_eq!(
            provider.resolve_auto_review_enabled(Some(&|| false)),
            Some(true)
        );
        assert_eq!(
            provider.resolve_auto_review_enabled(Some(&|| false)),
            Some(true)
        );
        assert_eq!(probes(&log), 5, "two fresh prefix plus version probes");
        assert_eq!(
            provider.gates(),
            CodexGates {
                goals_enabled: true,
                auto_review_enabled: true
            }
        );
        assert_eq!(
            probes(&log),
            9,
            "first create: goals and auto-review probed"
        );
        provider.gates();
        assert_eq!(provider.resolve_auto_review_enabled(None), Some(true));
        assert_eq!(
            probes(&log),
            9,
            "later creates and unsignalled calls hit both memos"
        );
        assert_eq!(
            provider.resolve_auto_review_enabled(Some(&|| false)),
            Some(true)
        );
        assert_eq!(probes(&log), 11, "a signal still probes afresh");
        assert_eq!(provider.resolve_default_mode_id(None), Ok("auto-review"));
        assert_eq!(probes(&log), 11, "no signal: the memo answers");
        assert_eq!(
            provider.resolve_default_mode_id(Some(&|| false)),
            Ok("auto-review")
        );
        assert_eq!(probes(&log), 13, "a signal probes afresh");
        assert_eq!(
            provider.resolve_default_mode_id(Some(&|| true)),
            Err(Aborted),
            "an aborted signal raises"
        );
        assert_eq!(probes(&log), 14, "only the launch prefix probe ran");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_model_counts_as_unset_like_javascript_falsiness() {
        assert_eq!(unset_if_empty(Some(String::new())), None);
        assert_eq!(unset_if_empty(None), None);
        assert_eq!(
            unset_if_empty(Some("gpt-6-astra".to_owned())),
            Some("gpt-6-astra".to_owned())
        );
    }

    #[test]
    fn string_coercions_leave_the_value_domain_as_utf8() {
        assert_eq!(js_string(&json!("a\u{10FFFF}b")), "a\u{10FFFF}b");
        assert_eq!(js_string(&json!(["\u{10FFFF}", "x"])), "\u{10FFFF},x");
        assert_eq!(
            property_key(Some(&json!("srv\u{10FFFF}\u{10FFFF}"))),
            "srv\u{10FFFF}\u{10FFFF}"
        );
    }

    #[test]
    fn approval_policy_fallback_is_javascript_string_of_the_value() {
        // `String(providerOptions.approval_policy ?? "")`: arrays join with
        // commas and numbers print as JavaScript prints them, where the old
        // helper wrote their JSON.
        assert_eq!(js_string(&Value::Null), "");
        assert_eq!(js_string(&json!("on-request")), "on-request");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(2.0)), "2");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(["a", 1, null, ["b", 2]])), "a,1,,b,2");
        assert_eq!(js_string(&json!({"a": 1})), "[object Object]");
    }

    #[test]
    fn the_sandbox_fallback_is_compared_raw_not_stringified() {
        let ws = "workspace-write";
        assert!(sandbox_is_workspace_write(Some(ws), None));
        assert!(!sandbox_is_workspace_write(
            Some("read-only"),
            Some(&json!(ws))
        ));
        assert!(sandbox_is_workspace_write(None, Some(&json!(ws))));
        // `String(["workspace-write"])` is "workspace-write", but the array
        // itself is not `=== "workspace-write"`.
        assert!(!sandbox_is_workspace_write(None, Some(&json!([ws]))));
        assert!(!sandbox_is_workspace_write(None, Some(&json!({"a": ws}))));
        assert!(!sandbox_is_workspace_write(None, Some(&json!(1))));
        assert!(!sandbox_is_workspace_write(None, Some(&Value::Null)));
        assert!(!sandbox_is_workspace_write(None, None));
    }

    #[test]
    fn promotion_needs_the_reviewer_the_policy_and_the_sandbox() {
        let promote = should_promote_thread_response_to_auto_review;
        assert!(promote(Some("auto_review"), "on-request", true));
        assert!(promote(Some("guardian_subagent"), "on-request", true));
        assert!(!promote(Some("user"), "on-request", true));
        assert!(!promote(None, "on-request", true));
        assert!(!promote(Some("auto_review"), "never", true));
        assert!(!promote(Some("auto_review"), "on-request", false));
    }

    #[test]
    fn tool_policy_uses_grant_values_raw_and_coerces_only_property_keys() {
        // Paseo keeps `grant.server` and `grant.tool` as given: the `Map` is
        // keyed by the server value itself and `enabled_tools` holds the tool
        // values; only the object keys are `String(value)`.
        let configured = apply_codex_tool_policy(
            Map::new(),
            Some(&json!({"preapproved": [
                {"server": "a", "tool": 1},
                {"server": "a", "tool": ["x", "y"]},
                {"tool": "t"}
            ]})),
        );
        assert_eq!(
            serde_json::to_string(&Value::Object(configured)).unwrap(),
            r#"{"mcp_servers":{"a":{"enabled_tools":[1,["x","y"]],"default_tools_approval_mode":"prompt","tools":{"1":{"approval_mode":"approve"},"x,y":{"approval_mode":"approve"}}},"undefined":{"enabled_tools":["t"],"default_tools_approval_mode":"prompt","tools":{"t":{"approval_mode":"approve"}}}}}"#
        );
    }
}
