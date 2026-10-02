//! `ClaudeAgentClient` from `providers/claude/agent.ts`: sessions, the
//! model and mode catalog, features, and availability.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::agent_sdk::{
    AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError, AgentLaunchContext,
    AgentProvider, AgentResult, AgentResumeSessionOptions, AgentSession, BoxFuture,
    FetchCatalogOptions, ProviderRefreshContext, ResolveAgentDefaultModeInput, run_activity,
};

use crate::actor::{ClaudeActor, ClaudeSessionHandle};
use crate::launch::{
    ClaudeRuntimeSettings, is_available, process_env, provider_env, resolve_claude_binary,
    resolve_claude_code_version,
};
use crate::local::LocalBoxFuture;
use crate::models::{
    build_claude_features, get_claude_models_with_settings, resolve_configured_claude_model,
};
use crate::project_dir::claude_config_dir;
use crate::provider_options::parse_claude_provider_options;
use crate::sdk_query::QueryFactory;
use crate::session::{
    ResolveBinary, RewindSdk, SessionOptions, claude_capabilities, claude_mode_catalog,
};
use crate::transcript::is_mcp_servers_record;

/// A `Send` source of a value that is not `Send` itself, built on the
/// session's thread.
pub type OnSessionThread<T> = Arc<dyn Fn() -> T + Send + Sync>;

/// `ClaudeAgentClientOptions`.
#[derive(Clone, Default)]
pub struct ClaudeClientOptions {
    pub defaults_agents: Option<JsValue>,
    pub runtime_settings: Option<ClaudeRuntimeSettings>,
    pub query_factory: Option<OnSessionThread<QueryFactory>>,
    pub resolve_binary: Option<OnSessionThread<ResolveBinary>>,
    #[allow(clippy::type_complexity)]
    pub resolve_version: Option<
        Arc<dyn Fn(Option<AbortSignal>) -> BoxFuture<'static, AgentResult<String>> + Send + Sync>,
    >,
    pub rewind_sdk: Option<OnSessionThread<Rc<dyn RewindSdk>>>,
    /// `process.env`; the daemon's own when absent.
    pub process_env: Option<Arc<dyn Fn() -> JsObject + Send + Sync>>,
}

/// `ClaudeAgentClient`.
#[derive(Clone, Default)]
pub struct ClaudeClient {
    options: ClaudeClientOptions,
}

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `coerceSessionMetadata(metadata)`.
fn coerce_session_metadata(metadata: Option<&JsValue>) -> JsObject {
    let mut result = JsObject::new();
    let Some(metadata) =
        metadata.filter(|metadata| matches!(metadata, JsValue::Object(_) | JsValue::Array(_)))
    else {
        return result;
    };
    if matches!(str_of(metadata, "provider"), Some("claude" | "codex")) {
        result.insert(
            "provider",
            metadata
                .get("provider")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
    }
    for key in ["cwd", "modeId", "model"] {
        if let Some(value) = str_of(metadata, key) {
            result.insert(key, JsValue::String(value.to_owned()));
        }
    }
    if let Some(title) = metadata
        .get("title")
        .filter(|title| title.is_string() || title.is_null())
    {
        result.insert("title", title.clone());
    }
    if let Some(options) = metadata.get("providerOptions")
        && let Ok(parsed) = parse_claude_provider_options(options)
    {
        result.insert("providerOptions", parsed);
    }
    if let Some(prompt) = str_of(metadata, "systemPrompt") {
        result.insert("systemPrompt", JsValue::String(prompt.to_owned()));
    }
    if is_mcp_servers_record(metadata.get("mcpServers")) {
        result.insert(
            "mcpServers",
            metadata
                .get("mcpServers")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
    }
    result
}

impl ClaudeClient {
    /// `new ClaudeAgentClient(options)`.
    #[must_use]
    pub const fn new(options: ClaudeClientOptions) -> Self {
        Self { options }
    }

    fn process_env(&self) -> JsObject {
        self.options
            .process_env
            .as_ref()
            .map_or_else(process_env, |source| source())
    }

    /// `buildProviderEnv(launchEnv)`.
    fn build_provider_env(&self, launch_env: Option<&JsObject>) -> JsObject {
        provider_env(
            &self.process_env(),
            self.options.runtime_settings.as_ref(),
            launch_env,
        )
    }

    /// `assertConfig(config)`.
    fn assert_config(config: &JsValue) -> Result<JsObject, AgentError> {
        let provider = config.get("provider");
        if provider.and_then(JsValue::as_str) != Some("claude") {
            return Err(AgentError::new(format!(
                "ClaudeAgentClient received config for provider '{}'",
                spocky_contracts::js::js_string(provider)
            )));
        }
        let model = config
            .get("model")
            .and_then(JsValue::as_str)
            .map(|model| spocky_contracts::text::js_trim(model).to_owned());
        let provider_options = config
            .get("providerOptions")
            .filter(|options| !matches!(options, JsValue::Undefined | JsValue::Null))
            .cloned()
            .unwrap_or_else(|| JsValue::Object(JsObject::new()));
        let parsed =
            parse_claude_provider_options(&provider_options).map_err(|message| AgentError {
                name: "ZodError".to_owned(),
                message,
            })?;
        let mut result = spocky_contracts::js::spread(Some(config));
        result.insert("provider", JsValue::String("claude".to_owned()));
        result.insert(
            "model",
            model
                .filter(|model| !model.is_empty())
                .map_or(JsValue::Undefined, JsValue::String),
        );
        result.insert("providerOptions", parsed);
        Ok(result)
    }

    fn spawn_session(
        &self,
        config: JsObject,
        handle: Option<JsValue>,
        launch_context: Option<&AgentLaunchContext>,
        persist_session: Option<bool>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        let options = self.options.clone();
        let agent_id = launch_context.and_then(|context| context.agent_id.clone());
        let launch_env = launch_context.and_then(|context| context.env.clone());
        let factory = Box::new(move || {
            let runtime_settings = options.runtime_settings.clone();
            let process_env_source = options.process_env.clone();
            let env_for_binary = process_env_source.clone();
            let settings_for_binary = runtime_settings.clone();
            let default_resolve: ResolveBinary = Rc::new(move || {
                let settings = settings_for_binary.clone();
                let env = env_for_binary
                    .as_ref()
                    .map_or_else(process_env, |source| source());
                let future: LocalBoxFuture<'static, Result<String, AgentError>> =
                    Box::pin(async move { resolve_claude_binary(settings.as_ref(), &env) });
                future
            });
            SessionOptions {
                defaults_agents: options.defaults_agents.clone(),
                runtime_settings,
                handle,
                agent_id,
                launch_env,
                persist_session,
                query_factory: options.query_factory.as_ref().map(|factory| factory()),
                resolve_binary: options
                    .resolve_binary
                    .as_ref()
                    .map_or(default_resolve, |resolve| resolve()),
                rewind_sdk: options.rewind_sdk.as_ref().map(|sdk| sdk()),
                process_env: Rc::new(move || {
                    process_env_source
                        .as_ref()
                        .map_or_else(process_env, |source| source())
                }),
            }
        });
        let actor = ClaudeActor::spawn(config, factory)?;
        Ok(Arc::new(ClaudeSessionHandle::new(actor)))
    }
}

impl AgentClient for ClaudeClient {
    fn provider(&self) -> AgentProvider {
        "claude".to_owned()
    }

    fn capabilities(&self) -> JsValue {
        claude_capabilities()
    }

    fn create_session(
        &self,
        config: JsValue,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            let claude_config = Self::assert_config(&config)?;
            self.spawn_session(
                claude_config,
                None,
                launch_context.as_ref(),
                options.and_then(|options| options.persist_session),
            )
        })
    }

    fn resume_session(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        launch_context: Option<AgentLaunchContext>,
        _options: Option<AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            let mut merged = coerce_session_metadata(handle.get("metadata"));
            spocky_contracts::js::spread_into(&mut merged, overrides.as_ref());
            let cwd = merged.get("cwd").cloned();
            if !spocky_contracts::js::truthy(cwd.as_ref()) {
                return Err(AgentError::new(
                    "Claude resume requires the original working directory in metadata",
                ));
            }
            merged.insert("provider", JsValue::String("claude".to_owned()));
            merged.insert("cwd", cwd.unwrap_or(JsValue::Undefined));
            let claude_config = Self::assert_config(&JsValue::Object(merged))?;
            self.spawn_session(claude_config, Some(handle), launch_context.as_ref(), None)
        })
    }

    fn get_catalog_cache_key(
        &self,
        _options: &FetchCatalogOptions,
    ) -> Option<BoxFuture<'static, AgentResult<Option<String>>>> {
        // Discovery goes through host configuration, independent of any cwd.
        Some(Box::pin(async { Ok(Some("host".to_owned())) }))
    }

    fn fetch_catalog(
        &self,
        _options: FetchCatalogOptions,
        context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async move {
            let signal = context.as_ref().map(|context| context.signal().clone());
            let version_future: BoxFuture<'_, AgentResult<String>> = if let Some(resolve) =
                &self.options.resolve_version
            {
                resolve(signal)
            } else {
                let settings = self.options.runtime_settings.clone();
                let env = self.process_env();
                Box::pin(async move { resolve_claude_code_version(settings.as_ref(), &env).await })
            };
            let resolved = match &context {
                Some(context) => run_activity(context.as_ref(), "version", version_future).await,
                None => version_future.await,
            };
            let claude_code_version = resolved.ok();
            let env = self.build_provider_env(None);
            let config_dir = claude_config_dir(&env);
            let models_future = async {
                Ok(get_claude_models_with_settings(
                    Path::new(&config_dir),
                    claude_code_version.as_deref(),
                ))
            };
            let models = match &context {
                Some(context) => run_activity(context.as_ref(), "settings", models_future).await?,
                None => models_future.await?,
            };
            let (mode_list, default_mode) = claude_mode_catalog(&env);
            let mut catalog = JsObject::new();
            catalog.insert("models", JsValue::Array(models));
            catalog.insert("modes", JsValue::Array(mode_list));
            catalog.insert("defaultModeId", JsValue::String(default_mode.to_owned()));
            Ok(JsValue::Object(catalog))
        })
    }

    fn resolve_configured_model(&self, model: &JsValue) -> Option<JsValue> {
        Some(resolve_configured_claude_model(model))
    }

    fn resolve_default_mode_id(
        &self,
        input: ResolveAgentDefaultModeInput,
    ) -> Option<BoxFuture<'_, AgentResult<Option<String>>>> {
        Some(Box::pin(async move {
            let env = self.build_provider_env(input.env.as_ref());
            Ok(Some(claude_mode_catalog(&env).1.to_owned()))
        }))
    }

    fn list_features(&self, config: JsValue) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        Some(Box::pin(async move {
            let claude_config = Self::assert_config(&config)?;
            let fast = claude_config
                .get("featureValues")
                .and_then(|values| values.get("fast_mode"))
                == Some(&JsValue::Bool(true));
            Ok(JsValue::Array(build_claude_features(
                claude_config.get("model").and_then(JsValue::as_str),
                fast,
            )))
        }))
    }

    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        Box::pin(async move {
            is_available(self.options.runtime_settings.as_ref(), &self.process_env())
        })
    }
}
