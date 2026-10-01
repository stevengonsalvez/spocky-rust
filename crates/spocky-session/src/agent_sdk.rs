//! The provider seam from pinned Paseo `agent/agent-sdk-types.ts`: the
//! `AgentClient` and `AgentSession` interfaces a provider implements and the
//! agent manager drives.
//!
//! Members keep the baseline's names in snake case and its order. Values the
//! baseline passes as plain objects (configs, stream events, timeline items,
//! capability flags, persistence handles, runtime info, modes, features,
//! permission requests and responses, catalogs) are [`JsValue`]s with the
//! baseline's shapes, so key order and `undefined` slots survive. Optional
//! interface members (`member?()`) are methods that return `None` when the
//! provider does not implement them; the default implementations do that.
//! A rejected promise or a thrown `Error` is an [`AgentError`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use spocky_store::js_value::{JsObject, JsValue};
use tokio::sync::Notify;

/// A boxed `Send` future, the shape of every asynchronous member.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `AgentProvider`: a provider id.
pub type AgentProvider = String;

/// An `AgentStreamEvent` object: `thread_started`, `turn_started`,
/// `turn_completed`, `usage_updated`, `mode_changed`, `model_changed`,
/// `thinking_option_changed`, `turn_failed`, `turn_canceled`, `timeline`,
/// `permission_requested`, `permission_resolved`, `attention_required`, or
/// `provider_subagent`, with the baseline's fields.
pub type AgentStreamEvent = JsValue;

/// A JavaScript `Error` a provider throws or rejects with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    /// `error.name`: `Error` unless the baseline uses a subclass.
    pub name: String,
    /// `error.message`.
    pub message: String,
}

impl AgentError {
    /// `new Error(message)`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            name: "Error".to_owned(),
            message: message.into(),
        }
    }

    /// `new StaleProviderSessionError(sessionId)` from
    /// `stale-provider-session-error.ts`.
    #[must_use]
    pub fn stale_provider_session(session_id: &str) -> Self {
        Self {
            name: "StaleProviderSessionError".to_owned(),
            message: format!("Provider session {session_id} is stale after its plugin reloaded"),
        }
    }

    /// `isStaleProviderSessionError`.
    #[must_use]
    pub fn is_stale_provider_session(&self) -> bool {
        self.name == "StaleProviderSessionError"
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {}

/// The result of a fallible asynchronous member.
pub type AgentResult<T> = Result<T, AgentError>;

/// `AbortSignal`: set once, observed by providers.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    aborted: Arc<AtomicBool>,
    reason: Arc<std::sync::OnceLock<JsValue>>,
    notify: Arc<Notify>,
}

impl AbortSignal {
    /// `controller.abort(reason)`; later calls keep the first reason.
    pub fn abort(&self, reason: JsValue) {
        let _ = self.reason.set(reason);
        self.aborted.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// `signal.aborted`.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    /// `signal.reason`.
    #[must_use]
    pub fn reason(&self) -> Option<&JsValue> {
        self.reason.get()
    }

    /// Resolves when the signal aborts.
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.aborted() {
                return;
            }
            notified.await;
        }
    }
}

/// `AgentPromptInput`: a string or `AgentPromptContentBlock[]` (text, image,
/// and attachment blocks as objects).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentPromptInput {
    Text(String),
    Blocks(Vec<JsValue>),
}

/// `AgentRunOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentRunOptions {
    pub output_schema: Option<JsValue>,
    /// `AgentPersistenceHandle`.
    pub resume_from: Option<JsValue>,
    pub max_thinking_tokens: Option<f64>,
    pub client_message_id: Option<String>,
}

/// `SteerActiveTurnOptions`: `AgentSteerOptions` plus the expected turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SteerActiveTurnOptions {
    pub run: AgentRunOptions,
    pub clear_pending_permissions: Option<bool>,
    pub expected_turn_id: String,
}

/// `SteerResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerResult {
    Accepted,
    Unavailable,
}

/// `ImportedTimelineEntry`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedTimelineEntry {
    /// `AgentTimelineItem`.
    pub item: JsValue,
    pub timestamp: Option<String>,
}

/// `AgentCreateSessionOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentCreateSessionOptions {
    pub persist_session: Option<bool>,
}

/// `AgentResumePurpose`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentResumePurpose {
    Interactive,
    History,
}

/// `AgentResumeSessionOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentResumeSessionOptions {
    pub purpose: Option<AgentResumePurpose>,
}

/// `PaseoToolExecutionContext`.
#[derive(Clone, Default)]
pub struct PaseoToolExecutionContext {
    pub signal: Option<AbortSignal>,
    /// `sendUpdate(update)` with a `PaseoToolResult`.
    pub send_update: Option<Arc<dyn Fn(JsValue) + Send + Sync>>,
}

/// `PaseoToolDefinition` without its handler, which
/// [`PaseoToolCatalog::execute_tool`] runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PaseoToolDefinition {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    /// The input schema as JSON Schema.
    pub input_schema: Option<JsValue>,
    /// The output schema as JSON Schema.
    pub output_schema: Option<JsValue>,
}

/// `PaseoToolCatalog`.
pub trait PaseoToolCatalog: Send + Sync {
    /// `tools`, in map order.
    fn tools(&self) -> Vec<PaseoToolDefinition>;
    /// `getTool(name)`.
    fn get_tool(&self, name: &str) -> Option<PaseoToolDefinition>;
    /// `executeTool(name, input, context)`, resolving a `PaseoToolResult`.
    fn execute_tool(
        &self,
        name: &str,
        input: JsValue,
        context: Option<PaseoToolExecutionContext>,
    ) -> BoxFuture<'_, AgentResult<JsValue>>;
}

/// `AgentLaunchContext`.
#[derive(Clone, Default)]
pub struct AgentLaunchContext {
    pub agent_id: Option<String>,
    /// `env: Record<string, string>` in object key order.
    pub env: Option<JsObject>,
    /// Runtime-only internal Paseo tools; never persisted.
    pub paseo_tools: Option<Arc<dyn PaseoToolCatalog>>,
}

/// `FetchCatalogOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchCatalogOptions {
    Global { force: bool },
    Workspace { cwd: String, force: bool },
}

/// `ProviderRefreshContext`.
pub trait ProviderRefreshContext: Send + Sync {
    /// `signal`.
    fn signal(&self) -> &AbortSignal;
    /// `runActivity(name, operation)`: tracks an upstream operation so a
    /// timeout names the work still pending.
    fn run_activity<'a>(
        &'a self,
        name: &'a str,
        operation: BoxFuture<'a, AgentResult<JsValue>>,
    ) -> BoxFuture<'a, AgentResult<JsValue>>;
}

/// `ResolveAgentDefaultModeInput`.
#[derive(Clone)]
pub struct ResolveAgentDefaultModeInput {
    /// `AgentSessionConfig`.
    pub config: JsValue,
    pub env: Option<JsObject>,
    pub signal: Option<AbortSignal>,
}

/// `ResolveAgentCreateConfigInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveAgentCreateConfigInput {
    pub provider: AgentProvider,
    pub requested_mode: Option<String>,
    pub feature_values: Option<JsValue>,
    /// `AgentCreateConfigParent` (`{ provider, modeId, isUnattended }`).
    pub parent: Option<JsValue>,
    pub unattended: bool,
    /// `AgentMode[]`.
    pub available_modes: Option<JsValue>,
}

/// `ResolveAgentCreateConfigResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveAgentCreateConfigResult {
    pub mode_id: Option<String>,
    pub feature_values: Option<JsValue>,
}

/// `AgentCreateConfigUnattendedInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCreateConfigUnattendedInput {
    pub mode_id: Option<String>,
    /// `AgentSessionConfig`.
    pub config: JsValue,
    /// `AgentFeature[]`.
    pub features: Option<JsValue>,
    /// `AgentMode[]`.
    pub available_modes: JsValue,
}

/// `ListImportableSessionsOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListImportableSessionsOptions {
    pub limit: Option<f64>,
    pub query: Option<String>,
    pub scan_limit: Option<f64>,
    pub cwd: Option<String>,
}

/// `ImportableProviderSession`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportableProviderSession {
    pub provider_handle_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub first_prompt_preview: Option<String>,
    pub last_prompt_preview: Option<String>,
    /// `lastActivityAt` as epoch milliseconds.
    pub last_activity_at_millis: f64,
}

/// `ImportProviderSessionInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportProviderSessionInput {
    pub provider_handle_id: String,
    pub cwd: String,
}

/// `ImportProviderSessionContext`.
#[derive(Clone)]
pub struct ImportProviderSessionContext {
    pub config: JsValue,
    pub stored_config: JsValue,
    pub launch_context: Option<AgentLaunchContext>,
}

/// `ImportedProviderSession`.
pub struct ImportedProviderSession {
    pub session: Arc<dyn AgentSession>,
    /// `AgentSessionConfig`.
    pub config: JsValue,
    /// `AgentPersistenceHandle`.
    pub persistence: JsValue,
    pub timeline: Vec<ImportedTimelineEntry>,
    /// `provider_subagent` stream events.
    pub provider_subagent_events: Option<Vec<AgentStreamEvent>>,
}

/// Stops a subscription; the function `subscribe` returns.
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

/// A `subscribe` callback. Providers call it synchronously as events occur,
/// in order.
pub type StreamCallback = Arc<dyn Fn(AgentStreamEvent) + Send + Sync>;

/// `AsyncGenerator<AgentStreamEvent>` of `streamHistory`.
pub trait AgentEventStream: Send {
    /// `next()`: `None` when the generator is done.
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>>;
}

/// The handler `tryHandleOutOfBand` returns.
pub trait OutOfBandHandler: Send {
    /// `run({ emit })`.
    fn run(self: Box<Self>, emit: StreamCallback) -> BoxFuture<'static, AgentResult<()>>;
}

/// `AgentSession`.
pub trait AgentSession: Send + Sync {
    /// `provider`.
    fn provider(&self) -> AgentProvider;
    /// `id`.
    fn id(&self) -> Option<String>;
    /// `capabilities` (`AgentCapabilityFlags`).
    fn capabilities(&self) -> JsValue;
    /// `features?` (`AgentFeature[]`).
    fn features(&self) -> Option<JsValue> {
        None
    }
    /// `initialTimeline?`: provider-owned rows to commit on registration.
    fn initial_timeline(&self) -> Option<Vec<ImportedTimelineEntry>> {
        None
    }
    /// `getUsageReference?()`, resolving a `UsageReference` or `null`.
    fn get_usage_reference(&self) -> Option<BoxFuture<'_, AgentResult<Option<JsValue>>>> {
        None
    }
    /// `run(prompt, options)`, resolving an `AgentRunResult`.
    fn run(
        &self,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>>;
    /// `startTurn(prompt, options)`, resolving `{ turnId }`.
    fn start_turn(
        &self,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>>;
    /// `steerActiveTurn?(prompt, options)`.
    fn steer_active_turn(
        &self,
        _prompt: AgentPromptInput,
        _options: SteerActiveTurnOptions,
    ) -> Option<BoxFuture<'_, AgentResult<SteerResult>>> {
        None
    }
    /// `subscribe(callback)`.
    fn subscribe(&self, callback: StreamCallback) -> Unsubscribe;
    /// `streamHistory()`.
    fn stream_history(&self) -> Box<dyn AgentEventStream + '_>;
    /// `getRuntimeInfo()`, resolving `AgentRuntimeInfo`.
    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>>;
    /// `getAvailableModes()`, resolving `AgentMode[]`.
    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>>;
    /// `getCurrentMode()`.
    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>>;
    /// `setMode(modeId)`, resolving nothing or an `AgentProviderNotice`.
    fn set_mode(&self, mode_id: &str) -> BoxFuture<'_, AgentResult<Option<JsValue>>>;
    /// `getPendingPermissions()` (`AgentPermissionRequest[]`); it may throw.
    ///
    /// # Errors
    ///
    /// The provider's error.
    fn get_pending_permissions(&self) -> AgentResult<Vec<JsValue>>;
    /// `respondToPermission(requestId, response)`, resolving nothing or an
    /// `AgentPermissionResult`.
    fn respond_to_permission(
        &self,
        request_id: &str,
        response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>>;
    /// `describePersistence()` (`AgentPersistenceHandle` or `null`).
    fn describe_persistence(&self) -> Option<JsValue>;
    /// `interrupt()`.
    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>>;
    /// `close()`.
    fn close(&self) -> BoxFuture<'_, AgentResult<()>>;
    /// `listCommands?()`, resolving `AgentSlashCommand[]`.
    fn list_commands(&self) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        None
    }
    /// `setModel?(modelId)`.
    fn set_model(&self, _model_id: Option<&str>) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `setThinkingOption?(thinkingOptionId)`, resolving nothing or an
    /// `AgentProviderNotice`.
    fn set_thinking_option(
        &self,
        _thinking_option_id: Option<&str>,
    ) -> Option<BoxFuture<'_, AgentResult<Option<JsValue>>>> {
        None
    }
    /// `setFeature?(featureId, value)`.
    fn set_feature(
        &self,
        _feature_id: &str,
        _value: JsValue,
    ) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `revertConversation?({ messageId })`.
    fn revert_conversation(&self, _message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `revertFiles?({ messageId })`.
    fn revert_files(&self, _message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `revertBoth?({ messageId })`.
    fn revert_both(&self, _message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `tryHandleOutOfBand?(prompt)`: the outer `Option` is the member's
    /// presence, the inner one its `null` result.
    fn try_handle_out_of_band(
        &self,
        _prompt: &AgentPromptInput,
    ) -> Option<Option<Box<dyn OutOfBandHandler>>> {
        None
    }
}

/// `AgentClient`.
pub trait AgentClient: Send + Sync {
    /// `provider`.
    fn provider(&self) -> AgentProvider;
    /// `capabilities` (`AgentCapabilityFlags`).
    fn capabilities(&self) -> JsValue;
    /// `createSession(config, launchContext, options)`.
    fn create_session(
        &self,
        config: JsValue,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>>;
    /// `resumeSession(handle, overrides, launchContext, options)`.
    fn resume_session(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>>;
    /// `getCatalogCacheKey?(options)`.
    fn get_catalog_cache_key(
        &self,
        _options: &FetchCatalogOptions,
    ) -> Option<BoxFuture<'_, AgentResult<Option<String>>>> {
        None
    }
    /// `fetchCatalog(options, context)`, resolving a `ProviderCatalog`
    /// (`{ models, modes, defaultModeId? }`).
    fn fetch_catalog<'a>(
        &'a self,
        options: FetchCatalogOptions,
        context: Option<&'a dyn ProviderRefreshContext>,
    ) -> BoxFuture<'a, AgentResult<JsValue>>;
    /// `resolveConfiguredModel?(model)`.
    fn resolve_configured_model(&self, _model: &JsValue) -> Option<JsValue> {
        None
    }
    /// `resolveDefaultModeId?(input)`.
    fn resolve_default_mode_id(
        &self,
        _input: ResolveAgentDefaultModeInput,
    ) -> Option<BoxFuture<'_, AgentResult<Option<String>>>> {
        None
    }
    /// `resolveCreateConfig?(input)`.
    fn resolve_create_config(
        &self,
        _input: &ResolveAgentCreateConfigInput,
    ) -> Option<ResolveAgentCreateConfigResult> {
        None
    }
    /// `isCreateConfigUnattended?(input)`.
    fn is_create_config_unattended(
        &self,
        _input: &AgentCreateConfigUnattendedInput,
    ) -> Option<bool> {
        None
    }
    /// `listCommands?(config)`, resolving `AgentSlashCommand[]`.
    fn list_commands(&self, _config: JsValue) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        None
    }
    /// `listFeatures?(config)`, resolving `AgentFeature[]`.
    fn list_features(&self, _config: JsValue) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        None
    }
    /// `listImportableSessions?(options)`.
    fn list_importable_sessions(
        &self,
        _options: Option<ListImportableSessionsOptions>,
    ) -> Option<BoxFuture<'_, AgentResult<Vec<ImportableProviderSession>>>> {
        None
    }
    /// `importSession?(input, context)`.
    fn import_session(
        &self,
        _input: ImportProviderSessionInput,
        _context: ImportProviderSessionContext,
    ) -> Option<BoxFuture<'_, AgentResult<ImportedProviderSession>>> {
        None
    }
    /// `isAvailable(signal, options)`.
    fn is_available(
        &self,
        signal: Option<AbortSignal>,
        options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>>;
    /// `getDiagnostic?()`, resolving `{ diagnostic }`.
    fn get_diagnostic(&self) -> Option<BoxFuture<'_, AgentResult<String>>> {
        None
    }
    /// `archiveNativeSession?(handle)`.
    fn archive_native_session(&self, _handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `unarchiveNativeSession?(handle)`.
    fn unarchive_native_session(&self, _handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
    /// `shutdown?()`.
    fn shutdown(&self) -> Option<BoxFuture<'_, AgentResult<()>>> {
        None
    }
}

/// `getAgentStreamEventTurnId`: the event's `turnId` when it has one.
#[must_use]
pub fn stream_event_turn_id(event: &AgentStreamEvent) -> Option<&str> {
    event.get("turnId").and_then(JsValue::as_str)
}

#[cfg(test)]
mod tests {
    use super::{AbortSignal, AgentError, stream_event_turn_id};
    use spocky_store::js_value::{JsValue, parse};

    #[tokio::test]
    async fn errors_signals_and_turn_ids_follow_the_baseline() {
        let stale = AgentError::stale_provider_session("s1");
        assert!(stale.is_stale_provider_session());
        assert_eq!(
            stale.to_string(),
            "Provider session s1 is stale after its plugin reloaded"
        );
        assert!(!AgentError::new("boom").is_stale_provider_session());
        let signal = AbortSignal::default();
        let waiter = signal.clone();
        let wait = tokio::spawn(async move { waiter.wait().await });
        signal.abort(JsValue::String("first".to_owned()));
        signal.abort(JsValue::String("second".to_owned()));
        wait.await.expect("wait resolves");
        assert!(signal.aborted());
        assert_eq!(signal.reason().and_then(JsValue::as_str), Some("first"));
        let event =
            parse(r#"{"type":"turn_started","provider":"codex","turnId":"t"}"#).expect("event");
        assert_eq!(stream_event_turn_id(&event), Some("t"));
        let event = parse(r#"{"type":"thread_started","provider":"codex"}"#).expect("event");
        assert_eq!(stream_event_turn_id(&event), None);
    }
}
