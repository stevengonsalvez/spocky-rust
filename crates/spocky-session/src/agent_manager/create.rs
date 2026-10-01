//! `createAgent`, `registerSession`, and session config preparation from
//! pinned Paseo `agent/agent-manager.ts`.

use std::io;
use std::sync::Arc;

use spocky_store::js_value::{JsObject, JsValue};

use super::{AgentLifecycle, AgentManager, ManagedAgent, ManagedAgentSnapshot, validate_agent_id};
use crate::agent_projection::{AgentAttention, SnapshotOverrides};
use crate::agent_sdk::{
    AgentClient, AgentCreateSessionOptions, AgentError, AgentLaunchContext, AgentResumePurpose,
    AgentResumeSessionOptions, AgentSession, FetchCatalogOptions,
};
use crate::runtime_mcp_config::{strip_internal_paseo_mcp_server, with_runtime_paseo_mcp_server};
use crate::text::js_trim;
use spocky_contracts::js::{js_string, spread, spread_into, truthy};

/// `CreateAgentOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreateAgentOptions {
    /// `labels: Record<string, string>`.
    pub labels: Option<JsValue>,
    pub initial_prompt: Option<String>,
    /// `env: Record<string, string>`.
    pub env: Option<JsObject>,
    pub persist_session: Option<bool>,
    /// `initialTitle?: string | null`.
    pub initial_title: Option<String>,
    /// `workspaceId: string | undefined`; `None` keeps the agent out of the
    /// sidebar, as the baseline's explicit undefined does.
    pub workspace_id: Option<String>,
    /// `AgentOwner`.
    pub owner: Option<JsValue>,
}

/// `registerSession` options the create and resume paths pass.
#[derive(Default)]
pub(crate) struct RegisterOptions {
    pub(crate) labels: Option<JsValue>,
    pub(crate) initial_title: Option<String>,
    pub(crate) workspace_id: Option<String>,
    pub(crate) owner: Option<JsValue>,
    pub(crate) history_primed: Option<bool>,
    pub(crate) created_at_millis: Option<i64>,
    pub(crate) updated_at_millis: Option<i64>,
    /// `lastUserMessageAt ?? null`.
    pub(crate) last_user_message_at_millis: Option<i64>,
    /// `resolveInitialAttention(attention)`.
    pub(crate) attention: Option<AgentAttention>,
    /// `persistence ?? session.describePersistence()`.
    pub(crate) persistence: Option<JsValue>,
    /// Bringing a known agent back: installing the session is not activity
    /// in it, so `updatedAt` is not touched.
    pub(crate) restoring: bool,
}

/// `resumeAgentFromPersistence` options: what the stored record says
/// about the agent being brought back.
#[derive(Debug, Clone, Default)]
pub struct ResumeAgentOptions {
    pub created_at_millis: Option<i64>,
    pub updated_at_millis: Option<i64>,
    pub last_user_message_at_millis: Option<i64>,
    pub labels: Option<JsValue>,
    pub workspace_id: Option<String>,
    pub owner: Option<JsValue>,
    pub attention: Option<AgentAttention>,
}

/// `PreparedSessionConfig`.
struct PreparedSessionConfig {
    stored_config: JsValue,
    launch_config: JsValue,
    paseo_tool_policy: Option<JsValue>,
}

fn config_text<'a>(config: &'a JsValue, key: &str) -> Option<&'a str> {
    config.get(key).and_then(JsValue::as_str)
}

/// `formatProviderList`.
fn format_provider_list(providers: &[String]) -> String {
    if providers.is_empty() {
        "none".to_owned()
    } else {
        providers.join(", ")
    }
}

/// libuv's `uv_strerror` text for the errors `stat` reports.
fn uv_description(code: &str) -> &'static str {
    match code {
        "EACCES" => "permission denied",
        "ENOTDIR" => "not a directory",
        "ELOOP" => "too many symbolic links encountered",
        "ENAMETOOLONG" => "name too long",
        "EPERM" => "operation not permitted",
        "EIO" => "i/o error",
        "EMFILE" => "too many open files",
        _ => "unknown error",
    }
}

/// `assertUsableWorkingDirectory(cwd)`.
fn assert_usable_working_directory(cwd: &str) -> Result<(), AgentError> {
    match std::fs::metadata(cwd) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(AgentError::new(format!(
            "Working directory is not a directory: {cwd}"
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(AgentError::new(format!(
            "Working directory does not exist: {cwd}"
        ))),
        Err(error) => {
            let code = error
                .raw_os_error()
                .and_then(crate::git::errno_name)
                .unwrap_or("UNKNOWN");
            Err(AgentError::new(format!(
                "{code}: {}, stat '{cwd}'",
                uv_description(code)
            )))
        }
    }
}

/// `attachPersistenceCwd(handle, cwd)`.
pub(crate) fn attach_persistence_cwd(handle: Option<JsValue>, cwd: &str) -> Option<JsValue> {
    let handle = handle.filter(|handle| truthy(Some(handle)))?;
    let mut metadata = spread(handle.get("metadata"));
    metadata.insert("cwd", JsValue::String(cwd.to_owned()));
    let mut attached = spread(Some(&handle));
    attached.insert("metadata", JsValue::Object(metadata));
    Some(JsValue::Object(attached))
}

impl AgentManager {
    pub(crate) fn assert_accepting_agent_registrations(&self) -> Result<(), AgentError> {
        if self.lock().accepting_agent_registrations {
            Ok(())
        } else {
            Err(shutting_down())
        }
    }

    /// `assertAgentRegistrationActive`: still accepting, and the map still
    /// holds this registration (`registration` is the agent's session).
    fn assert_agent_registration_active(
        &self,
        agent_id: &str,
        session: &Arc<dyn AgentSession>,
    ) -> Result<(), AgentError> {
        let state = self.lock();
        let active = state.accepting_agent_registrations
            && state.agent(agent_id).is_some_and(|agent| {
                agent
                    .session
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, session))
            });
        if active { Ok(()) } else { Err(shutting_down()) }
    }

    /// `createAgent(config, agentId, options)`.
    ///
    /// # Errors
    ///
    /// The baseline's validation, provider, and registration errors.
    pub async fn create_agent(
        &self,
        config: JsValue,
        agent_id: Option<String>,
        options: CreateAgentOptions,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        self.assert_accepting_agent_registrations()?;
        let resolved_agent_id = validate_agent_id(
            &agent_id.unwrap_or_else(|| (self.inner.id_factory)()),
            "createAgent",
        )?;
        let internal = config.get("internal").cloned();
        let config = if self.lock().plugin_lifecycle && !truthy(internal.as_ref()) {
            let mut parsed = spread(Some(&before_agent_create(&config)?));
            parsed.insert("internal", internal.unwrap_or(JsValue::Undefined));
            JsValue::Object(parsed)
        } else {
            config
        };
        self.delete_agent_state(&resolved_agent_id);
        let prepared = self
            .prepare_session_config(
                &config,
                &resolved_agent_id,
                options.env.as_ref(),
                AgentResumePurpose::Interactive,
            )
            .await?;
        let provider = config_text(&prepared.stored_config, "provider")
            .unwrap_or_default()
            .to_owned();
        self.require_enabled_provider(&provider)?;
        let client = self.require_available_client(&provider).await?;
        self.lock().paseo_tool_policies.insert(
            resolved_agent_id.clone(),
            prepared.paseo_tool_policy.clone(),
        );
        let cwd = config_text(&prepared.stored_config, "cwd")
            .unwrap_or_default()
            .to_owned();
        let launch_context =
            Self::build_launch_context(&resolved_agent_id, &cwd, options.env.as_ref());
        let create_options =
            options
                .persist_session
                .map(|persist_session| AgentCreateSessionOptions {
                    persist_session: Some(persist_session),
                });
        let session = client
            .create_session(prepared.launch_config, Some(launch_context), create_options)
            .await?;
        self.require_external_mcp_support(&session, &prepared.stored_config)
            .await?;
        self.register_session(
            session,
            prepared.stored_config,
            &resolved_agent_id,
            RegisterOptions {
                labels: options.labels,
                initial_title: options.initial_title,
                workspace_id: options.workspace_id,
                owner: options.owner,
                history_primed: Some(true),
                ..RegisterOptions::default()
            },
        )
        .await
    }

    /// `resumeAgentFromPersistence(handle, overrides, agentId, options,
    /// resumeOptions)`: reopens a stored agent's provider session and
    /// registers it as restored, inside the agent's lifecycle lane.
    ///
    /// # Errors
    ///
    /// The baseline's id, configuration, provider and registration errors.
    pub async fn resume_agent_from_persistence(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        agent_id: Option<String>,
        options: ResumeAgentOptions,
        resume_options: Option<AgentResumeSessionOptions>,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        let resolved_agent_id = validate_agent_id(
            &agent_id.unwrap_or_else(|| (self.inner.id_factory)()),
            "resumeAgentFromPersistence",
        )?;
        let lane = Self::lane(&mut self.lock().lifecycle_lanes, &resolved_agent_id);
        let _turn = lane.lock().await;
        self.resume_agent_from_persistence_internal(
            &handle,
            overrides.as_ref(),
            &resolved_agent_id,
            options,
            resume_options,
        )
        .await
    }

    async fn resume_agent_from_persistence_internal(
        &self,
        handle: &JsValue,
        overrides: Option<&JsValue>,
        agent_id: &str,
        options: ResumeAgentOptions,
        resume_options: Option<AgentResumeSessionOptions>,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        self.assert_accepting_agent_registrations()?;
        let resolved_agent_id = validate_agent_id(agent_id, "resumeAgentFromPersistence")?;
        let mut merged = spread(
            handle
                .get("metadata")
                .filter(|metadata| !matches!(metadata, JsValue::Undefined | JsValue::Null)),
        );
        spread_into(&mut merged, overrides);
        merged.insert(
            "provider",
            handle
                .get("provider")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        // Residency comes from durable state inside the lane: a queued
        // archive or restore may have completed since the caller read it.
        let registry = self.inner.registry.clone();
        let record = match &registry {
            Some(registry) => registry.get(&resolved_agent_id).await,
            None => None,
        };
        let current_resume_options = match &record {
            Some(record) => Some(AgentResumeSessionOptions {
                purpose: Some(if truthy(record.get("archivedAt")) {
                    AgentResumePurpose::History
                } else {
                    AgentResumePurpose::Interactive
                }),
            }),
            None => resume_options,
        };
        let purpose = current_resume_options
            .and_then(|options| options.purpose)
            .unwrap_or(AgentResumePurpose::Interactive);
        let prepared = self
            .prepare_session_config(&JsValue::Object(merged), &resolved_agent_id, None, purpose)
            .await?;
        let provider = js_string(handle.get("provider"));
        let Some(client) = self.lock().client(&provider) else {
            return Err(AgentError::new(format!(
                "No client registered for provider '{provider}'"
            )));
        };
        if !client.is_available(None, None).await? {
            return Err(AgentError::new(format!(
                "Provider '{provider}' is not available. Please ensure the CLI is installed."
            )));
        }
        self.lock().paseo_tool_policies.insert(
            resolved_agent_id.clone(),
            prepared.paseo_tool_policy.clone(),
        );
        let cwd = js_string(prepared.stored_config.get("cwd"));
        let launch_context = Self::build_launch_context(&resolved_agent_id, &cwd, None);
        let session = client
            .resume_session(
                handle.clone(),
                Some(prepared.launch_config),
                Some(launch_context),
                current_resume_options,
            )
            .await?;
        self.require_external_mcp_support(&session, &prepared.stored_config)
            .await?;
        self.register_session(
            session,
            prepared.stored_config,
            &resolved_agent_id,
            RegisterOptions {
                labels: options.labels,
                workspace_id: options.workspace_id,
                owner: options.owner,
                created_at_millis: options.created_at_millis,
                updated_at_millis: options.updated_at_millis,
                last_user_message_at_millis: options.last_user_message_at_millis,
                attention: options.attention,
                persistence: Some(handle.clone()),
                restoring: true,
                ..RegisterOptions::default()
            },
        )
        .await
    }

    /// `deleteAgentState` without a durable store:
    /// `discardRetainedAgentState`.
    pub(crate) fn delete_agent_state(&self, agent_id: &str) {
        let mut state = self.lock();
        state.timeline.delete(agent_id);
        state.paseo_tool_policies.remove(agent_id);
    }

    /// `normalizeConfig(config, { purpose: "interactive" })`.
    /// Reading an archived agent's history (`purpose: "history"`) runs
    /// nothing, so its working directory need not exist.
    async fn normalize_config(
        &self,
        config: &JsValue,
        purpose: AgentResumePurpose,
    ) -> Result<JsValue, AgentError> {
        let mut normalized = spread(Some(config));
        if let Some(cwd) = normalized.get("cwd").filter(|cwd| truthy(Some(cwd))) {
            let resolved = crate::paths::resolve_from_cwd(&js_string(Some(cwd)));
            normalized.insert("cwd", JsValue::String(resolved.clone()));
            if purpose != AgentResumePurpose::History {
                assert_usable_working_directory(&resolved)?;
            }
        }
        if let Some(model) = normalized.get("model").and_then(JsValue::as_str) {
            let trimmed = js_trim(model).to_owned();
            normalized.insert(
                "model",
                if trimmed.is_empty() || trimmed == "default" {
                    JsValue::Undefined
                } else {
                    JsValue::String(trimmed)
                },
            );
        }
        if !truthy(normalized.get("model")) {
            let normalized_value = JsValue::Object(normalized.clone());
            if let Some(default_model) = self.resolve_default_model_id(&normalized_value).await {
                normalized.insert("model", JsValue::String(default_model));
            }
        }
        self.apply_provider_configuration(&JsValue::Object(normalized))
    }

    /// `resolveDefaultModelId(config)`.
    async fn resolve_default_model_id(&self, config: &JsValue) -> Option<String> {
        let provider = js_string(config.get("provider"));
        let client = self.lock().client(&provider)?;
        let catalog = client
            .fetch_catalog(
                FetchCatalogOptions::Workspace {
                    cwd: js_string(config.get("cwd")),
                    force: false,
                },
                None,
            )
            .await
            .ok()?;
        let models = catalog.get("models").and_then(JsValue::as_array)?;
        let model = models
            .iter()
            .find(|model| truthy(model.get("isDefault")))
            .or_else(|| models.first())?;
        model.get("id").and_then(JsValue::as_str).map(str::to_owned)
    }

    /// `applyProviderConfiguration(config)`.
    fn apply_provider_configuration(&self, config: &JsValue) -> Result<JsValue, AgentError> {
        let provider = js_string(config.get("provider"));
        let definition = self
            .lock()
            .provider_definitions
            .iter()
            .find(|(id, _)| *id == provider)
            .map(|(_, definition)| definition.clone());
        let provider_options = config
            .get("providerOptions")
            .filter(|options| !matches!(options, JsValue::Undefined));
        let validate = definition
            .as_ref()
            .and_then(|definition| definition.validate_options.clone());
        if provider_options.is_some() && validate.is_none() {
            return Err(AgentError::new(format!(
                "Provider '{provider}' does not accept providerOptions"
            )));
        }
        let validated = match &validate {
            Some(validate) => validate(provider_options)?,
            None => None,
        };
        let with_options = match definition
            .as_ref()
            .and_then(|definition| definition.apply_options.clone())
        {
            Some(apply) => apply(config, validated.as_ref()),
            None => config.clone(),
        };
        validate_tool_policy_servers(&with_options)?;
        let apply_tool_policy = definition
            .as_ref()
            .and_then(|definition| definition.apply_tool_policy.clone());
        if truthy(with_options.get("toolPolicy")) && apply_tool_policy.is_none() {
            return Err(AgentError::new(format!(
                "Provider '{provider}' cannot preapprove exact MCP tools for unattended execution"
            )));
        }
        Ok(match apply_tool_policy {
            Some(apply) => apply(&with_options, with_options.get("toolPolicy")),
            None => with_options,
        })
    }

    /// `prepareSessionConfig(config, agentId, { env })`.
    async fn prepare_session_config(
        &self,
        config: &JsValue,
        agent_id: &str,
        _env: Option<&JsObject>,
        purpose: AgentResumePurpose,
    ) -> Result<PreparedSessionConfig, AgentError> {
        let stored_config = self
            .normalize_config(&strip_internal_paseo_mcp_server(config), purpose)
            .await?;
        let provider = js_string(stored_config.get("provider"));
        let (tools_enabled, mcp_base_url, append_system_prompt) = {
            let state = self.lock();
            (
                state.paseo_tools_enabled,
                state.mcp_base_url.clone(),
                state.append_system_prompt.clone(),
            )
        };
        let paseo_tool_policy = if tools_enabled {
            self.inner
                .resolve_paseo_tool_policy
                .as_ref()
                .and_then(|resolve| resolve(&provider))
        } else {
            let mut disabled = JsObject::new();
            disabled.insert("enabled", JsValue::Bool(false));
            Some(JsValue::Object(disabled))
        };
        // `isPaseoToolPolicyEnabled`: `policy?.enabled !== false`.
        let policy_enabled = paseo_tool_policy
            .as_ref()
            .and_then(|policy| policy.get("enabled"))
            != Some(&JsValue::Bool(false));
        let runtime = with_runtime_paseo_mcp_server(
            &stored_config,
            agent_id,
            mcp_base_url
                .as_deref()
                .filter(|_| tools_enabled && policy_enabled),
            self.inner.mcp_auth_token.as_deref(),
        );
        let launch_config = apply_daemon_append_system_prompt(&runtime, &append_system_prompt);
        Ok(PreparedSessionConfig {
            stored_config,
            launch_config,
            paseo_tool_policy,
        })
    }

    /// `buildLaunchContext` without plugin hooks or a Paseo tool catalog:
    /// `{ agentId, env: { ...env, PASEO_AGENT_ID, PASEO_AGENT_CWD } }`.
    fn build_launch_context(
        agent_id: &str,
        cwd: &str,
        env: Option<&JsObject>,
    ) -> AgentLaunchContext {
        let mut launch_env = JsObject::new();
        if let Some(env) = env {
            spread_into(&mut launch_env, Some(&JsValue::Object(env.clone())));
        }
        launch_env.insert("PASEO_AGENT_ID", JsValue::String(agent_id.to_owned()));
        launch_env.insert("PASEO_AGENT_CWD", JsValue::String(cwd.to_owned()));
        AgentLaunchContext {
            agent_id: Some(agent_id.to_owned()),
            env: Some(launch_env),
            paseo_tools: None,
        }
    }

    /// `requireEnabledProvider`.
    fn require_enabled_provider(&self, provider: &str) -> Result<(), AgentError> {
        let disabled = self
            .lock()
            .provider_enabled
            .iter()
            .any(|(id, enabled)| id == provider && !*enabled);
        if disabled {
            Err(AgentError::new(format!(
                "Provider '{provider}' is disabled"
            )))
        } else {
            Ok(())
        }
    }

    /// `getConfiguredProviderIds`: enabled-flag providers, then clients.
    fn configured_provider_ids(&self) -> Vec<String> {
        let state = self.lock();
        let mut ids: Vec<String> = Vec::new();
        for id in state
            .provider_enabled
            .iter()
            .map(|(id, _)| id)
            .chain(state.clients.iter().map(|(id, _)| id))
        {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    /// `listProviderAvailability()`: `(provider, available, error)` per client.
    pub async fn list_provider_availability(&self) -> Vec<(String, bool, Option<String>)> {
        let clients: Vec<(String, Arc<dyn AgentClient>)> = self.lock().clients.clone();
        let mut availability = Vec::with_capacity(clients.len());
        for (provider, client) in clients {
            availability.push(match client.is_available(None, None).await {
                Ok(available) => (provider, available, None),
                Err(error) => (provider, false, Some(error.message)),
            });
        }
        availability
    }

    /// `requireAvailableClient({ provider })`.
    async fn require_available_client(
        &self,
        provider: &str,
    ) -> Result<Arc<dyn AgentClient>, AgentError> {
        let Some(client) = self.lock().client(provider) else {
            return Err(AgentError::new(format!(
                "Unknown provider '{provider}'. Configured providers: {}.",
                format_provider_list(&self.configured_provider_ids())
            )));
        };
        let unavailable_reason = match client.is_available(None, None).await {
            Ok(true) => return Ok(client),
            Ok(false) => None,
            Err(error) => Some(error.message),
        };
        let available: Vec<String> = self
            .list_provider_availability()
            .await
            .into_iter()
            .filter(|(_, available, _)| *available)
            .map(|(provider, _, _)| provider)
            .collect();
        let reason = unavailable_reason
            .map(|reason| format!(" Reason: {reason}."))
            .unwrap_or_default();
        Err(AgentError::new(format!(
            "Provider '{provider}' is not available.{reason} Available providers: {}. Use one of those providers, or install/configure '{provider}'.",
            format_provider_list(&available)
        )))
    }

    /// `closeUnregisteredSession`: errors are only logged.
    async fn close_unregistered_session(session: &Arc<dyn AgentSession>) {
        let _ = session.close().await;
    }

    /// `requireExternalMcpSupport`.
    async fn require_external_mcp_support(
        &self,
        session: &Arc<dyn AgentSession>,
        stored_config: &JsValue,
    ) -> Result<(), AgentError> {
        let has_servers = stored_config
            .get("mcpServers")
            .and_then(JsValue::as_object)
            .is_some_and(|servers| !servers.is_empty());
        if !has_servers
            || session.capabilities().get("supportsMcpServers") == Some(&JsValue::Bool(true))
        {
            return Ok(());
        }
        Self::close_unregistered_session(session).await;
        Err(AgentError::new(format!(
            "Provider '{}' does not support MCP servers",
            js_string(stored_config.get("provider"))
        )))
    }

    /// `resolveInitialPersistedTitle(agentId, config, fallbackTitle)`.
    async fn resolve_initial_persisted_title(
        &self,
        agent_id: &str,
        config: &JsValue,
        fallback_title: Option<String>,
    ) -> Option<String> {
        if let Some(registry) = &self.inner.registry
            && let Some(existing) = registry.get(agent_id).await
        {
            return existing
                .get("title")
                .and_then(JsValue::as_str)
                .map(str::to_owned);
        }
        config
            .get("title")
            .and_then(JsValue::as_str)
            .map(js_trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .or(fallback_title)
    }

    /// `initializeAgentTimelineForRegister`: whether the agent already has
    /// timeline rows; otherwise starts an empty timeline at `now`.
    fn initialize_agent_timeline_for_register(
        &self,
        agent_id: &str,
        now: i64,
    ) -> Result<bool, AgentError> {
        let mut state = self.lock();
        let already_primed = state.timeline.has(agent_id);
        if !already_primed {
            state
                .timeline
                .initialize(
                    agent_id,
                    Vec::new(),
                    None,
                    None,
                    Some(crate::clock::iso_from_millis(now)),
                )
                .map_err(|error| AgentError {
                    name: "TypeError".to_owned(),
                    message: error.0,
                })?;
        }
        Ok(already_primed)
    }

    /// `registerSession(session, config, agentId, options)` for a new agent.
    pub(crate) async fn register_session(
        &self,
        session: Arc<dyn AgentSession>,
        config: JsValue,
        agent_id: &str,
        options: RegisterOptions,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        let mut registered = false;
        let result = self
            .register_session_inner(&session, config, agent_id, options, &mut registered)
            .await;
        if result.is_err() && !registered {
            Self::close_unregistered_session(&session).await;
        }
        result
    }

    async fn register_session_inner(
        &self,
        session: &Arc<dyn AgentSession>,
        config: JsValue,
        agent_id: &str,
        options: RegisterOptions,
        registered: &mut bool,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        self.assert_accepting_agent_registrations()?;
        let resolved_agent_id = validate_agent_id(agent_id, "registerSession")?;
        if self.lock().agent(&resolved_agent_id).is_some() {
            return Err(AgentError::new(format!(
                "Agent with id {resolved_agent_id} already exists"
            )));
        }
        let initial_persisted_title = self
            .resolve_initial_persisted_title(&resolved_agent_id, &config, options.initial_title)
            .await;
        let now = crate::clock::now_millis();
        let durable_timeline_has_rows =
            self.initialize_agent_timeline_for_register(&resolved_agent_id, now)?;
        let snapshot = initial_snapshot(
            &resolved_agent_id,
            config,
            session,
            InitialAgentFields {
                labels: options.labels,
                workspace_id: options.workspace_id,
                owner: options.owner,
                history_primed: options.history_primed.unwrap_or(durable_timeline_has_rows),
                created_at_millis: options.created_at_millis,
                updated_at_millis: options.updated_at_millis,
                last_user_message_at_millis: options.last_user_message_at_millis,
                attention: options.attention,
                persistence: options.persistence,
            },
            now,
        );
        let initial_timeline = session.initial_timeline().unwrap_or_default();
        let startup_history = if !initial_timeline.is_empty() && !snapshot.history_primed {
            Some(super::events::read_startup_history(session.as_ref()).await?)
        } else {
            None
        };
        self.assert_accepting_agent_registrations()?;
        {
            let mut state = self.lock();
            state.agents.push((
                resolved_agent_id.clone(),
                ManagedAgent {
                    snapshot,
                    session: Some(Arc::clone(session)),
                    buffered_permission_resolutions: Vec::new(),
                    in_flight_permission_responses: Vec::new(),
                    foreground_turn_waiters: Vec::new(),
                    finalized_foreground_turn_ids: Vec::new(),
                    unsubscribe_session: None,
                },
            ));
            state
                .previous_statuses
                .insert(resolved_agent_id.clone(), AgentLifecycle::Initializing);
        }
        *registered = true;
        self.record_initial_timeline(&resolved_agent_id, initial_timeline, startup_history)
            .await?;
        self.refresh_runtime_info(&resolved_agent_id, false).await;
        self.assert_agent_registration_active(&resolved_agent_id, session)?;
        self.persist_snapshot(
            &resolved_agent_id,
            SnapshotOverrides {
                title: Some(initial_persisted_title),
                internal: None,
            },
        )
        .await?;
        self.assert_agent_registration_active(&resolved_agent_id, session)?;
        self.emit_state(&resolved_agent_id, false);
        self.refresh_session_state(&resolved_agent_id, false).await;
        self.assert_agent_registration_active(&resolved_agent_id, session)?;
        {
            let mut state = self.lock();
            if let Some(agent) = state.agent_mut(&resolved_agent_id) {
                agent.snapshot.lifecycle = AgentLifecycle::Idle;
                // Stamping now over a restored timestamp would rewrite the
                // workspace's "last used" every time a chat is reopened.
                if !options.restoring {
                    touch_updated_at(&mut agent.snapshot);
                }
            }
        }
        self.persist_snapshot(&resolved_agent_id, SnapshotOverrides::default())
            .await?;
        self.assert_agent_registration_active(&resolved_agent_id, session)?;
        self.emit_state(&resolved_agent_id, false);
        self.subscribe_to_session(&resolved_agent_id);
        self.get_agent(&resolved_agent_id).ok_or_else(shutting_down)
    }

    /// `registerSession`'s initial timeline: primed from the startup
    /// history when the agent's history was not primed, else the session's
    /// own initial rows.
    async fn record_initial_timeline(
        &self,
        agent_id: &str,
        initial_timeline: Vec<crate::agent_sdk::ImportedTimelineEntry>,
        startup_history: Option<super::events::ReplayedHistory>,
    ) -> Result<(), AgentError> {
        if !initial_timeline.is_empty() {
            if let Some(history) = startup_history {
                // Legacy or imported chats need their existing history
                // before the startup rows.
                self.prime_from_history(
                    agent_id,
                    &super::events::HydrateBroadcast::Now(false),
                    Box::new(history),
                )
                .await?;
            } else {
                for entry in initial_timeline {
                    self.record_timeline(agent_id, entry.item, entry.timestamp, None, None)?;
                }
            }
            self.refresh_session_persistence(agent_id);
        }
        Ok(())
    }
}

/// `pluginLifecycle.before("agent.create", { config, env })` with no plugin
/// loaded: the request is only parsed by
/// `CreateAgentRequestMessageSchema.pick({ config, env }).strict()`, so the
/// config keeps its schema keys, in schema order, and loses any other key.
/// `env` (a record of strings) parses to itself.
// ponytail: a config that fails the schema reports serde's message, not
// zod's issue list; callers pass wire-parsed configs, which never fail.
fn before_agent_create(config: &JsValue) -> Result<JsValue, AgentError> {
    let parse_error = |message: String| AgentError {
        name: "ZodError".to_owned(),
        message,
    };
    let parsed =
        <spocky_contracts::agent_config::AgentSessionConfig as serde::Deserialize>::deserialize(
            spocky_contracts::json::JsValueDeserializer(config),
        )
        .map_err(|error| parse_error(error.to_string()))?;
    let text = serde_json::to_string(&parsed).map_err(|error| parse_error(error.to_string()))?;
    spocky_store::js_value::parse(&text).map_err(|error| parse_error(error.to_string()))
}

/// The live-agent fields `registerSession` takes from its options.
struct InitialAgentFields {
    labels: Option<JsValue>,
    workspace_id: Option<String>,
    owner: Option<JsValue>,
    history_primed: bool,
    created_at_millis: Option<i64>,
    updated_at_millis: Option<i64>,
    last_user_message_at_millis: Option<i64>,
    attention: Option<AgentAttention>,
    persistence: Option<JsValue>,
}

/// `buildManagedAgentForRegister`: an initializing agent over `session`.
fn initial_snapshot(
    agent_id: &str,
    config: JsValue,
    session: &Arc<dyn AgentSession>,
    fields: InitialAgentFields,
    now: i64,
) -> ManagedAgentSnapshot {
    let cwd = js_string(config.get("cwd"));
    ManagedAgentSnapshot {
        id: agent_id.to_owned(),
        provider: js_string(config.get("provider")),
        cwd: cwd.clone(),
        workspace_id: fields.workspace_id,
        owner: fields.owner,
        capabilities: session.capabilities(),
        internal: config
            .get("internal")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
        config,
        runtime_info: None,
        created_at_millis: fields.created_at_millis.unwrap_or(now),
        updated_at_millis: fields.updated_at_millis.unwrap_or(now),
        available_modes: Vec::new(),
        features: None,
        current_mode_id: None,
        pending_permissions: Vec::new(),
        pending_replacement: false,
        persistence: attach_persistence_cwd(
            fields
                .persistence
                .or_else(|| session.describe_persistence()),
            &cwd,
        ),
        history_primed: fields.history_primed,
        last_user_message_at_millis: fields.last_user_message_at_millis,
        active_turn_id: None,
        active_turn_started_at_millis: None,
        last_usage: None,
        last_error: None,
        attention: fields.attention.unwrap_or(AgentAttention::None),
        labels: fields
            .labels
            .unwrap_or_else(|| JsValue::Object(JsObject::new())),
        lifecycle: AgentLifecycle::Initializing,
        active_foreground_turn_id: None,
    }
}

/// `touchUpdatedAt`: now, or one millisecond past the previous value.
pub(crate) fn touch_updated_at(agent: &mut ManagedAgentSnapshot) -> i64 {
    let now = crate::clock::now_millis();
    let next = if now > agent.updated_at_millis {
        now
    } else {
        agent.updated_at_millis + 1
    };
    agent.updated_at_millis = next;
    next
}

/// `AgentManagerShuttingDownError`.
pub(crate) fn shutting_down() -> AgentError {
    AgentError {
        name: "AgentManagerShuttingDownError".to_owned(),
        message: "Agent manager is shutting down".to_owned(),
    }
}

/// `applyDaemonAppendSystemPrompt(config)`.
fn apply_daemon_append_system_prompt(config: &JsValue, append_system_prompt: &str) -> JsValue {
    let trimmed = js_trim(append_system_prompt);
    let mut next = JsObject::new();
    for (key, value) in spread(Some(config)).iter() {
        if key != "daemonAppendSystemPrompt" {
            next.insert(key, value.clone());
        }
    }
    if !trimmed.is_empty() {
        next.insert(
            "daemonAppendSystemPrompt",
            JsValue::String(trimmed.to_owned()),
        );
    }
    JsValue::Object(next)
}

/// `validateToolPolicyServers(config)`.
fn validate_tool_policy_servers(config: &JsValue) -> Result<(), AgentError> {
    let Some(policy) = config
        .get("toolPolicy")
        .filter(|policy| truthy(Some(policy)))
    else {
        return Ok(());
    };
    let server_names: Vec<String> = config
        .get("mcpServers")
        .and_then(JsValue::as_object)
        .map(|servers| servers.iter().map(|(name, _)| name.to_owned()).collect())
        .unwrap_or_default();
    // `for (const grant of config.toolPolicy.preapproved)`: arrays and
    // strings (per code point) iterate; anything else throws.
    let grants: Vec<JsValue> = match policy.get("preapproved") {
        Some(JsValue::Array(grants)) => grants.clone(),
        Some(JsValue::String(text)) => text
            .chars()
            .map(|character| JsValue::String(character.to_string()))
            .collect(),
        _ => {
            return Err(AgentError {
                name: "TypeError".to_owned(),
                message: "config.toolPolicy.preapproved is not iterable".to_owned(),
            });
        }
    };
    for grant in grants {
        if let JsValue::Undefined | JsValue::Null = grant {
            let receiver = if grant.is_null() { "null" } else { "undefined" };
            return Err(AgentError {
                name: "TypeError".to_owned(),
                message: format!("Cannot read properties of {receiver} (reading 'server')"),
            });
        }
        let server = grant.get("server");
        let known = server
            .and_then(JsValue::as_str)
            .is_some_and(|server| server_names.iter().any(|name| name == server));
        if !known {
            let server = js_string(server);
            return Err(AgentError::new(format!(
                "toolPolicy preapproval '{server}.{}' requires MCP server '{server}' in the same agent request",
                js_string(grant.get("tool"))
            )));
        }
    }
    Ok(())
}
