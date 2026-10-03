//! `createAgent`, `registerSession`, and session config preparation from
//! pinned Paseo `agent/agent-manager.ts`.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use spocky_contracts::zod::Outcome;
use spocky_store::js_value::{JsObject, JsValue, js_text_to_utf8};

use super::{
    AgentLifecycle, AgentManager, AgentManagerEvent, ManagedAgent, ManagedAgentSnapshot,
    validate_agent_id,
};
use crate::agent_identity::resolve_create_agent_titles;
use crate::agent_projection::{AgentAttention, SnapshotOverrides};
use crate::agent_prompt::is_system_injected_envelope;
use crate::agent_sdk::{
    AgentClient, AgentCreateSessionOptions, AgentError, AgentLaunchContext, AgentResumePurpose,
    AgentResumeSessionOptions, AgentSession, FetchCatalogOptions, ImportProviderSessionContext,
    ImportProviderSessionInput, ImportedTimelineEntry,
};
use crate::runtime_mcp_config::{strip_internal_paseo_mcp_server, with_runtime_paseo_mcp_server};
use crate::text::js_trim;
use crate::timeline::{SeedRow, TimelineRow, TimelineSeed};
use crate::timeline_content::limit_agent_timeline_item_content;
use spocky_contracts::js::{js_string, spread, spread_into, truthy};

/// `importProviderSession`'s input.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportProviderSessionRequest {
    pub provider: String,
    pub provider_handle_id: String,
    pub cwd: String,
    pub workspace_id: String,
    /// `Record<string, string>`.
    pub labels: Option<JsValue>,
}

/// `buildImportedTimelineRows(entries)`: provider rows renumbered from 1,
/// without the system-injected user messages.
fn build_imported_timeline_rows(
    entries: &[ImportedTimelineEntry],
) -> Result<Vec<TimelineRow>, AgentError> {
    let mut rows: Vec<TimelineRow> = Vec::new();
    for entry in entries {
        let item = &entry.item;
        if item.get("type").and_then(JsValue::as_str) == Some("user_message")
            && is_system_injected_envelope(&js_string(item.get("text")))
        {
            continue;
        }
        let item = limit_agent_timeline_item_content(item.clone()).map_err(|error| AgentError {
            name: "TypeError".to_owned(),
            message: error.0,
        })?;
        rows.push(TimelineRow {
            seq: i64::try_from(rows.len()).unwrap_or(i64::MAX) + 1,
            timestamp: entry
                .timestamp
                .clone()
                .unwrap_or_else(crate::clock::now_iso),
            item,
            turn_id: None,
            provider_message_id: None,
        });
    }
    Ok(rows)
}

/// `resolveImportedAgentTitle(config, timelineRows)`.
fn resolve_imported_agent_title(config: &JsValue, rows: &[TimelineRow]) -> Option<String> {
    let prompt = rows.iter().find_map(|row| {
        if row.item.get("type").and_then(JsValue::as_str) != Some("user_message") {
            return None;
        }
        let text = js_trim(&js_string(row.item.get("text"))).to_owned();
        (!text.is_empty()).then_some(text)
    })?;
    let titles =
        resolve_create_agent_titles(config.get("title").and_then(JsValue::as_str), Some(&prompt));
    titles.explicit_title.or(titles.provisional_title)
}

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
    pub(crate) last_usage: Option<JsValue>,
    pub(crate) last_error: Option<String>,
    /// Bringing a known agent back: installing the session is not activity
    /// in it, so `updatedAt` is not touched.
    pub(crate) restoring: bool,
    /// `timelineRows`: rows to seed as they are.
    pub(crate) timeline_rows: Vec<TimelineRow>,
    /// `timelineNextSeq`.
    pub(crate) timeline_next_seq: Option<i64>,
    /// `publishWhenReady`: the agent is first announced once it is ready.
    pub(crate) publish_when_ready: bool,
}

/// `reloadAgentSession` options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReloadAgentOptions {
    /// `rehydrateFromDisk`: wipe the in-memory timeline and read the
    /// provider's history again.
    pub rehydrate_from_disk: bool,
}

/// `RELOAD_SESSION_CLOSE_TIMEOUT_MS`.
pub(crate) const RELOAD_SESSION_CLOSE_TIMEOUT_MS: u64 = 3_000;

/// The close of a session a reload replaced (`reloadedSessionCloses`, a
/// `WeakMap` keyed by session).
pub(crate) struct ReloadedClose {
    session: std::sync::Weak<dyn AgentSession>,
    result: tokio::sync::watch::Receiver<Option<Result<(), AgentError>>>,
}

/// What `reloadAgentSessionInternal` has worked out before it closes the
/// old session.
struct ReloadPlan<'a> {
    agent_id: &'a str,
    existing: ManagedAgentSnapshot,
    existing_session: Arc<dyn AgentSession>,
    handle: Option<JsValue>,
    client: Arc<dyn AgentClient>,
    prepared: PreparedSessionConfig,
    launch_context: AgentLaunchContext,
    rehydrate_from_disk: bool,
}

/// How far a reload got, for its `catch` and `finally`.
#[derive(Default)]
struct ReloadProgress {
    closed_existing: Option<ManagedAgentSnapshot>,
    session: Option<Arc<dyn AgentSession>>,
    handed_to_registration: bool,
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

/// `assertUsableWorkingDirectory(cwd)`. `cwd` is JavaScript text; node
/// encodes it to UTF-8 for the `stat`, so a lone surrogate is U+FFFD there.
fn assert_usable_working_directory(cwd: &str) -> Result<(), AgentError> {
    match std::fs::metadata(js_text_to_utf8(cwd)) {
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
        let _registration = self.track_agent_registration();
        self.assert_accepting_agent_registrations()?;
        let resolved_agent_id = validate_agent_id(
            &agent_id.unwrap_or_else(|| (self.inner.id_factory)()),
            "createAgent",
        )?;
        let internal = config.get("internal").cloned();
        let config = if self.lock().plugin_lifecycle && !truthy(internal.as_ref()) {
            let mut parsed = spread(Some(&before_agent_create(&config, options.env.as_ref())?));
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

    /// `importProviderSession(input)`: registers a session the provider
    /// already holds, seeding the agent's timeline from its history.
    ///
    /// # Errors
    ///
    /// The shutdown, provider and client errors of a create, `Provider '<p>'
    /// does not support importing sessions`, and the provider's own import
    /// or registration failure.
    pub async fn import_provider_session(
        &self,
        input: ImportProviderSessionRequest,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        let _registration = self.track_agent_registration();
        self.assert_accepting_agent_registrations()?;
        let resolved_agent_id =
            validate_agent_id(&(self.inner.id_factory)(), "importProviderSession")?;
        self.require_enabled_provider(&input.provider)?;
        let client = self.require_available_client(&input.provider).await?;
        if !client.supports_import_session() {
            return Err(AgentError::new(format!(
                "Provider '{}' does not support importing sessions",
                input.provider
            )));
        }
        let mut provider_config = JsObject::new();
        provider_config.insert("provider", JsValue::String(input.provider.clone()));
        provider_config.insert("cwd", JsValue::String(input.cwd.clone()));
        let prepared = self
            .prepare_session_config(
                &JsValue::Object(provider_config),
                &resolved_agent_id,
                None,
                AgentResumePurpose::Interactive,
            )
            .await?;
        self.lock().paseo_tool_policies.insert(
            resolved_agent_id.clone(),
            prepared.paseo_tool_policy.clone(),
        );
        let cwd = config_text(&prepared.stored_config, "cwd")
            .unwrap_or_default()
            .to_owned();
        let launch_context = Self::build_launch_context(&resolved_agent_id, &cwd, None);
        let Some(import) = client.import_session(
            ImportProviderSessionInput {
                provider_handle_id: input.provider_handle_id.clone(),
                cwd: input.cwd.clone(),
            },
            ImportProviderSessionContext {
                config: prepared.launch_config.clone(),
                stored_config: prepared.stored_config.clone(),
                launch_context: Some(launch_context),
            },
        ) else {
            return Err(AgentError::new(format!(
                "Provider '{}' does not support importing sessions",
                input.provider
            )));
        };
        let imported = import.await?;
        let session = Arc::clone(&imported.session);
        let prepared_import = async {
            let config = self
                .normalize_config(
                    &strip_internal_paseo_mcp_server(&imported.config),
                    AgentResumePurpose::Interactive,
                )
                .await?;
            let rows = build_imported_timeline_rows(&imported.timeline)?;
            let title = resolve_imported_agent_title(&config, &rows);
            Ok::<_, AgentError>((config, rows, title))
        }
        .await;
        let (config, rows, title) = match prepared_import {
            Ok(prepared) => prepared,
            Err(error) => {
                self.close_unregistered_session(&session).await;
                return Err(error);
            }
        };
        let next_seq = i64::try_from(rows.len()).unwrap_or(i64::MAX) + 1;
        let agent = self
            .register_session(
                session,
                config,
                &resolved_agent_id,
                RegisterOptions {
                    labels: input.labels,
                    workspace_id: Some(input.workspace_id),
                    timeline_rows: rows,
                    timeline_next_seq: Some(next_seq),
                    persistence: Some(imported.persistence),
                    history_primed: Some(true),
                    initial_title: title,
                    publish_when_ready: true,
                    ..RegisterOptions::default()
                },
            )
            .await?;
        self.replay_provider_subagent_events(&agent.id, imported.provider_subagent_events)?;
        Ok(agent)
    }

    /// Applies the provider sub-agent events an import carried, in order.
    fn replay_provider_subagent_events(
        &self,
        agent_id: &str,
        events: Option<Vec<JsValue>>,
    ) -> Result<(), AgentError> {
        for event in events.unwrap_or_default() {
            let provider = js_string(event.get("provider"));
            let inner = event.get("event").cloned().unwrap_or(JsValue::Undefined);
            let mut state = self.lock();
            let update = state
                .provider_subagents
                .apply(agent_id, &provider, &inner)
                .map_err(|error| AgentError {
                    name: "TypeError".to_owned(),
                    message: error.to_string(),
                })?;
            self.dispatch(&state, AgentManagerEvent::ProviderSubagent(update));
        }
        Ok(())
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
        let _registration = self.track_agent_registration();
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

    /// `reloadAgentSession(agentId, overrides, options)`: closes the agent's
    /// session and opens a replacement from the same persistence handle (or
    /// a new session when it has none), keeping the agent's labels,
    /// timestamps and timeline.
    ///
    /// # Errors
    ///
    /// The unknown-agent and no-session errors, `AgentRunCancellationError`
    /// when an active run cannot be cancelled, `No client registered for
    /// provider '<p>'`, `Provider '<p>' does not support MCP servers`,
    /// `Timed out closing previous session during refresh`, or the
    /// provider's, persistence or registration error. A failure after the
    /// old session closed leaves the agent closed.
    pub async fn reload_agent_session(
        &self,
        agent_id: &str,
        overrides: Option<JsValue>,
        options: ReloadAgentOptions,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        let _registration = self.track_agent_registration();
        let lane = Self::lane(&mut self.lock().lifecycle_lanes, agent_id);
        let _turn = lane.lock().await;
        self.reload_agent_session_internal(agent_id, overrides.as_ref(), options)
            .await
    }

    /// The agent and its session, as `requireSessionAgent` hands them out.
    fn require_session_agent(
        &self,
        agent_id: &str,
    ) -> Result<(ManagedAgentSnapshot, Arc<dyn AgentSession>), AgentError> {
        let state = self.lock();
        let agent = Self::require_agent(&state, agent_id)?;
        let Some(session) = agent.session.clone() else {
            return Err(AgentError::new(format!(
                "Agent '{}' has no managed session",
                agent.snapshot.id
            )));
        };
        Ok((agent.snapshot.clone(), session))
    }

    async fn reload_agent_session_internal(
        &self,
        agent_id: &str,
        overrides: Option<&JsValue>,
        options: ReloadAgentOptions,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        self.assert_accepting_agent_registrations()?;
        let (mut existing, mut existing_session) = self.require_session_agent(agent_id)?;
        if self.has_in_flight_run(agent_id) {
            self.cancel_agent_run_before(agent_id, "reload").await?;
            (existing, existing_session) = self.require_session_agent(agent_id)?;
        }
        let handle = existing.persistence.clone();
        let provider = handle
            .as_ref()
            .and_then(|handle| handle.get("provider"))
            .filter(|provider| !matches!(provider, JsValue::Undefined | JsValue::Null))
            .map_or_else(|| existing.provider.clone(), |p| js_string(Some(p)));
        let Some(client) = self.lock().client(&provider) else {
            return Err(AgentError::new(format!(
                "No client registered for provider '{provider}'"
            )));
        };
        let mut refresh = spread(Some(&existing.config));
        spread_into(&mut refresh, overrides);
        refresh.insert("provider", JsValue::String(provider.clone()));
        let prepared = self
            .prepare_session_config(
                &JsValue::Object(refresh),
                agent_id,
                None,
                AgentResumePurpose::Interactive,
            )
            .await?;
        let previous_policy = self.lock().paseo_tool_policies.get(agent_id).cloned();
        let cwd = js_string(prepared.stored_config.get("cwd"));
        let launch_context = Self::build_launch_context(agent_id, &cwd, None);
        let has_mcp_servers = matches!(
            prepared.stored_config.get("mcpServers"),
            Some(JsValue::Object(servers)) if servers.iter().next().is_some()
        );
        if has_mcp_servers
            && existing_session.capabilities().get("supportsMcpServers")
                != Some(&JsValue::Bool(true))
        {
            return Err(AgentError::new(format!(
                "Provider '{provider}' does not support MCP servers"
            )));
        }
        let plan = ReloadPlan {
            agent_id,
            existing,
            existing_session,
            handle,
            client,
            prepared,
            launch_context,
            rehydrate_from_disk: options.rehydrate_from_disk,
        };
        let mut progress = ReloadProgress::default();
        let result = self.swap_reloaded_session(&plan, &mut progress).await;
        if let Err(error) = &result {
            if let Some(closed) = progress.closed_existing.take() {
                let mut state = self.lock();
                self.emit_detached_state_locked(&mut state, closed);
            } else {
                let same_agent = self.lock().agent(agent_id).is_some_and(|agent| {
                    agent
                        .session
                        .as_ref()
                        .is_some_and(|session| Arc::ptr_eq(session, &plan.existing_session))
                });
                if same_agent {
                    if let Some(agent) = self.lock().agent_mut(agent_id) {
                        agent.snapshot.lifecycle = AgentLifecycle::Error;
                        agent.snapshot.last_error = Some(error.message.clone());
                    }
                    self.emit_state(agent_id, true);
                }
            }
        }
        if !progress.handed_to_registration {
            {
                let mut state = self.lock();
                match previous_policy {
                    Some(policy) => {
                        state
                            .paseo_tool_policies
                            .insert(agent_id.to_owned(), policy);
                    }
                    None => {
                        state.paseo_tool_policies.remove(agent_id);
                    }
                }
            }
            if let Some(session) = &progress.session {
                self.close_unregistered_session(session).await;
            }
        }
        result
    }

    /// The `try` of `reloadAgentSessionInternal`: close the old session,
    /// close the agent, open the replacement and register it.
    async fn swap_reloaded_session(
        &self,
        plan: &ReloadPlan<'_>,
        progress: &mut ReloadProgress,
    ) -> Result<ManagedAgentSnapshot, AgentError> {
        let agent_id = plan.agent_id;
        // A persisted thread can have only one writer, even when its turn
        // is idle.
        self.close_reloaded_session(&plan.existing_session, agent_id)
            .await?;
        self.drain_session_events_async(agent_id).await;
        let closed = {
            let mut state = self.lock();
            self.cancel_running_provider_subagents(&mut state, agent_id);
            self.prepare_agent_for_closure(&mut state, agent_id, "agent reloaded")
        }
        .unwrap_or_else(|| {
            let mut closed = plan.existing.clone();
            closed.lifecycle = AgentLifecycle::Closed;
            closed
        });
        progress.closed_existing = Some(closed.clone());
        self.persist_snapshot_of(agent_id, Some(closed.clone()), SnapshotOverrides::default())
            .await?;
        self.assert_accepting_agent_registrations()?;
        self.lock()
            .paseo_tool_policies
            .insert(agent_id.to_owned(), plan.prepared.paseo_tool_policy.clone());
        let session = match &plan.handle {
            Some(handle) => {
                plan.client
                    .resume_session(
                        handle.clone(),
                        Some(plan.prepared.launch_config.clone()),
                        Some(plan.launch_context.clone()),
                        None,
                    )
                    .await?
            }
            None => {
                plan.client
                    .create_session(
                        plan.prepared.launch_config.clone(),
                        Some(plan.launch_context.clone()),
                        None,
                    )
                    .await?
            }
        };
        progress.session = Some(Arc::clone(&session));
        self.require_external_mcp_support(&session, &plan.prepared.stored_config)
            .await?;
        self.assert_accepting_agent_registrations()?;
        if plan.rehydrate_from_disk {
            // Wipe the in-memory timeline so registerSession mints a new
            // epoch and the provider history is read again.
            let mut state = self.lock();
            state.timeline.delete(agent_id);
            for event in state.provider_subagents.delete_parent(agent_id) {
                self.dispatch(&state, AgentManagerEvent::ProviderSubagent(event));
            }
        }
        progress.handed_to_registration = true;
        let preserved = &plan.existing;
        self.register_session(
            session,
            plan.prepared.stored_config.clone(),
            agent_id,
            RegisterOptions {
                labels: Some(closed.labels),
                workspace_id: closed.workspace_id,
                owner: closed.owner,
                created_at_millis: Some(closed.created_at_millis),
                updated_at_millis: Some(closed.updated_at_millis),
                last_user_message_at_millis: closed.last_user_message_at_millis,
                history_primed: Some(!plan.rehydrate_from_disk && preserved.history_primed),
                last_usage: preserved.last_usage.clone(),
                last_error: preserved.last_error.clone(),
                attention: Some(preserved.attention.clone()),
                restoring: true,
                ..RegisterOptions::default()
            },
        )
        .await
    }

    /// `closeReloadedSession(session, agentId)`: one shared close per
    /// session, kept across a timeout so a retry waits for the same
    /// release.
    async fn close_reloaded_session(
        &self,
        session: &Arc<dyn AgentSession>,
        agent_id: &str,
    ) -> Result<(), AgentError> {
        let mut result = self.reloaded_session_close(session);
        let timeout = Duration::from_millis(self.inner.reload_session_close_ms);
        let waited = tokio::time::timeout(timeout, result.wait_for(Option::is_some))
            .await
            .map(|inner| inner.map(|done| done.clone()));
        match waited {
            Ok(Ok(done)) => done.unwrap_or(Ok(())),
            Ok(Err(_)) => Ok(()),
            Err(_) => {
                // A failure after the timeout is only logged.
                let manager = self.clone();
                let agent_id = agent_id.to_owned();
                tokio::spawn(async move {
                    let late = match result.wait_for(Option::is_some).await {
                        Ok(done) => done.clone(),
                        Err(_) => None,
                    };
                    if let (Some(Err(_)), Some(warn)) = (late, &manager.inner.log_warn) {
                        // pino prints the `err` binding, an `Error`, as `{}`.
                        let mut bindings = JsObject::new();
                        bindings.insert("err", JsValue::Object(JsObject::new()));
                        bindings.insert("agentId", JsValue::String(agent_id));
                        warn(
                            JsValue::Object(bindings),
                            "Previous session close failed after refresh timeout",
                        );
                    }
                });
                Err(AgentError::new(
                    "Timed out closing previous session during refresh",
                ))
            }
        }
    }

    /// The close of `session`, started once: a failed close is forgotten so
    /// a retry closes again.
    fn reloaded_session_close(
        &self,
        session: &Arc<dyn AgentSession>,
    ) -> tokio::sync::watch::Receiver<Option<Result<(), AgentError>>> {
        let mut state = self.lock();
        state
            .reloaded_session_closes
            .retain(|close| close.session.strong_count() > 0);
        if let Some(close) = state.reloaded_session_closes.iter().find(|close| {
            close
                .session
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, session))
        }) {
            return close.result.clone();
        }
        let (tx, rx) = tokio::sync::watch::channel(None);
        state.reloaded_session_closes.push(ReloadedClose {
            session: Arc::downgrade(session),
            result: rx.clone(),
        });
        drop(state);
        let manager = self.clone();
        let session = Arc::clone(session);
        tokio::spawn(async move {
            let result = session.close().await;
            let failed = result.is_err();
            tx.send_replace(Some(result));
            if failed {
                manager.lock().reloaded_session_closes.retain(|close| {
                    close
                        .session
                        .upgrade()
                        .is_none_or(|known| !Arc::ptr_eq(&known, &session))
                });
            }
        });
        rx
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
        self.normalize_config_with(config, purpose, true).await
    }

    /// `normalizeConfig(config, { purpose, resolveDefaultModel })`.
    pub(super) async fn normalize_config_with(
        &self,
        config: &JsValue,
        purpose: AgentResumePurpose,
        resolve_default_model: bool,
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
        if resolve_default_model && !truthy(normalized.get("model")) {
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

    /// `listProviderAvailability()`: `(provider, available, error)` per client,
    /// each checked concurrently like `Promise.all`.
    pub async fn list_provider_availability(&self) -> Vec<(String, bool, Option<String>)> {
        let providers: Vec<String> = self
            .lock()
            .clients
            .iter()
            .map(|(provider, _)| provider.clone())
            .collect();
        let checks: Vec<_> = providers
            .into_iter()
            .map(|provider| {
                let manager = self.clone();
                tokio::spawn(async move { manager.get_provider_availability(&provider).await })
            })
            .collect();
        let mut availability = Vec::with_capacity(checks.len());
        for check in checks {
            match check.await {
                Ok(entry) => availability.push(entry),
                Err(error) => std::panic::resume_unwind(error.into_panic()),
            }
        }
        availability
    }

    /// `getProviderAvailability(provider)`: `(provider, available, error)`.
    pub async fn get_provider_availability(
        &self,
        provider: &str,
    ) -> (String, bool, Option<String>) {
        let Some(client) = self.lock().client(provider) else {
            return (
                provider.to_owned(),
                false,
                Some(format!("No client registered for provider '{provider}'")),
            );
        };
        match client.is_available(None, None).await {
            Ok(available) => (provider.to_owned(), available, None),
            Err(error) => {
                // pino prints the `err` binding, an `Error`, as `{}`.
                let mut bindings = JsObject::new();
                bindings.insert("err", JsValue::Object(JsObject::new()));
                bindings.insert("provider", JsValue::String(provider.to_owned()));
                self.emit_warn(
                    JsValue::Object(bindings),
                    "Failed to check provider availability",
                );
                (provider.to_owned(), false, Some(error.message))
            }
        }
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
    async fn close_unregistered_session(&self, session: &Arc<dyn AgentSession>) {
        if session.close().await.is_err() {
            // pino prints the `err` binding, an `Error`, as `{}`.
            let mut bindings = JsObject::new();
            bindings.insert("err", JsValue::Object(JsObject::new()));
            self.emit_warn(
                JsValue::Object(bindings),
                "Failed to close unregistered agent session",
            );
        }
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
        self.close_unregistered_session(session).await;
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
        options: &RegisterOptions,
    ) -> Result<bool, AgentError> {
        let mut state = self.lock();
        let already_primed = state.timeline.has(agent_id);
        // `buildExplicitTimelineSeedForRegister`: given rows or a next
        // sequence seed the timeline even over a primed one.
        let explicit = !options.timeline_rows.is_empty() || options.timeline_next_seq.is_some();
        if explicit || !already_primed {
            let timestamp = crate::clock::iso_from_millis(
                options
                    .updated_at_millis
                    .or(options.created_at_millis)
                    .unwrap_or(now),
            );
            let seed = if explicit {
                TimelineSeed {
                    items: Vec::new(),
                    rows: options
                        .timeline_rows
                        .iter()
                        .cloned()
                        .map(SeedRow::Source)
                        .collect(),
                    epoch: None,
                    next_seq: options.timeline_next_seq,
                    timestamp: Some(timestamp),
                }
            } else {
                TimelineSeed {
                    items: Vec::new(),
                    rows: Vec::new(),
                    epoch: None,
                    next_seq: None,
                    timestamp: Some(crate::clock::iso_from_millis(now)),
                }
            };
            state
                .timeline
                .initialize_with(agent_id, seed)
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
            self.close_unregistered_session(&session).await;
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
            .resolve_initial_persisted_title(
                &resolved_agent_id,
                &config,
                options.initial_title.clone(),
            )
            .await;
        let now = crate::clock::now_millis();
        let durable_timeline_has_rows =
            self.initialize_agent_timeline_for_register(&resolved_agent_id, now, &options)?;
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
                last_usage: options.last_usage,
                last_error: options.last_error,
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
        if !options.publish_when_ready {
            self.emit_state(&resolved_agent_id, false);
        }
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
///
/// A failing request reports zod's issue list from the contracts schema:
/// the request is checked as a session `create_agent_request` holding only
/// the `config` and `env` (plus its required `type` and `requestId`), whose
/// discriminated option reports the same issues as the picked schema, under
/// the session envelope's `message` key, which each issue path drops.
fn before_agent_create(config: &JsValue, env: Option<&JsObject>) -> Result<JsValue, AgentError> {
    let parse_error = |message: String| AgentError {
        name: "ZodError".to_owned(),
        message,
    };
    let mut request = JsObject::new();
    request.insert("type", JsValue::String("create_agent_request".to_owned()));
    request.insert("config", config.clone());
    if let Some(env) = env {
        request.insert("env", JsValue::Object(env.clone()));
    }
    request.insert("requestId", JsValue::String("agent.create".to_owned()));
    let mut envelope = JsObject::new();
    envelope.insert("type", JsValue::String("session".to_owned()));
    envelope.insert("message", JsValue::Object(request));
    match spocky_contracts::zod_schemas::check_inbound(&JsValue::Object(envelope)) {
        Outcome::Invalid(issues) => return Err(parse_error(without_envelope_path(&issues))),
        Outcome::TooDeep => {
            return Err(AgentError {
                name: "RangeError".to_owned(),
                message: "Maximum call stack size exceeded".to_owned(),
            });
        }
        Outcome::Valid | Outcome::Unmodeled => {}
    }
    let parsed =
        <spocky_contracts::agent_config::AgentSessionConfig as serde::Deserialize>::deserialize(
            spocky_contracts::json::JsValueDeserializer(config),
        )
        .map_err(|error| parse_error(error.to_string()))?;
    let text = serde_json::to_string(&parsed).map_err(|error| parse_error(error.to_string()))?;
    spocky_store::js_value::parse(&text).map_err(|error| parse_error(error.to_string()))
}

/// Drops the session envelope's leading `message` from each issue path; a
/// nested union's issues keep their own relative paths.
fn without_envelope_path(issues: &str) -> String {
    let Ok(parsed) = spocky_store::js_value::parse(issues) else {
        return issues.to_owned();
    };
    let Some(list) = parsed.as_array() else {
        return issues.to_owned();
    };
    let issues = list
        .iter()
        .map(|issue| {
            let (JsValue::Object(object), Some(path)) =
                (issue, issue.get("path").and_then(JsValue::as_array))
            else {
                return issue.clone();
            };
            let skip = usize::from(path.first().and_then(JsValue::as_str) == Some("message"));
            let mut object = object.clone();
            object.insert("path", JsValue::Array(path[skip..].to_vec()));
            JsValue::Object(object)
        })
        .collect();
    spocky_store::js_value::stringify_pretty(&JsValue::Array(issues))
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
    last_usage: Option<JsValue>,
    last_error: Option<String>,
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
        last_usage: fields.last_usage,
        last_error: fields.last_error,
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

#[cfg(test)]
mod tests {
    use super::assert_usable_working_directory;

    /// A working directory that is JavaScript text is statted as UTF-8: a
    /// lone surrogate is U+FFFD, as for node's `fs.stat`.
    #[test]
    fn working_directory_check_encodes_a_lone_surrogate_as_node_does() {
        let root = std::env::temp_dir().join(format!("spocky-cwd-js-{}", std::process::id()));
        std::fs::create_dir_all(root.join("\u{FFFD}d")).expect("create dir");
        let lone = spocky_store::js_value::js_text_from_utf16(&[0xD800]);
        let given = format!("{}/{lone}d", root.to_string_lossy());
        assert!(assert_usable_working_directory(&given).is_ok());
        let missing = format!("{}/{lone}missing", root.to_string_lossy());
        assert_eq!(
            assert_usable_working_directory(&missing)
                .expect_err("missing")
                .message,
            format!("Working directory does not exist: {missing}")
        );
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
