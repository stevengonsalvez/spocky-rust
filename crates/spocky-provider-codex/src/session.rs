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
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::catalog::{self, normalize_thinking, read_configured_defaults};
use crate::items::{
    self, AsyncQuestionItem, AsyncQuestionResolution, ThreadItemMapping, async_question_permission,
    async_question_record, async_question_timeline, item_type, non_empty_string, plan_tool_call,
    plan_update_to_todo, thread_item_to_timeline, to_agent_usage,
};
use crate::launch::{self, CODEX_PROVIDER, CodexGates, CustomProvider, ProviderRuntimeSettings};
use crate::notification::{ItemSource, ParsedNotification, parse_notification};
use crate::transport::{AppServerClient, ClientError, DEFAULT_REQUEST_TIMEOUT, js_trim};

const TURN_START_TIMEOUT: Duration = Duration::from_millis(90 * 1000);
const INTERRUPT_TIMEOUT: Duration = Duration::from_millis(2_000);
const ASSISTANT_MESSAGE_BOUNDARY_MARKDOWN: &str = "\n\n---\n\n";
const DEFAULT_CODEX_MODE_ID: &str = "auto";
const CLOSED_MESSAGE: &str = "Codex app-server session is closed";

/// Approval request methods Paseo registers handlers for.
const APPROVAL_REQUEST_METHODS: [&str; 5] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
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
    /// Already validated `CodexProviderOptions`.
    pub provider_options: Option<Map<String, Value>>,
    /// `ToolPolicy` (`{ preapproved: [{ server, tool }] }`).
    pub tool_policy: Option<Value>,
    /// `Record<string, McpServerConfig>`.
    pub mcp_servers: Option<Map<String, Value>>,
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
    Connected,
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
    connection: ConnectionState,
    connecting: bool,
    connect_error: Option<String>,
    closed: bool,
    collaboration_modes: Vec<CollaborationMode>,
    resolved_collaboration_mode: Option<ResolvedCollaborationMode>,
    unported: Vec<String>,
}

struct Inner {
    state: Mutex<State>,
    state_changed: Condvar,
    subscribers: Mutex<Vec<(u64, Subscriber)>>,
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
    /// Returns the `Invalid Codex mode` message for an unknown `mode_id`.
    pub fn new(options: SessionOptions) -> Result<Self, String> {
        let mut config = options.config;
        if let Some(mode_id) = &config.mode_id {
            validate_mode(mode_id)?;
        }
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
        let mut unported = Vec::new();
        if plan_mode_enabled {
            unported.push("plan_mode feature".to_owned());
        }
        let state = State {
            config,
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
            connection: ConnectionState::Disconnected,
            connecting: false,
            connect_error: None,
            closed: false,
            collaboration_modes: Vec::new(),
            resolved_collaboration_mode: None,
            unported,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                state_changed: Condvar::new(),
                subscribers: Mutex::new(Vec::new()),
                next_subscriber: AtomicU64::new(0),
                spawn: options.spawn,
                custom_codex_config: options.custom_codex_config,
                ephemeral: options.ephemeral,
                gates: options.gates,
            }),
        })
    }

    /// Registers a stream event subscriber; returns its id.
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
        lock(&self.inner.state).unported.clone()
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

    /// Pending async questions as permission requests.
    #[must_use]
    pub fn pending_permissions(&self) -> Vec<Value> {
        lock(&self.inner.state)
            .async_questions
            .iter()
            .filter(|record| record.resolution.is_none())
            .map(|record| async_question_permission(&record.item))
            .collect()
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
            metadata.insert("providerOptions".to_owned(), Value::Object(options.clone()));
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
        let mut state = lock(&self.inner.state);
        if state.closed {
            return Err(CLOSED_MESSAGE.to_owned());
        }
        state.connection = ConnectionState::Connected;
        Ok(())
    }

    fn register_request_handlers(&self, client: &AppServerClient) {
        for method in APPROVAL_REQUEST_METHODS {
            let weak = Arc::downgrade(&self.inner);
            client.set_request_handler(
                method,
                Arc::new(move |_params, _id, responder| {
                    // Paseo holds the request open until the user answers;
                    // the approval flow is not ported, so it stays unanswered.
                    drop(responder);
                    if let Some(session) = upgrade(&weak) {
                        session.record_unported(format!("server request {method}"));
                    }
                }),
            );
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
        let provider_option = |key: &str| {
            state
                .config
                .provider_options
                .as_ref()
                .and_then(|options| options.get(key))
                .map(js_string)
                .unwrap_or_default()
        };
        let approval_policy = approval_policy.unwrap_or_else(|| provider_option("approval_policy"));
        let sandbox = sandbox.unwrap_or_else(|| provider_option("sandbox_mode"));
        if matches!(reviewer, Some("auto_review" | "guardian_subagent"))
            && approval_policy == "on-request"
            && sandbox == "workspace-write"
        {
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
        if let Some(options) = &state.config.provider_options {
            for (key, value) in options {
                inner.insert(key.clone(), value.clone());
            }
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
        let provider_has = |key: &str| {
            state
                .config
                .provider_options
                .as_ref()
                .is_some_and(|options| options.contains_key(key))
        };
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
        let provider_option = |key: &str| {
            state
                .config
                .provider_options
                .as_ref()
                .and_then(|options| options.get(key))
        };
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
        {
            let mut state = lock(&self.inner.state);
            state.closed = true;
            state.active_foreground_turn_id = None;
            state.active_client_message_id = None;
            if let Some(pending) = state.pending_identification.take() {
                pending.slot.resolve(None);
            }
        }
        lock(&self.inner.subscribers).clear();
        let outcome = self.dispose_client();
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
        }
        self.publish(&events);
    }

    fn publish(&self, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let subscribers: Vec<Subscriber> = lock(&self.inner.subscribers)
            .iter()
            .map(|(_, subscriber)| Arc::clone(subscriber))
            .collect();
        for event in events {
            for subscriber in &subscribers {
                subscriber(event);
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
                state
                    .unported
                    .push(format!("sub-agent thread notification {method}"));
            }
        }
        self.publish(&events);
    }

    /// Feeds one notification through the session as the transport would.
    /// Exposed for tests that replay Paseo's notification fixtures.
    #[doc(hidden)]
    pub fn receive_notification(&self, method: &str, params: Option<&Value>) {
        self.handle_notification(method, params);
    }

    /// Puts the session in the state Paseo's `createSession()` test fixture
    /// builds: connected, on `thread_id`, with an optional foreground turn.
    #[doc(hidden)]
    pub fn prime_for_notification_test(&self, thread_id: &str, foreground_turn_id: Option<&str>) {
        let mut state = lock(&self.inner.state);
        state.connection = ConnectionState::Connected;
        state.current_thread_id = Some(thread_id.to_owned());
        state.active_foreground_turn_id = foreground_turn_id.map(str::to_owned);
    }

    /// Pid of the running `codex app-server` child, for process tests.
    #[doc(hidden)]
    #[must_use]
    pub fn app_server_pid(&self) -> Option<u32> {
        lock(&self.inner.state)
            .client
            .as_ref()
            .map(AppServerClient::pid)
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<CodexSession> {
    weak.upgrade().map(|inner| CodexSession { inner })
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String(value ?? "")` for the provider-option fallbacks.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Object(_) => "[object Object]".to_owned(),
        other => other.to_string(),
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
    let parsed_kind = notification_kind(&parsed);
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
        ParsedNotification::ExecCommandStarted { .. }
        | ParsedNotification::ExecCommandCompleted { .. }
        | ParsedNotification::ExecCommandOutputDelta { .. }
        | ParsedNotification::TerminalInteraction { .. }
        | ParsedNotification::PatchApplyStarted { .. }
        | ParsedNotification::PatchApplyCompleted { .. }
        | ParsedNotification::FileChangeOutputDelta { .. } => {
            state
                .unported
                .push(format!("tool notification {parsed_kind}"));
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

fn notification_kind(parsed: &ParsedNotification) -> &'static str {
    match parsed {
        ParsedNotification::ExecCommandStarted { .. } => "exec_command_started",
        ParsedNotification::ExecCommandCompleted { .. } => "exec_command_completed",
        ParsedNotification::ExecCommandOutputDelta { .. } => "exec_command_output_delta",
        ParsedNotification::TerminalInteraction { .. } => "terminal_interaction",
        ParsedNotification::PatchApplyStarted { .. } => "patch_apply_started",
        ParsedNotification::PatchApplyCompleted { .. } => "patch_apply_completed",
        ParsedNotification::FileChangeOutputDelta { .. } => "file_change_output_delta",
        _ => "other",
    }
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
        return step.chars().nth(hashes).is_some_and(char::is_whitespace);
    }
    if let Some(first) = chars.next()
        && matches!(first, '-' | '*' | '+')
    {
        return chars.next().is_some_and(char::is_whitespace);
    }
    let digits = step.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && step[digits..].starts_with('.')
        && step[digits + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn reset_turn_tracking_state(state: &mut State) {
    state.latest_plan_result = None;
    state.emitted_item_started_ids.clear();
    state.emitted_item_completed_ids.clear();
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
    if item_id
        .as_ref()
        .is_some_and(|id| state.emitted_item_completed_ids.contains(id))
    {
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
        state.emitted_item_completed_ids.insert(id);
    }
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
    if id
        .as_ref()
        .is_some_and(|id| state.emitted_item_started_ids.contains(id))
    {
        return;
    }
    emit(state, events, timeline_event(timeline_item));
    if let Some(id) = id {
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
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
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
    let mut grants: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(Value::Array(preapproved)) = policy.get("preapproved") {
        for grant in preapproved {
            let server = grant.get("server").map(js_string).unwrap_or_default();
            let tool = grant.get("tool").map(js_string).unwrap_or_default();
            match grants.iter_mut().find(|(name, _)| *name == server) {
                Some((_, tools)) => tools.push(tool),
                None => grants.push((server, vec![tool])),
            }
        }
    }
    for (server, tools) in grants {
        let mut server_config = match servers.get(&server) {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        let approvals: Map<String, Value> = tools
            .iter()
            .map(|tool| (tool.clone(), json!({"approval_mode": "approve"})))
            .collect();
        server_config.insert("enabled_tools".to_owned(), json!(tools));
        server_config.insert("default_tools_approval_mode".to_owned(), json!("prompt"));
        server_config.insert("tools".to_owned(), Value::Object(approvals));
        servers.insert(server, Value::Object(server_config));
    }
    config.insert("mcp_servers".to_owned(), Value::Object(servers));
    config
}

/// `resolveModelAndThinking()`.
fn resolve_model_and_thinking(
    client: &AppServerClient,
    model: Option<String>,
    thinking: Option<String>,
) -> Result<(String, Option<String>), String> {
    let mut model = model;
    let mut thinking = thinking;
    if model.is_none() || thinking.is_none() {
        let defaults = read_configured_defaults(client);
        model = model.or(defaults.model);
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

/// `CodexAppServerAgentClient`: owns launch settings and the version gates,
/// and creates sessions.
pub struct CodexProvider {
    runtime_settings: Option<ProviderRuntimeSettings>,
    custom_provider: Option<CustomProvider>,
    base_env: Vec<(OsString, OsString)>,
    gates: OnceLock<CodexGates>,
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
            gates: OnceLock::new(),
        }
    }

    /// `resolveGoalsEnabled` and `resolveAutoReviewEnabled`, probed once.
    pub fn gates(&self) -> CodexGates {
        *self
            .gates
            .get_or_init(|| launch::resolve_gates(self.runtime_settings.as_ref(), &self.base_env))
    }

    /// `fetchCatalog()`: a short-lived app-server (no launch env, no
    /// `--enable goals`), `model/list`, configured defaults, then dispose.
    ///
    /// # Errors
    /// Returns launch, initialize, or `model/list` failures.
    pub fn fetch_catalog(&self) -> Result<Value, String> {
        let gates = self.gates();
        let prefix = launch::resolve_launch_prefix(self.runtime_settings.as_ref(), &self.base_env)?;
        let env = launch::provider_env(&self.base_env, self.runtime_settings.as_ref(), None);
        let child = launch::spawn_app_server(&prefix, false, &env)?;
        let client = AppServerClient::new(child).map_err(|error| error.message)?;
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
        let disposed = client.dispose().map_err(|error| error.message);
        let models = models?;
        disposed?;
        Ok(catalog::catalog(models, gates.auto_review_enabled))
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
        let settings = self.runtime_settings.clone();
        let base_env = self.base_env.clone();
        let spawn: SpawnAppServer = Box::new(move || {
            let prefix = launch::resolve_launch_prefix(settings.as_ref(), &base_env)?;
            let env = launch::provider_env(&base_env, settings.as_ref(), launch_env.as_ref());
            launch::spawn_app_server(&prefix, gates.goals_enabled, &env)
        });
        let session = CodexSession::new(SessionOptions {
            config,
            spawn,
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
}
