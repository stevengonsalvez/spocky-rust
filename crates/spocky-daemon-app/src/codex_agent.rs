//! The Codex provider behind the agent manager's provider seam:
//! `CodexAppServerAgentClient` and `CodexAppServerAgentSession`
//! (`agent/providers/codex-app-server-agent.ts`) as
//! [`AgentClient`] and [`AgentSession`] over `spocky-provider-codex`.
//!
//! `spocky-provider-codex` calls block, so each future moves a clone of the
//! session or provider handle (both are `Arc`-backed) into `spawn_blocking`;
//! nothing borrowed is held across an await. Values cross the seam as
//! `JsValue` through `js_value` text, which keeps key order and absent keys.
//! Stream events reach subscribers synchronously and in order on the
//! provider's dispatcher thread, which is the seam's delivery contract.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;

use serde_json::{Map, Value};
use spocky_contracts::js::truthy;
use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_provider_codex::launch::{CODEX_NOT_FOUND_MESSAGE, resolve_launch_prefix};
use spocky_provider_codex::{
    CodexProvider, CodexSession, Prompt, ProviderRuntimeSettings, ResumeHandle, RunOptions,
    SessionConfig,
};
use spocky_session::agent_sdk::{
    AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError, AgentEventStream,
    AgentLaunchContext, AgentPromptInput, AgentProvider, AgentResult, AgentResumePurpose,
    AgentResumeSessionOptions, AgentRunOptions, AgentSession, AgentStreamEvent, BoxFuture,
    FetchCatalogOptions, ProviderRefreshContext, StreamCallback, Unsubscribe,
};

const CODEX: &str = "codex";

/// `CODEX_APP_SERVER_CAPABILITIES`.
const CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false}"#;

/// Members the baseline Codex session implements that
/// `spocky-provider-codex` does not expose yet; calling one fails loudly
/// rather than pretending the provider lacks it.
fn not_exposed(member: &str) -> AgentError {
    AgentError::new(format!(
        "spocky-provider-codex does not expose CodexSession.{member} yet"
    ))
}

fn capabilities() -> JsValue {
    js_value::parse(CAPABILITIES).expect("capability flags are JSON")
}

/// A provider value as a seam value, keys in the provider's order.
fn to_js(value: &Value) -> JsValue {
    js_value::parse(&value.to_string()).expect("serde_json writes JSON")
}

/// A seam value as a provider value. Text `serde_json` cannot hold (a lone
/// surrogate) is an error.
fn to_json(value: &JsValue) -> AgentResult<Value> {
    serde_json::from_str(&js_value::stringify(value))
        .map_err(|error| AgentError::new(error.to_string()))
}

fn to_object(value: &JsValue, what: &str) -> AgentResult<Map<String, Value>> {
    match to_json(value)? {
        Value::Object(object) => Ok(object),
        _ => Err(AgentError::new(format!("{what} is not an object"))),
    }
}

/// `launchContext?.env`; non-string entries never occur in a valid
/// `Record<string, string>` and are skipped.
fn launch_env(context: Option<&AgentLaunchContext>) -> Option<BTreeMap<String, String>> {
    let env = context?.env.as_ref()?;
    Some(
        env.iter()
            .filter_map(|(key, value)| Some((key.to_owned(), value.as_str()?.to_owned())))
            .collect(),
    )
}

fn prompt(input: AgentPromptInput) -> AgentResult<Prompt> {
    Ok(match input {
        AgentPromptInput::Text(text) => Prompt::Text(text),
        AgentPromptInput::Blocks(blocks) => {
            Prompt::Blocks(blocks.iter().map(to_json).collect::<AgentResult<_>>()?)
        }
    })
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> AgentResult<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AgentError::new(error.to_string()))?
        .map_err(AgentError::new)
}

/// The `handle` `resumeSession` passes through unchanged. A missing, `null`,
/// or empty `sessionId` becomes `""`, which the session constructor reads as
/// falsy (`if (this.resumeHandle?.sessionId)`) and so starts threadless.
fn resume_handle(handle: &JsValue) -> AgentResult<ResumeHandle> {
    let handle = to_object(handle, "handle")?;
    Ok(ResumeHandle {
        session_id: handle
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        metadata: handle.get("metadata").and_then(Value::as_object).cloned(),
    })
}

/// Keywords whose value maps names to schemas.
const SCHEMA_MAP_KEYWORDS: [&str; 5] = [
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// Keywords whose value is a schema or an array of schemas.
const SUBSCHEMA_KEYWORDS: [&str; 15] = [
    "items",
    "prefixItems",
    "additionalItems",
    "contains",
    "additionalProperties",
    "unevaluatedProperties",
    "unevaluatedItems",
    "propertyNames",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "if",
    "then",
    "else",
];

/// `isSchemaRecord`: a non-null, non-array object.
fn schema_record(value: &JsValue) -> Option<&JsObject> {
    value.as_object()
}

/// `isObjectSchemaNode`.
fn is_object_schema_node(schema: &JsObject) -> bool {
    schema
        .get("properties")
        .is_some_and(|p| p.as_object().is_some())
        || match schema.get("type") {
            Some(JsValue::String(kind)) => kind == "object",
            Some(JsValue::Array(kinds)) => kinds.iter().any(|k| k.as_str() == Some("object")),
            _ => false,
        }
}

fn normalize_subschema(schema: &JsValue, path: &str) -> Result<JsValue, String> {
    match schema {
        JsValue::Array(entries) => entries
            .iter()
            .enumerate()
            .map(|(index, entry)| normalize_node(entry, &format!("{path}[{index}]")))
            .collect::<Result<_, _>>()
            .map(JsValue::Array),
        other => normalize_node(other, path),
    }
}

fn normalize_node(schema: &JsValue, path: &str) -> Result<JsValue, String> {
    let Some(source) = schema_record(schema) else {
        return Ok(schema.clone());
    };
    let mut normalized = source.clone();
    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(schemas) = source.get(keyword).and_then(schema_record) {
            let mut mapped = JsObject::new();
            for (name, child) in schemas.iter() {
                mapped.insert(
                    name,
                    normalize_node(child, &format!("{path}.{keyword}.{name}"))?,
                );
            }
            normalized.insert(keyword, JsValue::Object(mapped));
        }
    }
    for keyword in SUBSCHEMA_KEYWORDS {
        if let Some(value) = source.get(keyword) {
            normalized.insert(
                keyword,
                normalize_subschema(value, &format!("{path}.{keyword}"))?,
            );
        }
    }
    if !is_object_schema_node(&normalized) {
        return Ok(JsValue::Object(normalized));
    }
    match normalized.get("additionalProperties") {
        None | Some(JsValue::Undefined) => {
            normalized.insert("additionalProperties", JsValue::Bool(false));
        }
        Some(JsValue::Bool(false)) => {}
        Some(_) => {
            return Err(format!(
                "Codex structured outputs require {path} to set additionalProperties to false for object schemas."
            ));
        }
    }
    let Some(properties) = normalized.get("properties").and_then(schema_record) else {
        return Ok(JsValue::Object(normalized));
    };
    let keys: Vec<String> = properties.iter().map(|(key, _)| key.to_owned()).collect();
    let mut required: Vec<String> = match normalized.get("required") {
        Some(JsValue::Array(entries)) => entries
            .iter()
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    required.extend(keys);
    let mut seen = std::collections::HashSet::new();
    required.retain(|key| seen.insert(key.clone()));
    normalized.insert(
        "required",
        JsValue::Array(required.into_iter().map(JsValue::String).collect()),
    );
    Ok(JsValue::Object(normalized))
}

/// `normalizeCodexOutputSchema` (`codex-app-server-agent.ts`): every object
/// schema node gets `additionalProperties: false` and requires all of its
/// properties, as Codex structured outputs demand.
///
/// # Errors
///
/// The baseline's messages for a non-object schema, a non-object root, or
/// an object node whose `additionalProperties` is not `false`.
pub fn normalize_codex_output_schema(schema: &JsValue) -> Result<JsValue, String> {
    if schema_record(schema).is_none() {
        return Err("Codex structured outputs require a JSON object schema.".to_owned());
    }
    let normalized = normalize_node(schema, "$")?;
    if !normalized.as_object().is_some_and(is_object_schema_node) {
        return Err("Codex structured outputs require a root object schema.".to_owned());
    }
    Ok(normalized)
}

/// `CodexAppServerAgentSession`.
pub struct CodexAgentSession {
    session: BlockingDrop,
}

impl CodexAgentSession {
    #[must_use]
    pub fn new(session: CodexSession) -> Self {
        Self {
            session: BlockingDrop(Some(session)),
        }
    }
}

/// Owns the adapter's session handle. Dropping the last handle disposes the
/// app-server, which blocks, so inside a tokio runtime the drop moves to the
/// blocking pool instead of stalling an executor thread.
struct BlockingDrop(Option<CodexSession>);

impl std::ops::Deref for BlockingDrop {
    type Target = CodexSession;

    fn deref(&self) -> &CodexSession {
        self.0.as_ref().expect("the session is present until drop")
    }
}

impl Drop for BlockingDrop {
    fn drop(&mut self) {
        let Some(session) = self.0.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn_blocking(move || drop(session)));
        }
    }
}

/// `streamHistory()` over the replayed history the provider already holds.
struct HistoryStream(std::vec::IntoIter<Value>);

impl AgentEventStream for HistoryStream {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        let event = self.0.next().map(|event| Ok(to_js(&event)));
        Box::pin(async move { event })
    }
}

impl AgentSession for CodexAgentSession {
    fn provider(&self) -> AgentProvider {
        CODEX.to_owned()
    }

    fn id(&self) -> Option<String> {
        self.session.id()
    }

    fn capabilities(&self) -> JsValue {
        capabilities()
    }

    fn features(&self) -> Option<JsValue> {
        Some(to_js(&self.session.features()))
    }

    fn run(
        &self,
        _prompt: AgentPromptInput,
        _options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Err(not_exposed("run")) })
    }

    fn start_turn(
        &self,
        input: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        let session = self.session.clone();
        Box::pin(async move {
            let prompt = prompt(input)?;
            // `buildTurnStartParams` reads `outputSchema` when truthy and
            // ignores `resumeFrom` and `maxThinkingTokens`.
            let options = options.unwrap_or_default();
            if let Some(schema) = options
                .output_schema
                .as_ref()
                .filter(|schema| truthy(Some(schema)))
            {
                // The baseline throws the normalizer's error before turn/start;
                // a valid schema needs a provider field that does not exist yet.
                normalize_codex_output_schema(schema).map_err(AgentError::new)?;
                return Err(not_exposed("startTurn outputSchema"));
            }
            let options = RunOptions {
                client_message_id: options.client_message_id,
            };
            blocking(move || session.start_turn(&prompt, &options)).await
        })
    }

    fn subscribe(&self, callback: StreamCallback) -> Unsubscribe {
        let id = self
            .session
            .subscribe(Arc::new(move |event: &Value| callback(to_js(event))));
        let session = self.session.clone();
        Box::new(move || session.unsubscribe(id))
    }

    fn stream_history(&self) -> Box<dyn AgentEventStream> {
        Box::new(HistoryStream(self.session.stream_history().into_iter()))
    }

    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        let session = self.session.clone();
        Box::pin(async move {
            blocking(move || session.runtime_info())
                .await
                .map(|v| to_js(&v))
        })
    }

    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        let modes = to_js(&self.session.available_modes());
        Box::pin(async move { Ok(modes) })
    }

    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>> {
        let mode = self.session.current_mode();
        Box::pin(async move { Ok(Some(mode)) })
    }

    fn set_mode(&self, _mode_id: &str) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        Box::pin(async { Err(not_exposed("setMode")) })
    }

    fn get_pending_permissions(&self) -> AgentResult<Vec<JsValue>> {
        Ok(self
            .session
            .pending_permissions()
            .iter()
            .map(to_js)
            .collect())
    }

    fn respond_to_permission(
        &self,
        request_id: &str,
        response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        let session = self.session.clone();
        let request_id = request_id.to_owned();
        Box::pin(async move {
            let response = to_json(&response)?;
            blocking(move || session.respond_to_permission(&request_id, &response)).await?;
            Ok(None)
        })
    }

    fn describe_persistence(&self) -> Option<JsValue> {
        self.session.describe_persistence().as_ref().map(to_js)
    }

    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        let session = self.session.clone();
        Box::pin(blocking(move || session.interrupt()))
    }

    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        let session = self.session.clone();
        Box::pin(blocking(move || session.close()))
    }
}

/// `CodexAppServerAgentClient`.
pub struct CodexAgentClient {
    provider: Arc<CodexProvider>,
    runtime_settings: Option<ProviderRuntimeSettings>,
    base_env: Vec<(OsString, OsString)>,
}

impl CodexAgentClient {
    /// `new CodexAppServerAgentClient(logger, runtimeSettings)` for the
    /// built-in id; `base_env` is `process.env`.
    #[must_use]
    pub fn new(
        runtime_settings: Option<ProviderRuntimeSettings>,
        base_env: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            provider: Arc::new(CodexProvider::new(
                runtime_settings.clone(),
                None,
                base_env.clone(),
            )),
            runtime_settings,
            base_env,
        }
    }
}

impl AgentClient for CodexAgentClient {
    fn provider(&self) -> AgentProvider {
        CODEX.to_owned()
    }

    fn capabilities(&self) -> JsValue {
        capabilities()
    }

    fn create_session(
        &self,
        config: JsValue,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let config = SessionConfig::from_json(&to_object(&config, "config")?);
            let env = launch_env(launch_context.as_ref());
            let ephemeral = options.and_then(|options| options.persist_session) == Some(false);
            let session = blocking(move || provider.create_session(config, env, ephemeral)).await?;
            Ok(Arc::new(CodexAgentSession::new(session)) as Arc<dyn AgentSession>)
        })
    }

    fn resume_session(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let handle = resume_handle(&handle)?;
            let overrides = match overrides {
                Some(overrides) => to_object(&overrides, "overrides")?,
                None => Map::new(),
            };
            let env = launch_env(launch_context.as_ref());
            let history_only =
                options.and_then(|options| options.purpose) == Some(AgentResumePurpose::History);
            let session =
                blocking(move || provider.resume_session(&handle, &overrides, env, history_only))
                    .await?;
            Ok(Arc::new(CodexAgentSession::new(session)) as Arc<dyn AgentSession>)
        })
    }

    /// `getCatalogCacheKey`: the client discovers through host
    /// configuration, independent of project cwd.
    fn get_catalog_cache_key(
        &self,
        _options: &FetchCatalogOptions,
    ) -> Option<BoxFuture<'static, AgentResult<Option<String>>>> {
        Some(Box::pin(async { Ok(Some("host".to_owned())) }))
    }

    fn fetch_catalog(
        &self,
        _options: FetchCatalogOptions,
        _context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let provider = Arc::clone(&self.provider);
        // ponytail: the refresh signal does not reach the app-server; the
        // provider takes a deadline instead. Map signal to deadline if the
        // snapshot manager needs abort.
        Box::pin(async move {
            blocking(move || provider.fetch_catalog(None))
                .await
                .map(|catalog| to_js(&catalog))
        })
    }

    /// `isAvailable()`: `resolveCodexLaunch` then
    /// `checkCodexLaunchAvailable`, a found binary or not; a failed lookup
    /// rejects.
    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        let settings = self.runtime_settings.clone();
        let base_env = self.base_env.clone();
        Box::pin(async move {
            let found = tokio::task::spawn_blocking(move || {
                resolve_launch_prefix(settings.as_ref(), &base_env)
            })
            .await
            .map_err(|error| AgentError::new(error.to_string()))?;
            match found {
                Ok(_) => Ok(true),
                Err(message) if message == CODEX_NOT_FOUND_MESSAGE => Ok(false),
                Err(message) => Err(AgentError::new(message)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use spocky_contracts::js_value::{self, JsObject, JsValue};
    use spocky_provider_codex::session::{CodexSession, SessionOptions};
    use spocky_provider_codex::{
        CodexGates, ProviderCommand, ProviderRuntimeSettings, SessionConfig,
    };
    use spocky_session::agent_sdk::{
        AgentClient, AgentLaunchContext, AgentPromptInput, AgentRunOptions, AgentSession,
    };

    use super::{CodexAgentClient, CodexAgentSession, launch_env, resume_handle};

    /// Session options whose spawn always fails, so no app-server starts.
    fn test_options(mode: &str) -> SessionOptions {
        SessionOptions {
            config: SessionConfig {
                cwd: "/tmp/p".to_owned(),
                mode_id: Some(mode.to_owned()),
                model: Some("gpt-6-astra".to_owned()),
                ..SessionConfig::default()
            },
            spawn: Box::new(|| Err("Test session cannot spawn Codex app-server".to_owned())),
            custom_codex_config: None,
            ephemeral: false,
            gates: CodexGates {
                goals_enabled: false,
                auto_review_enabled: false,
            },
        }
    }

    /// A session primed as the provider's notification fixtures prime it:
    /// connected on `test-thread`, never spawning an app-server.
    fn primed_session(mode: &str) -> CodexSession {
        let session = CodexSession::new(test_options(mode)).expect("session");
        session.prime_for_notification_test("test-thread", Some("test-turn"));
        session
    }

    fn texts(events: &Mutex<Vec<JsValue>>) -> Vec<String> {
        events
            .lock()
            .unwrap()
            .iter()
            .map(js_value::stringify)
            .collect()
    }

    fn missing_binary_client() -> CodexAgentClient {
        CodexAgentClient::new(
            Some(ProviderRuntimeSettings {
                command: Some(ProviderCommand::Replace {
                    argv: vec!["/nonexistent/spocky-codex".to_owned()],
                }),
                env: None,
            }),
            vec![(OsString::from("PATH"), OsString::from("/nonexistent"))],
        )
    }

    #[test]
    fn subscribe_delivers_events_in_order_until_unsubscribed() {
        let session = primed_session("full-access");
        let agent = CodexAgentSession::new(session.clone());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let unsubscribe = agent.subscribe(Arc::new(move |event| sink.lock().unwrap().push(event)));
        session.receive_notification(
            "turn/completed",
            Some(&json!({"turn": {"status": "completed", "error": null}})),
        );
        assert_eq!(
            texts(&events),
            [r#"{"type":"turn_completed","provider":"codex","turnId":"test-turn"}"#]
        );
        unsubscribe();
        session.prime_for_notification_test("test-thread", Some("second"));
        session.receive_notification(
            "turn/completed",
            Some(&json!({"turn": {"status": "completed", "error": null}})),
        );
        assert_eq!(texts(&events).len(), 1);
    }

    #[test]
    fn persistence_and_modes_cross_as_js_values_in_provider_order() {
        let agent = CodexAgentSession::new(primed_session("full-access"));
        let persistence = js_value::stringify(&agent.describe_persistence().expect("handle"));
        assert!(
            persistence.starts_with(
                r#"{"provider":"codex","sessionId":"test-thread","nativeHandle":"test-thread","metadata":{"provider":"codex","cwd":"/tmp/p","title":null,"threadId":"test-thread","modeId":"full-access","model":"gpt-6-astra""#
            ),
            "{persistence}"
        );
        assert_eq!(agent.id().as_deref(), Some("test-thread"));
        assert_eq!(agent.provider(), "codex");
        let flags = js_value::stringify(&agent.capabilities());
        assert!(
            flags.starts_with(r#"{"supportsStreaming":true,"#),
            "{flags}"
        );
    }

    #[tokio::test]
    async fn every_workspace_shares_the_host_catalog() {
        let client = missing_binary_client();
        let options = spocky_session::agent_sdk::FetchCatalogOptions::Workspace {
            cwd: "/tmp/a".to_owned(),
            force: false,
        };
        let key = client.get_catalog_cache_key(&options).expect("keyed");
        assert_eq!(key.await.unwrap().as_deref(), Some("host"));
    }

    #[tokio::test]
    async fn current_mode_and_close_run_through_the_seam() {
        let agent = CodexAgentSession::new(primed_session("full-access"));
        assert_eq!(
            agent.get_current_mode().await.unwrap().as_deref(),
            Some("full-access")
        );
        agent.close().await.expect("close");
        assert_eq!(agent.describe_persistence(), None);
        agent
            .interrupt()
            .await
            .expect("interrupt after close is a no-op");
    }

    #[tokio::test]
    async fn start_turn_reports_the_provider_error_as_agent_error() {
        let agent = CodexAgentSession::new(primed_session("full-access"));
        agent.close().await.expect("close");
        let error = agent
            .start_turn(AgentPromptInput::Text("hi".to_owned()), None)
            .await
            .expect_err("closed session rejects");
        assert_eq!(error.name, "Error");
        assert!(!error.message.is_empty());
    }

    #[tokio::test]
    async fn unexposed_members_fail_loudly() {
        let agent = CodexAgentSession::new(primed_session("full-access"));
        let run = agent
            .run(AgentPromptInput::Text("hi".to_owned()), None)
            .await
            .expect_err("run");
        assert_eq!(
            run.message,
            "spocky-provider-codex does not expose CodexSession.run yet"
        );
        assert!(agent.set_mode("auto").await.is_err());
        let schema = AgentRunOptions {
            output_schema: Some(js_value::parse(r#"{"type":"object"}"#).unwrap()),
            ..AgentRunOptions::default()
        };
        let invalid = AgentRunOptions {
            output_schema: Some(js_value::parse(r#"{"type":"string"}"#).unwrap()),
            ..AgentRunOptions::default()
        };
        let error = agent
            .start_turn(AgentPromptInput::Text("hi".to_owned()), Some(invalid))
            .await
            .expect_err("invalid outputSchema");
        assert_eq!(
            error.message,
            "Codex structured outputs require a root object schema."
        );
        let error = agent
            .start_turn(AgentPromptInput::Text("hi".to_owned()), Some(schema))
            .await
            .expect_err("outputSchema");
        assert_eq!(
            error.message,
            "spocky-provider-codex does not expose CodexSession.startTurn outputSchema yet"
        );
    }

    #[tokio::test]
    async fn missing_binary_is_unavailable_and_fails_create() {
        let client = missing_binary_client();
        assert!(!client.is_available(None, None).await.expect("lookup"));
        let config =
            js_value::parse(r#"{"provider":"codex","cwd":"/tmp/p","modeId":"full-access"}"#)
                .unwrap();
        let error = client
            .create_session(config, None, None)
            .await
            .err()
            .expect("no binary");
        assert_eq!(
            error.message,
            spocky_provider_codex::launch::CODEX_NOT_FOUND_MESSAGE
        );
        let not_object = JsValue::String("x".to_owned());
        let error = client
            .create_session(not_object, None, None)
            .await
            .err()
            .expect("bad config");
        assert_eq!(error.message, "config is not an object");
    }

    #[test]
    fn resume_handle_without_a_session_id_starts_threadless() {
        // Baseline constructor: `if (this.resumeHandle?.sessionId)` takes the
        // thread only for a truthy id; missing, null, and "" start threadless.
        for (handle, thread) in [
            (r#"{"provider":"codex","metadata":{"cwd":"/tmp/p"}}"#, None),
            (r#"{"provider":"codex","sessionId":null}"#, None),
            (r#"{"provider":"codex","sessionId":""}"#, None),
            (
                r#"{"provider":"codex","sessionId":"thread-1"}"#,
                Some("thread-1"),
            ),
        ] {
            let handle = resume_handle(&js_value::parse(handle).unwrap()).expect(handle);
            let session = CodexSession::resumed(test_options("full-access"), &handle, false)
                .expect("resumed session");
            assert_eq!(session.id().as_deref(), thread, "{handle:?}");
        }
    }

    #[tokio::test]
    async fn dropping_the_last_handle_disposes_off_the_executor_thread() {
        // A subscriber's captures drop with the provider session, so this
        // guard reports the thread that ran the final drop.
        struct Probe(Arc<Mutex<Option<std::thread::ThreadId>>>);
        impl Drop for Probe {
            fn drop(&mut self) {
                *self.0.lock().unwrap() = Some(std::thread::current().id());
            }
        }
        let dropped_on = Arc::new(Mutex::new(None));
        let session = primed_session("full-access");
        let probe = Probe(Arc::clone(&dropped_on));
        session.subscribe(Arc::new(move |_| {
            let _ = &probe;
        }));
        drop(CodexAgentSession::new(session));
        let executor = std::thread::current().id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let thread = loop {
            if let Some(thread) = *dropped_on.lock().unwrap() {
                break thread;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "session never dropped"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_ne!(thread, executor);
    }

    #[test]
    fn launch_env_keeps_string_entries() {
        let mut env = JsObject::new();
        env.insert("A", JsValue::String("1".to_owned()));
        env.insert("B", JsValue::Bool(true));
        let context = AgentLaunchContext {
            env: Some(env),
            ..AgentLaunchContext::default()
        };
        let resolved = launch_env(Some(&context)).expect("env");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved["A"], "1");
        assert_eq!(launch_env(None), None);
    }
}
