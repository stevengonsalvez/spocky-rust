//! `ClaudeAgentSession` from `providers/claude/agent.ts`.
//!
//! The session runs on one thread (see [`crate::actor`]), as the baseline
//! runs on one Node event loop: state lives in a `RefCell` and is never
//! borrowed across an await, so code between awaits runs uninterrupted as
//! JavaScript does. The implementation is split by concern across this
//! module's files; each mirrors a group of the baseline's methods.

mod events;
mod history;
mod options;
mod permissions;
mod pump;
mod rewind;
mod timeline;
mod turns;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::time::Duration;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::{AgentError, StreamCallback};

use crate::context_usage::ContextUsageState;
use crate::launch::{ClaudeRuntimeSettings, provider_env};
use crate::local::LocalBoxFuture;
use crate::model_manifest::{
    CLAUDE_DISABLED_THINKING_OPTION_ID, CLAUDE_ULTRACODE_THINKING_OPTION_ID,
    resolve_claude_disabled_thinking_for_model,
};
use crate::models::{
    build_claude_features, claude_model_supports_fast_mode, find_claude_model_context_window,
};
use crate::process::{ChildProcess, TerminateResult, terminate_with_tree_kill};
use crate::sdk_query::{ClaudeQuery, PromptInput, QueryFactory};
use crate::sidechain_tracker::ClaudeSidechainTracker;
use crate::subagents::live_source::ClaudeTaskProtocolSource;
use crate::subagents::workflow_output::read_claude_workflow_result_file;
use crate::task_state::ClaudeTaskState;
use crate::timeline_assembler::TimelineAssembler;

/// `DEFAULT_MODES` ids, in order.
pub const VALID_CLAUDE_MODES: [&str; 5] = [
    "plan",
    "default",
    "acceptEdits",
    "auto",
    "bypassPermissions",
];

/// `DEFAULT_MODES`.
#[must_use]
pub fn default_modes() -> Vec<JsValue> {
    [
        (
            "plan",
            "Plan Mode",
            "Analyze the codebase without executing tools or edits",
        ),
        (
            "default",
            "Always Ask",
            "Prompts for permission the first time a tool is used",
        ),
        (
            "acceptEdits",
            "Accept File Edits",
            "Automatically approves edit-focused tools without prompting",
        ),
        (
            "auto",
            "Auto mode",
            "Uses a model classifier to review permission prompts automatically",
        ),
        (
            "bypassPermissions",
            "Bypass",
            "Skip all permission prompts (use with caution)",
        ),
    ]
    .iter()
    .map(|(id, label, description)| {
        let mut mode = JsObject::new();
        mode.insert("id", text(id));
        mode.insert("label", text(label));
        mode.insert("description", text(description));
        JsValue::Object(mode)
    })
    .collect()
}

/// `CLAUDE_CAPABILITIES`.
#[must_use]
pub fn claude_capabilities() -> JsValue {
    let mut flags = JsObject::new();
    for key in [
        "supportsStreaming",
        "supportsSessionPersistence",
        "supportsSessionListing",
        "supportsDynamicModes",
        "supportsMcpServers",
        "supportsReasoningStream",
        "supportsToolInvocations",
        "supportsRewindConversation",
        "supportsRewindFiles",
        "supportsRewindBoth",
    ] {
        flags.insert(key, JsValue::Bool(true));
    }
    JsValue::Object(flags)
}

pub(crate) fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `ClaudeRewindSdk.forkSession(sessionId, { upToMessageId })`.
pub trait RewindSdk {
    /// Resolves the new session id.
    fn fork_session(
        &self,
        session_id: &str,
        up_to_message_id: &str,
    ) -> LocalBoxFuture<'static, Result<String, AgentError>>;
}

/// `resolveBinary()`.
pub type ResolveBinary = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<String, AgentError>>>;
/// A source of `process.env`, read each time the baseline reads it.
pub type EnvSource = Rc<dyn Fn() -> JsObject>;

/// `ClaudeAgentSessionOptions`.
#[derive(Clone)]
pub struct SessionOptions {
    pub defaults_agents: Option<JsValue>,
    pub runtime_settings: Option<ClaudeRuntimeSettings>,
    pub handle: Option<JsValue>,
    pub agent_id: Option<String>,
    pub launch_env: Option<JsObject>,
    pub persist_session: Option<bool>,
    pub query_factory: Option<QueryFactory>,
    pub resolve_binary: ResolveBinary,
    pub rewind_sdk: Option<Rc<dyn RewindSdk>>,
    pub process_env: EnvSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnState {
    Idle,
    Foreground,
    Autonomous,
}

/// `ToolUseCacheEntry`.
#[derive(Debug, Clone)]
pub(crate) struct ToolUseEntry {
    pub name: String,
    pub server: String,
    pub classification: &'static str,
    pub started: bool,
    pub command_text: Option<String>,
    pub files: Option<Vec<(String, String)>>,
    /// `AgentMetadata`: an object or array.
    pub input: Option<JsValue>,
}

/// A permission request awaiting a response.
pub(crate) struct PendingPermission {
    pub request: JsValue,
    pub resolve: Rc<crate::local::Deferred<Result<JsValue, AgentError>>>,
}

/// `ClaudeRewindTurnAnchor`.
#[derive(Debug, Clone)]
pub(crate) struct RewindAnchor {
    pub user_message_id: String,
    pub assistant_message_id: Option<String>,
}

/// The mutable session fields.
#[allow(clippy::struct_excessive_bools, clippy::struct_field_names)] // The baseline's fields.
pub(crate) struct State {
    pub config: JsObject,
    pub query: Option<Rc<dyn ClaudeQuery>>,
    pub child_process: Option<Rc<ChildProcess>>,
    pub input: Option<Rc<PromptInput>>,
    pub active_foreground_query: Option<Rc<dyn ClaudeQuery>>,
    pub active_foreground_input: Option<Rc<PromptInput>>,
    pub queued_steer_uuids: Vec<String>,
    pub permission_clearing_steer_uuids: HashSet<String>,
    pub claude_session_id: Option<String>,
    pub persistence: Option<JsValue>,
    pub current_mode: String,
    pub plan_resume_mode: Option<String>,
    pub available_modes: Vec<JsValue>,
    pub tool_use_index_to_id: HashMap<String, String>,
    pub tool_use_input_buffers: HashMap<String, String>,
    pub pending_permissions: Vec<(String, PendingPermission)>,
    pub active_foreground_turn_id: Option<String>,
    pub autonomous_turn: Option<String>,
    pub subscribers: Vec<(u64, StreamCallback)>,
    pub next_subscriber: u64,
    pub timeline_assembler: TimelineAssembler,
    pub task_state: ClaudeTaskState,
    pub sidechain_tracker: ClaudeSidechainTracker,
    pub persisted_history: Vec<(JsValue, Option<String>)>,
    pub persisted_provider_subagent_events: Vec<JsValue>,
    pub history_pending: bool,
    pub turn_state: TurnState,
    pub next_turn_ordinal: u64,
    /// The identity of `cancelCurrentTurn`.
    pub cancel_current_turn: Option<u64>,
    pub next_cancel_token: u64,
    pub cached_runtime_info: Option<JsValue>,
    pub last_options_model: Option<String>,
    pub last_runtime_model: Option<String>,
    pub compacting: bool,
    pub compaction_marker_open: bool,
    pub query_pump_running: bool,
    /// Identity of the running pump: a finished pump clears the flag only if
    /// it is still the current one (`queryPumpPromise === pump`).
    pub query_pump_generation: u64,
    pub query_restart_needed: bool,
    pub pending_interrupt_abort: bool,
    pub foreground_has_visible_activity: bool,
    pub active_turn_has_assistant_text: bool,
    pub context_usage: ContextUsageState,
    pub user_message_ids: Vec<String>,
    pub emitted_user_message_ids: HashSet<String>,
    pub rewind_turn_anchors: Vec<RewindAnchor>,
    pub pending_fresh_session_id: Option<String>,
    pub recent_stderr: String,
    pub closed: bool,
}

/// `ClaudeAgentSession`.
pub struct ClaudeSession {
    pub(crate) state: RefCell<State>,
    /// `toolUseCache`, shared with the task protocol source's lookup.
    pub(crate) tool_use_cache: Rc<RefCell<Vec<(String, ToolUseEntry)>>>,
    pub(crate) task_protocol_source: RefCell<ClaudeTaskProtocolSource>,
    pub(crate) options: SessionOptions,
}

fn is_permission_mode(value: Option<&str>) -> bool {
    value.is_some_and(|value| VALID_CLAUDE_MODES.contains(&value))
}

pub(crate) fn valid_modes_list() -> String {
    VALID_CLAUDE_MODES.join(", ")
}

/// `isClaudeThinkingEffort(value)`.
pub(crate) fn is_thinking_effort(value: Option<&str>) -> bool {
    matches!(value, Some("low" | "medium" | "high" | "xhigh" | "max"))
}

/// `assertClaudeThinkingOptionSupported(modelId, thinkingOptionId)`.
pub(crate) fn assert_thinking_option_supported(
    model_id: Option<&str>,
    thinking_option_id: Option<&str>,
) -> Result<(), AgentError> {
    if thinking_option_id != Some(CLAUDE_DISABLED_THINKING_OPTION_ID)
        || resolve_claude_disabled_thinking_for_model(model_id).0
    {
        return Ok(());
    }
    Err(AgentError::new(format!(
        "Thinking option '{}' is not available for model '{}'",
        thinking_option_id.unwrap_or_default(),
        model_id.unwrap_or("default")
    )))
}

/// `isTruthyEnvValue(value)`.
fn is_truthy_env_value(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let normalized = spocky_contracts::text::js_trim(value).to_lowercase();
    !normalized.is_empty() && !matches!(normalized.as_str(), "0" | "false" | "no" | "off")
}

/// `claudeAutoModeUnavailableOn(env)`.
pub(crate) fn auto_mode_unavailable_on(env: &JsObject) -> Option<&'static str> {
    let read = |key: &str| env.get(key).and_then(JsValue::as_str);
    if is_truthy_env_value(read("CLAUDE_CODE_USE_BEDROCK")) {
        return Some("Bedrock");
    }
    if is_truthy_env_value(read("CLAUDE_CODE_USE_VERTEX")) {
        return Some("Vertex");
    }
    None
}

/// `assertClaudeModeCanRun(mode, env)`.
pub(crate) fn assert_mode_can_run(mode: &str, env: &JsObject) -> Result<(), AgentError> {
    if mode != "auto" {
        return Ok(());
    }
    let Some(transport) = auto_mode_unavailable_on(env) else {
        return Ok(());
    };
    let variable = if transport == "Bedrock" {
        "CLAUDE_CODE_USE_BEDROCK"
    } else {
        "CLAUDE_CODE_USE_VERTEX"
    };
    Err(AgentError::new(format!(
        "Claude Auto mode requires the Anthropic API and is not supported when Claude Code uses {transport}. Select another permission mode or unset the {variable} environment variable."
    )))
}

/// `claudeModeCatalog(env)`: `(modes, defaultModeId)`.
#[must_use]
pub fn claude_mode_catalog(env: &JsObject) -> (Vec<JsValue>, &'static str) {
    if auto_mode_unavailable_on(env).is_some() {
        let modes = default_modes()
            .into_iter()
            .filter(|mode| mode.get("id").and_then(JsValue::as_str) != Some("auto"))
            .collect();
        return (modes, "default");
    }
    (default_modes(), "auto")
}

impl ClaudeSession {
    /// `new ClaudeAgentSession(config, options)`.
    ///
    /// # Errors
    ///
    /// The baseline constructor's throws: an unsupported thinking option, a
    /// handle without a session id, or an invalid mode.
    #[allow(clippy::too_many_lines)] // The baseline constructor.
    pub fn new(config: JsObject, options: SessionOptions) -> Result<Rc<Self>, AgentError> {
        let model = config
            .get("model")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        assert_thinking_option_supported(
            model.as_deref(),
            config.get("thinkingOptionId").and_then(JsValue::as_str),
        )?;
        let tool_use_cache: Rc<RefCell<Vec<(String, ToolUseEntry)>>> = Rc::default();
        let lookup_cache = Rc::clone(&tool_use_cache);
        let source = ClaudeTaskProtocolSource::new(
            Box::new(move |id| {
                lookup_cache
                    .borrow()
                    .iter()
                    .find(|(entry_id, _)| entry_id == id)
                    .and_then(|(_, entry)| entry.input.clone())
                    .and_then(|input| input.as_object().cloned())
            }),
            Box::new(read_claude_workflow_result_file),
        );
        let mode_id = config
            .get("modeId")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        let state = State {
            config,
            query: None,
            child_process: None,
            input: None,
            active_foreground_query: None,
            active_foreground_input: None,
            queued_steer_uuids: Vec::new(),
            permission_clearing_steer_uuids: HashSet::new(),
            claude_session_id: None,
            persistence: None,
            current_mode: "default".to_owned(),
            plan_resume_mode: None,
            available_modes: default_modes(),
            tool_use_index_to_id: HashMap::new(),
            tool_use_input_buffers: HashMap::new(),
            pending_permissions: Vec::new(),
            active_foreground_turn_id: None,
            autonomous_turn: None,
            subscribers: Vec::new(),
            next_subscriber: 0,
            timeline_assembler: TimelineAssembler::default(),
            task_state: ClaudeTaskState::default(),
            sidechain_tracker: ClaudeSidechainTracker::default(),
            persisted_history: Vec::new(),
            persisted_provider_subagent_events: Vec::new(),
            history_pending: false,
            turn_state: TurnState::Idle,
            next_turn_ordinal: 1,
            cancel_current_turn: None,
            next_cancel_token: 0,
            cached_runtime_info: None,
            last_options_model: None,
            last_runtime_model: None,
            compacting: false,
            compaction_marker_open: false,
            query_pump_running: false,
            query_pump_generation: 0,
            query_restart_needed: false,
            pending_interrupt_abort: false,
            foreground_has_visible_activity: false,
            active_turn_has_assistant_text: false,
            context_usage: ContextUsageState::new(find_claude_model_context_window(
                model.as_deref(),
            )),
            user_message_ids: Vec::new(),
            emitted_user_message_ids: HashSet::new(),
            rewind_turn_anchors: Vec::new(),
            pending_fresh_session_id: None,
            recent_stderr: String::new(),
            closed: false,
        };
        let session = Rc::new(Self {
            state: RefCell::new(state),
            tool_use_cache,
            task_protocol_source: RefCell::new(source),
            options,
        });
        if let Some(handle) = session.options.handle.clone() {
            let Some(session_id) = handle
                .get("sessionId")
                .filter(|id| spocky_contracts::js::truthy(Some(id)))
                .map(|id| spocky_contracts::js::js_string(Some(id)))
            else {
                return Err(AgentError::new(
                    "Cannot resume: persistence handle has no sessionId",
                ));
            };
            {
                let mut state = session.state.borrow_mut();
                state.claude_session_id = Some(session_id.clone());
                state.persistence = Some(handle);
            }
            session.load_persisted_history(&session_id);
        }
        if let Some(mode) = mode_id.as_deref().filter(|mode| !mode.is_empty())
            && !is_permission_mode(Some(mode))
        {
            return Err(AgentError::new(format!(
                "Invalid mode '{mode}' for Claude provider. Valid modes: {}",
                valid_modes_list()
            )));
        }
        {
            let mut state = session.state.borrow_mut();
            state.current_mode = if is_permission_mode(mode_id.as_deref()) {
                mode_id.clone().unwrap_or_default()
            } else {
                "default".to_owned()
            };
            if state.current_mode != "plan" {
                state.plan_resume_mode = Some(state.current_mode.clone());
            }
        }
        Ok(session)
    }

    pub(crate) fn config_str(&self, key: &str) -> Option<String> {
        self.state
            .borrow()
            .config
            .get(key)
            .and_then(JsValue::as_str)
            .map(str::to_owned)
    }

    /// `id`.
    #[must_use]
    pub fn id(&self) -> Option<String> {
        self.state.borrow().claude_session_id.clone()
    }

    fn fast_mode_enabled(&self) -> bool {
        self.state
            .borrow()
            .config
            .get("featureValues")
            .and_then(|values| values.get("fast_mode"))
            == Some(&JsValue::Bool(true))
    }

    /// `features`.
    #[must_use]
    pub fn features(&self) -> Vec<JsValue> {
        build_claude_features(
            self.config_str("model").as_deref(),
            self.fast_mode_enabled(),
        )
    }

    /// `buildSdkEnv()`.
    pub(crate) fn build_sdk_env(&self) -> JsObject {
        provider_env(
            &(self.options.process_env)(),
            self.options.runtime_settings.as_ref(),
            self.options.launch_env.as_ref(),
        )
    }

    /// `getUsageReference()`.
    #[must_use]
    pub fn get_usage_reference(&self) -> Option<JsValue> {
        let env = self.build_sdk_env();
        let read = |key: &str| {
            env.get(key)
                .and_then(JsValue::as_str)
                .filter(|value| !value.is_empty())
        };
        if read("ANTHROPIC_BASE_URL").is_some()
            || read("ANTHROPIC_API_KEY").is_some()
            || read("ANTHROPIC_AUTH_TOKEN").is_some()
        {
            return None;
        }
        let config_dir = read("CLAUDE_CONFIG_DIR").map_or_else(
            || {
                let home = read("HOME")
                    .map(str::to_owned)
                    .or_else(|| std::env::var("HOME").ok())
                    .unwrap_or_default();
                crate::project_dir::join_path(&home, ".claude")
            },
            str::to_owned,
        );
        let mut input = JsObject::new();
        input.insert("configDir", JsValue::String(config_dir));
        let mut reference = JsObject::new();
        reference.insert("source", text("claude"));
        reference.insert("input", JsValue::Object(input));
        Some(JsValue::Object(reference))
    }

    /// `getRuntimeInfo()`.
    #[must_use]
    pub fn get_runtime_info(&self) -> JsValue {
        let mut state = self.state.borrow_mut();
        if let Some(info) = &state.cached_runtime_info {
            return JsValue::Object(spocky_contracts::js::spread(Some(info)));
        }
        let mut info = JsObject::new();
        info.insert("provider", text("claude"));
        info.insert(
            "sessionId",
            state
                .claude_session_id
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        info.insert(
            "model",
            state
                .last_options_model
                .clone()
                .map_or(JsValue::Null, JsValue::String),
        );
        info.insert("modeId", text(&state.current_mode));
        if let Some(runtime_model) = state
            .last_runtime_model
            .clone()
            .filter(|model| !model.is_empty())
        {
            let mut extra = JsObject::new();
            extra.insert("runtimeModel", JsValue::String(runtime_model));
            info.insert("extra", JsValue::Object(extra));
        }
        let info = JsValue::Object(info);
        state.cached_runtime_info = Some(info.clone());
        JsValue::Object(spocky_contracts::js::spread(Some(&info)))
    }

    /// `getAvailableModes()`.
    #[must_use]
    pub fn get_available_modes(&self) -> Vec<JsValue> {
        self.state.borrow().available_modes.clone()
    }

    /// `getCurrentMode()`.
    #[must_use]
    pub fn get_current_mode(&self) -> Option<String> {
        Some(self.state.borrow().current_mode.clone())
    }

    /// `subscribe(callback)`: the subscription id for [`Self::unsubscribe`].
    pub fn subscribe(&self, callback: StreamCallback) -> u64 {
        let mut state = self.state.borrow_mut();
        let id = state.next_subscriber;
        state.next_subscriber += 1;
        state.subscribers.push((id, callback));
        id
    }

    /// The function `subscribe` returns.
    pub fn unsubscribe(&self, id: u64) {
        self.state
            .borrow_mut()
            .subscribers
            .retain(|(existing, _)| *existing != id);
    }

    /// `describePersistence()`.
    pub fn describe_persistence(&self) -> Option<JsValue> {
        let mut state = self.state.borrow_mut();
        if let Some(persistence) = &state.persistence {
            return Some(persistence.clone());
        }
        let session_id = state.claude_session_id.clone()?;
        let mut handle = JsObject::new();
        handle.insert("provider", text("claude"));
        handle.insert("sessionId", text(&session_id));
        handle.insert("nativeHandle", text(&session_id));
        handle.insert(
            "metadata",
            JsValue::Object(spocky_contracts::js::spread(Some(&JsValue::Object(
                state.config.clone(),
            )))),
        );
        let handle = JsValue::Object(handle);
        state.persistence = Some(handle.clone());
        Some(handle)
    }

    /// `notifySubscribers(event)`: tags the event with the active turn and
    /// delivers it to every subscriber, catching panics.
    pub(crate) fn notify_subscribers(&self, event: JsValue) {
        let (tagged, subscribers) = {
            let state = self.state.borrow();
            let turn_id = state
                .active_foreground_turn_id
                .clone()
                .or_else(|| state.autonomous_turn.clone());
            let tagged = match turn_id {
                Some(turn_id) => {
                    let mut object = spocky_contracts::js::spread(Some(&event));
                    object.insert("turnId", JsValue::String(turn_id));
                    JsValue::Object(object)
                }
                None => event,
            };
            let subscribers: Vec<StreamCallback> = state
                .subscribers
                .iter()
                .map(|(_, callback)| std::sync::Arc::clone(callback))
                .collect();
            (tagged, subscribers)
        };
        for callback in subscribers {
            let event = tagged.clone();
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| callback(event)));
        }
    }

    /// `pushEvent(event)`.
    pub(crate) fn push_event(&self, event: JsValue) {
        self.notify_subscribers(event);
    }

    /// `transitionTurnState(next, reason)`.
    pub(crate) fn transition_turn_state(&self, next: TurnState) {
        self.state.borrow_mut().turn_state = next;
    }

    /// `syncTurnState(reason)`.
    pub(crate) fn sync_turn_state(&self) {
        let mut state = self.state.borrow_mut();
        state.turn_state = if state.active_foreground_turn_id.is_some() {
            TurnState::Foreground
        } else if state.autonomous_turn.is_some() {
            TurnState::Autonomous
        } else {
            TurnState::Idle
        };
    }

    /// `createTurnId(owner)`.
    pub(crate) fn create_turn_id(&self, owner: &str) -> String {
        let mut state = self.state.borrow_mut();
        let ordinal = state.next_turn_ordinal;
        state.next_turn_ordinal += 1;
        format!("{owner}-turn-{ordinal}")
    }

    /// `setMode(modeId)`.
    ///
    /// # Errors
    ///
    /// An invalid or unavailable mode, or the query's failure.
    pub async fn set_mode(self: &Rc<Self>, mode_id: &str) -> Result<(), AgentError> {
        if !is_permission_mode(Some(mode_id)) {
            return Err(AgentError::new(format!(
                "Invalid mode '{mode_id}' for Claude provider. Valid modes: {}",
                valid_modes_list()
            )));
        }
        assert_mode_can_run(mode_id, &self.build_sdk_env())?;
        let previous = self.state.borrow().current_mode.clone();
        let query = self.ensure_query().await?;
        query.set_permission_mode(mode_id).await?;
        let mut state = self.state.borrow_mut();
        if mode_id == "plan" {
            if previous != "plan" {
                state.plan_resume_mode = Some(previous);
            }
        } else {
            state.plan_resume_mode = Some(mode_id.to_owned());
        }
        mode_id.clone_into(&mut state.current_mode);
        Ok(())
    }

    /// `setModel(modelId)`.
    ///
    /// # Errors
    ///
    /// The query's failure.
    pub async fn set_model(self: &Rc<Self>, model_id: Option<&str>) -> Result<(), AgentError> {
        let normalized = model_id
            .map(spocky_contracts::text::js_trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned);
        let query = self.ensure_query().await?;
        query.set_model(normalized.as_deref()).await?;
        self.state.borrow_mut().config.insert(
            "model",
            normalized
                .clone()
                .map_or(JsValue::Undefined, JsValue::String),
        );
        self.reconcile_thinking_option_for_model(normalized.as_deref());
        let model = self.config_str("model");
        let fast = self
            .state
            .borrow()
            .config
            .get("featureValues")
            .and_then(|values| values.get("fast_mode"))
            .is_some_and(|value| spocky_contracts::js::truthy(Some(value)));
        if !claude_model_supports_fast_mode(model.as_deref()) && fast {
            self.apply_fast_mode_feature(false, Some(query)).await?;
        }
        let mut state = self.state.borrow_mut();
        state.context_usage.set_initial_context_window_max_tokens(
            find_claude_model_context_window(model.as_deref()),
        );
        if normalized.is_some() {
            state.last_options_model.clone_from(&normalized);
        }
        state.last_runtime_model = None;
        state.cached_runtime_info = None;
        state.persistence = None;
        Ok(())
    }

    fn reconcile_thinking_option_for_model(&self, model_id: Option<&str>) {
        let current = self.config_str("thinkingOptionId");
        if current.as_deref() != Some(CLAUDE_DISABLED_THINKING_OPTION_ID) {
            return;
        }
        let (supported, fallback) = resolve_claude_disabled_thinking_for_model(model_id);
        if supported {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.config.insert(
                "thinkingOptionId",
                fallback.map_or(JsValue::Undefined, text),
            );
            state.query_restart_needed = true;
        }
        let mut event = JsObject::new();
        event.insert("type", text("thinking_option_changed"));
        event.insert("provider", text("claude"));
        event.insert("thinkingOptionId", fallback.map_or(JsValue::Null, text));
        self.push_event(JsValue::Object(event));
    }

    /// `setThinkingOption(thinkingOptionId)`: the notice when it applies on
    /// the next turn.
    ///
    /// # Errors
    ///
    /// An unknown or unsupported thinking option.
    pub fn set_thinking_option(&self, option: Option<&str>) -> Result<Option<JsValue>, AgentError> {
        let normalized = option.filter(|value| !spocky_contracts::text::js_trim(value).is_empty());
        match normalized {
            None | Some("default") => {
                self.state
                    .borrow_mut()
                    .config
                    .insert("thinkingOptionId", JsValue::Undefined);
            }
            Some(value)
                if value == CLAUDE_DISABLED_THINKING_OPTION_ID
                    || value == CLAUDE_ULTRACODE_THINKING_OPTION_ID
                    || is_thinking_effort(Some(value)) =>
            {
                assert_thinking_option_supported(self.config_str("model").as_deref(), Some(value))?;
                self.state
                    .borrow_mut()
                    .config
                    .insert("thinkingOptionId", text(value));
            }
            Some(value) => {
                return Err(AgentError::new(format!("Unknown thinking option: {value}")));
            }
        }
        let mut state = self.state.borrow_mut();
        state.query_restart_needed = true;
        if state.active_foreground_turn_id.is_some() || state.autonomous_turn.is_some() {
            let mut notice = JsObject::new();
            notice.insert("type", text("warning"));
            notice.insert("message", text("Thinking level applies next turn"));
            return Ok(Some(JsValue::Object(notice)));
        }
        Ok(None)
    }

    /// `setFeature(featureId, value)`.
    ///
    /// # Errors
    ///
    /// An unknown feature, fast mode on an unsupported model, or the query's
    /// failure.
    pub async fn set_feature(
        self: &Rc<Self>,
        feature_id: &str,
        value: &JsValue,
    ) -> Result<(), AgentError> {
        if feature_id != "fast_mode" {
            return Err(AgentError::new(format!(
                "Unknown Claude feature: {feature_id}"
            )));
        }
        let enabled = spocky_contracts::js::truthy(Some(value));
        let model = self.config_str("model");
        if enabled && !claude_model_supports_fast_mode(model.as_deref()) {
            return Err(AgentError::new(format!(
                "Claude fast mode is not available for model '{}'",
                model.as_deref().unwrap_or("default")
            )));
        }
        self.apply_fast_mode_feature(enabled, None).await
    }

    async fn apply_fast_mode_feature(
        &self,
        enabled: bool,
        query: Option<Rc<dyn ClaudeQuery>>,
    ) -> Result<(), AgentError> {
        let query = {
            let mut state = self.state.borrow_mut();
            let mut values = spocky_contracts::js::spread(state.config.get("featureValues"));
            values.insert("fast_mode", JsValue::Bool(enabled));
            state
                .config
                .insert("featureValues", JsValue::Object(values));
            query.or_else(|| state.query.clone())
        };
        if let Some(query) = query {
            let mut settings = JsObject::new();
            settings.insert("fastMode", JsValue::Bool(enabled));
            query.apply_flag_settings(JsValue::Object(settings)).await?;
        }
        self.state.borrow_mut().cached_runtime_info = None;
        Ok(())
    }

    /// `resolveFastModeSetting()`.
    pub(crate) fn resolve_fast_mode_setting(&self) -> Option<bool> {
        if !claude_model_supports_fast_mode(self.config_str("model").as_deref()) {
            return None;
        }
        Some(self.fast_mode_enabled())
    }

    /// `awaitWithTimeout(promise, label)`: three seconds, errors swallowed.
    pub(crate) async fn await_with_timeout<T>(future: Option<LocalBoxFuture<'static, T>>) {
        if let Some(future) = future {
            let _ = tokio::time::timeout(Duration::from_millis(3000), future).await;
            // `await` yields to queued jobs even for a settled promise: the
            // pump's reaction to a closed stream runs before the caller resumes.
            tokio::task::yield_now().await;
        }
    }

    /// `close()`.
    pub async fn close(self: &Rc<Self>) {
        self.state.borrow_mut().closed = true;
        self.reject_all_pending_permissions("Claude session closed");
        let cancel = self.state.borrow().cancel_current_turn;
        if let Some(token) = cancel {
            let _ = self.request_cancel(token);
        }
        let input = {
            let mut state = self.state.borrow_mut();
            state.subscribers.clear();
            state.active_foreground_turn_id = None;
            state.active_foreground_query = None;
            state.active_foreground_input = None;
            state.autonomous_turn = None;
            state.cancel_current_turn = None;
            state.turn_state = TurnState::Idle;
            state.sidechain_tracker.clear();
            state.input.clone()
        };
        self.task_protocol_source.borrow_mut().reset();
        if let Some(input) = &input {
            input.end();
        }
        // `this.query` is read again at each step: the pump clears it once the
        // closed stream ends.
        let query = self.state.borrow().query.clone();
        if let Some(query) = &query {
            query.close();
        }
        let query = self.state.borrow().query.clone();
        Self::await_with_timeout(query.map(|query| query.interrupt())).await;
        let query = self.state.borrow().query.clone();
        Self::await_with_timeout(query.map(|query| query.return_())).await;
        let child = {
            let mut state = self.state.borrow_mut();
            state.query = None;
            state.input = None;
            state.child_process.take()
        };
        if let Some(child) = child {
            let result = terminate_with_tree_kill(
                &child,
                Duration::from_millis(2000),
                Duration::from_millis(2000),
            )
            .await;
            let _ = result == TerminateResult::KillTimeout;
        }
        let session_id = self.state.borrow().claude_session_id.clone();
        if self.options.persist_session == Some(false)
            && let Some(session_id) = session_id
            && let Some(path) = self.resolve_history_path(&session_id)
        {
            let _ = std::fs::remove_file(path);
        }
    }

    /// `rejectAllPendingPermissions(error)`.
    pub(crate) fn reject_all_pending_permissions(&self, message: &str) {
        loop {
            let pending = {
                let mut state = self.state.borrow_mut();
                if state.pending_permissions.is_empty() {
                    break;
                }
                state.pending_permissions.remove(0)
            };
            let (id, pending) = pending;
            pending.resolve.settle(Err(AgentError::new(message)));
            let mut resolution = JsObject::new();
            resolution.insert("behavior", text("deny"));
            resolution.insert("message", text(message));
            let mut event = JsObject::new();
            event.insert("type", text("permission_resolved"));
            event.insert("provider", text("claude"));
            event.insert("requestId", JsValue::String(id));
            event.insert("resolution", JsValue::Object(resolution));
            self.push_event(JsValue::Object(event));
        }
    }
}
