//! The `spocky-daemon` session backend: one [`DaemonSession`] per client
//! session (`new Session(...)` in `websocket-server.ts`), routing each inbound
//! request through [`handle_request`] to its `session.ts` handler.
//!
//! Replies go to the socket that sent the request. `SessionDelivery`'s
//! owned-subscription rules (which frames a modern socket may receive) are
//! the session lane's port and are not applied here yet.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use spocky_contracts::frame::{FrameError, parse_frame};
use spocky_contracts::js::truthy;
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::json::{JsonValue, js_wire_text};
use spocky_contracts::number::PositiveInt;
use spocky_contracts::request::{
    FetchAgentRequest, FetchAgentTimelineRequest, FetchAgentsRequest, TimelineDirection,
    WaitForFinishRequest,
};
use spocky_contracts::session::SessionInbound;
use spocky_contracts::text::JsText;
use spocky_contracts::ws::DaemonPermission;
use spocky_daemon::session_api::{
    ProtocolFailure, SessionBackend, SessionHandle, SessionOpen, SessionSink, SocketId,
};
use spocky_message_receipts::MessageReceipts;
use spocky_session::agent_identity::{StoredAgentRef, resolve_agent_identifier};
use spocky_session::agent_loading::{EnsureAgentLoadedDeps, ensure_agent_loaded as load_agent};
use spocky_session::agent_manager::{
    AgentLifecycle, AgentManager, AgentManagerEvent, ManagedAgentSnapshot, SubscribeOptions,
    WaitForAgentOptions, WaitForAgentResult,
};
use spocky_session::agent_projection::{build_stored_agent_payload, to_agent_payload};
use spocky_session::agent_sdk::{AbortController, AbortReason, AbortSignal, AgentError};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::clock::random_uuid;
use spocky_session::creation::CreationService;
use spocky_session::persistence_hooks::is_stored_agent_provider_available;
use spocky_session::provider_snapshot_manager::ProviderSnapshotManager;
use spocky_session::provisioning::WorkspaceProvisioning;
use spocky_session::timeline::{FetchDirection, TimelineCursor};
use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, resolve_project_display_name,
    resolve_workspace_display_name,
};
use spocky_store::time::parse_iso_millis;

use crate::agent_control::{agent_permission_response, cancel_agent};
use crate::agent_create::agent_create;
use crate::agent_directory::{
    AGENTS, CursorError, agent_sort, checkout_from_persisted_workspace_placement, compare,
    compare_with_cursor, decode_cursor, encode_cursor, matches_agent_updates_filter,
};
use crate::agent_message::send_agent_message;
use crate::agent_updates::{AgentUpdates, WaitGuard};
use crate::authorization::SessionAuthorization;
use crate::events::EventDelivery;
use crate::inline_task::start_inline;
use crate::request::{Emit, handle_request, now_millis, pong, request_type};
use crate::workspace_handlers::{fetch_workspaces, workspace_create};

/// `LEGACY_PROVIDER_IDS`: providers every client may see.
const LEGACY_PROVIDER_IDS: [&str; 3] = ["claude", "codex", "opencode"];

/// What every session of the daemon shares: the agent manager, agent
/// records, and the project and workspace registries.
pub struct Services {
    pub runtime: tokio::runtime::Handle,
    pub manager: Arc<AgentManager>,
    pub storage: Arc<AgentStorage>,
    pub provisioning: Arc<WorkspaceProvisioning>,
    pub creation: CreationService,
    /// `providerSnapshotManager`.
    pub snapshots: ProviderSnapshotManager,
    /// `messageReceipts` (`<paseoHome>/agent-requests`).
    pub receipts: MessageReceipts,
    pub paseo_home: PathBuf,
    /// `os.homedir()`, for tilde expansion.
    pub home: String,
}

/// The production [`SessionBackend`].
pub struct DaemonBackend {
    services: Arc<Services>,
}

impl DaemonBackend {
    #[must_use]
    pub fn new(services: Arc<Services>) -> Self {
        Self { services }
    }
}

/// A session message as wire text, so string contents keep their encoding.
fn inbound(message: &Value) -> Result<SessionInbound, FrameError> {
    parse_frame(&js_wire_text(&message.to_string()))
}

impl SessionBackend for DaemonBackend {
    fn open(&self, open: SessionOpen) -> Arc<dyn SessionHandle> {
        let authorization = Arc::new(SessionAuthorization::new(&open.permissions));
        let capabilities = Arc::new(Mutex::new(open.client_capabilities));
        let app_version = Arc::new(Mutex::new(open.app_version));
        let visible_capabilities = Arc::clone(&capabilities);
        let visible_app_version = Arc::clone(&app_version);
        let updates = Arc::new(AgentUpdates::new(
            &self.services,
            Arc::clone(&open.sink),
            Arc::clone(&authorization),
            Arc::new(move |provider: &str| {
                provider_visible(
                    locked(&visible_capabilities).as_ref(),
                    locked(&visible_app_version).as_deref(),
                    provider,
                )
            }),
        ));
        // ponytail: subscribed for the session's lifetime, where the baseline
        // subscribes while a producer has demand (`refreshObservationProducers`);
        // without a subscription `forwardLiveAgent` has no observer either way.
        let events = Arc::new(EventDelivery::new(
            Arc::clone(&open.sink),
            Arc::clone(&authorization),
            Arc::clone(&capabilities),
        ));
        let forward = Arc::downgrade(&updates);
        let fan_out = Arc::downgrade(&events);
        let unsubscribe = self
            .services
            .manager
            .subscribe(
                Arc::new(move |event: &AgentManagerEvent| match event {
                    AgentManagerEvent::AgentState(agent) => {
                        if let Some(updates) = forward.upgrade() {
                            updates.forward_live_agent(agent);
                        }
                    }
                    AgentManagerEvent::AgentStream {
                        agent_id, event, ..
                    } => {
                        if let Some(events) = fan_out.upgrade() {
                            emit_permission_event(&events, agent_id, event);
                        }
                    }
                    _ => {}
                }),
                SubscribeOptions {
                    agent_id: None,
                    replay_state: Some(false),
                },
            )
            .map(|unsubscribe| Box::new(unsubscribe) as Box<dyn FnOnce() + Send>)
            .ok();
        Arc::new(DaemonSession {
            id: random_uuid(),
            authorization,
            permissions: open.permissions,
            capabilities,
            app_version,
            events,
            updates,
            unsubscribe_agent_events: Mutex::new(unsubscribe),
            sink: open.sink,
            services: Arc::clone(&self.services),
        })
    }

    fn validate_inbound(&self, message: &Value) -> Result<(), String> {
        inbound(message)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Runs on the thread that stops the daemon, outside the async runtime,
    /// so it blocks until every agent is closed and storage is flushed.
    fn stop_agents(&self) {
        self.services
            .runtime
            .block_on(crate::shutdown::stop_agents(&self.services));
    }
}

/// One client session.
pub struct DaemonSession {
    id: String,
    authorization: Arc<SessionAuthorization>,
    permissions: Vec<DaemonPermission>,
    capabilities: Arc<Mutex<Option<Value>>>,
    app_version: Arc<Mutex<Option<String>>>,
    events: Arc<EventDelivery>,
    updates: Arc<AgentUpdates>,
    unsubscribe_agent_events: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    sink: Arc<dyn SessionSink>,
    services: Arc<Services>,
}

fn locked<T: Clone>(value: &Mutex<T>) -> T {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn set<T>(slot: &Mutex<T>, value: T) {
    *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}

impl SessionHandle for DaemonSession {
    fn session_id(&self) -> String {
        self.id.clone()
    }

    fn permissions(&self) -> Vec<DaemonPermission> {
        self.permissions.clone()
    }

    fn update_client_capabilities(
        &self,
        capabilities: Option<&Value>,
        source: SocketId,
        app_version: Option<&str>,
    ) {
        self.events.attach(source, capabilities);
        set(&self.capabilities, capabilities.cloned());
        if let Some(app_version) = app_version {
            set(&self.app_version, Some(app_version.to_owned()));
        }
    }

    fn update_app_version(&self, app_version: &str) {
        set(&self.app_version, Some(app_version.to_owned()));
    }

    fn handle_message(&self, message: Value, source: SocketId) {
        let Ok(message) = inbound(&message) else {
            return;
        };
        let sink = Arc::clone(&self.sink);
        let emit: Emit = Arc::new(move |frame| sink.send_to_source(source, &frame));
        let context = Arc::new(RequestContext {
            services: Arc::clone(&self.services),
            capabilities: locked(&self.capabilities),
            app_version: locked(&self.app_version),
            source,
            modern: self.events.is_modern(source),
            request_signal: self.events.request_signal(source),
            updates: Arc::clone(&self.updates),
            events: Arc::clone(&self.events),
        });
        let session_events = Arc::clone(&self.events);
        // Pinned starts each message without queueing it, so a handler's
        // synchronous prefix runs in arrival order.
        start_inline(
            &self.services.runtime,
            handle_request(
                Arc::clone(&self.authorization),
                message,
                emit,
                Arc::new(move |frame| {
                    session_events.emit(&frame);
                }),
                move |message, emit| route(context, message, emit),
            ),
        );
    }

    fn protocol_failure(&self, source: SocketId, failure: ProtocolFailure) {
        let frame = match failure.request_id {
            Some(request_id) => serde_json::json!({
                "type": "rpc_error",
                "payload": {
                    "requestId": request_id,
                    "requestType": failure.request_type,
                    "error": failure.error,
                    "code": failure.code,
                }
            }),
            None => serde_json::json!({
                "type": "status",
                "payload": { "status": "error", "message": failure.error }
            }),
        };
        self.sink.send_to_source(source, &frame);
    }

    fn socket_detached(&self, source: SocketId) {
        self.updates.detach(source);
        self.events.detach(source);
    }

    fn cleanup(&self) {
        let unsubscribe = self
            .unsubscribe_agent_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.updates.dispose();
    }
}

/// What one request's handler can reach: the shared services and the
/// session's client capabilities and app version when the request arrived.
pub(crate) struct RequestContext {
    pub(crate) services: Arc<Services>,
    capabilities: Option<Value>,
    app_version: Option<String>,
    pub(crate) source: SocketId,
    pub(crate) modern: bool,
    pub(crate) updates: Arc<AgentUpdates>,
    pub(crate) events: Arc<EventDelivery>,
    /// `delivery.requestSignal`: aborts when the requesting socket detaches.
    pub(crate) request_signal: AbortSignal,
}

/// `MIN_VERSION_ALL_PROVIDERS`.
const MIN_VERSION_ALL_PROVIDERS: [u64; 3] = [0, 1, 45];

/// `isAppVersionAtLeast(appVersion, MIN_VERSION_ALL_PROVIDERS)`: the prerelease
/// suffix is dropped and missing parts read as 0. A part that is not a number
/// is `NaN` in the baseline, which is neither greater nor less, so the
/// comparison moves on.
fn app_version_at_least(app_version: Option<&str>, min: [u64; 3]) -> bool {
    let Some(app_version) = app_version.filter(|version| !version.is_empty()) else {
        return false;
    };
    let base = app_version.split('-').next().unwrap_or_default();
    let parts: Vec<Option<u64>> = base.split('.').map(|part| part.parse().ok()).collect();
    for (index, minimum) in min.iter().enumerate() {
        match parts.get(index).copied().unwrap_or(Some(0)) {
            Some(part) if part > *minimum => return true,
            Some(part) if part < *minimum => return false,
            _ => {}
        }
    }
    true
}

impl RequestContext {
    /// `supports(capability)`: `capabilities[capability] === true`.
    fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .as_ref()
            .and_then(|caps| caps.get(capability))
            == Some(&Value::Bool(true))
    }

    /// `isProviderVisibleToClient`.
    fn provider_visible(&self, provider: &str) -> bool {
        provider_visible(
            self.capabilities.as_ref(),
            self.app_version.as_deref(),
            provider,
        )
    }
}

/// `isProviderVisibleToClient` for a client's capabilities and app version.
fn provider_visible(
    capabilities: Option<&Value>,
    app_version: Option<&str>,
    provider: &str,
) -> bool {
    capabilities.and_then(|caps| caps.get("all_providers")) == Some(&Value::Bool(true))
        || app_version_at_least(app_version, MIN_VERSION_ALL_PROVIDERS)
        || LEGACY_PROVIDER_IDS.contains(&provider)
}

/// `dispatchInboundMessage` for the slice's requests.
async fn route(
    context: Arc<RequestContext>,
    message: SessionInbound,
    emit: Emit,
) -> Result<(), JsText> {
    match message {
        SessionInbound::Ping(ping) => {
            emit(pong(ping, now_millis()));
            Ok(())
        }
        SessionInbound::FetchAgents(request) => fetch_agents(&context, request, &emit).await,
        SessionInbound::FetchAgent(request) => fetch_agent(&context, request, &emit).await,
        SessionInbound::SendAgentMessage(request) => {
            send_agent_message(&context, request, &emit).await;
            Ok(())
        }
        SessionInbound::CancelAgent(request) => {
            cancel_agent(&context, request, &emit).await;
            Ok(())
        }
        SessionInbound::AgentPermissionResponse(request) => {
            agent_permission_response(&context, request, &emit).await
        }
        SessionInbound::AgentCreate(request) => {
            agent_create(&context.services, &context.updates, *request, &emit).await;
            Ok(())
        }
        SessionInbound::WorkspaceCreate(request) => {
            workspace_create(&context, *request, &emit).await;
            Ok(())
        }
        SessionInbound::FetchWorkspaces(request) => {
            fetch_workspaces(&context, request, &emit).await;
            Ok(())
        }
        SessionInbound::WaitForFinish(request) => wait_for_finish(&context, request, &emit).await,
        SessionInbound::FetchAgentTimeline(request) => {
            fetch_agent_timeline(&context, request, &emit).await;
            Ok(())
        }
        other => Err(JsText::new(&format!(
            "{} is not ported in spocky-daemon-app yet",
            request_type(&other)
        ))),
    }
}

/// The permission frames `subscribeToAgentEvents` emits for a provider
/// stream event: `agent_permission_request` through `emit` and
/// `agent_permission_resolved` through `emitSubscribedEvent`, both session
/// events here (neither is a reply to a request in flight).
fn emit_permission_event(events: &EventDelivery, agent_id: &str, event: &JsValue) {
    let mut payload = JsObject::new();
    payload.insert("agentId", JsValue::String(agent_id.to_owned()));
    let kind = match event.get("type").and_then(JsValue::as_str) {
        Some("permission_requested") => {
            payload.insert(
                "request",
                event.get("request").cloned().unwrap_or(JsValue::Undefined),
            );
            "agent_permission_request"
        }
        Some("permission_resolved") => {
            payload.insert(
                "requestId",
                event
                    .get("requestId")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            payload.insert(
                "resolution",
                event
                    .get("resolution")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
            "agent_permission_resolved"
        }
        _ => return,
    };
    events.emit(&frame(kind, payload));
}

pub(crate) fn js_text(value: &JsText) -> JsValue {
    JsValue::String(value.as_str().to_owned())
}

pub(crate) fn to_frame(value: JsValue) -> Value {
    serde_json::to_value(JsonValue(value)).expect("payload objects serialize to JSON")
}

/// `buildAgentPayload`: `toAgentPayload(agent)` with the stored title and
/// archive time (`enrichAgentPayload`).
pub(crate) async fn agent_payload(
    services: &Services,
    agent: &ManagedAgentSnapshot,
) -> Result<JsValue, String> {
    let mut payload =
        to_agent_payload(&agent.payload_view(), None).map_err(|error| error.to_string())?;
    let stored = services.storage.get(&agent.id).await;
    if let JsValue::Object(object) = &mut payload {
        let field = |key: &str| {
            stored
                .as_ref()
                .and_then(|record| record.get(key))
                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                .cloned()
                .unwrap_or(JsValue::Null)
        };
        object.insert("title", field("title"));
        object.insert("archivedAt", field("archivedAt"));
    }
    Ok(payload)
}

async fn live_agent_payloads(context: &RequestContext) -> Result<Vec<JsValue>, String> {
    let mut payloads = Vec::new();
    for agent in context.services.manager.list_agents() {
        payloads.push(agent_payload(&context.services, &agent).await?);
    }
    Ok(payloads)
}

/// `Boolean(record[key])`: JavaScript truthiness, so `internal: false` and
/// `archivedAt: null` are falsy.
fn truthy_text(record: &JsValue, key: &str) -> bool {
    truthy(record.get(key))
}

/// `listAgentPayloads` for `fetch_agents_request`.
async fn list_agent_payloads(
    context: &RequestContext,
    request: &FetchAgentsRequest,
) -> Result<Vec<JsValue>, String> {
    let filter = request.filter.as_ref();
    let include_archived = filter.and_then(|f| f.include_archived) == Some(true);
    let labels: Vec<(String, String)> = filter
        .and_then(|f| f.labels.as_ref())
        .map(|labels| {
            labels
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let label_matches = |record: &JsValue| {
        labels.iter().all(|(key, value)| {
            record
                .get("labels")
                .and_then(|labels| labels.get(key))
                .and_then(JsValue::as_str)
                == Some(value.as_str())
        })
    };
    let live = live_agent_payloads(context).await?;
    let live_ids: Vec<String> = context
        .services
        .manager
        .list_agents()
        .into_iter()
        .map(|agent| agent.id.clone())
        .collect();
    let registered = context.services.manager.registered_provider_ids();
    let persisted = context
        .services
        .storage
        .list()
        .await
        .into_iter()
        .filter(|record| {
            let id = record.get("id").and_then(JsValue::as_str).unwrap_or("");
            !live_ids.iter().any(|live| live == id) && !truthy_text(record, "internal")
        })
        .filter(|record| include_archived || !truthy_text(record, "archivedAt"))
        .filter(|record| label_matches(record))
        .filter(|record| is_stored_agent_provider_available(record, Some(&registered)))
        .map(|record| build_stored_agent_payload(&record, &registered))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.message)?;
    Ok(live
        .into_iter()
        .chain(persisted)
        .filter(|agent| {
            context.provider_visible(
                agent
                    .get("provider")
                    .and_then(JsValue::as_str)
                    .unwrap_or(""),
            )
        })
        .filter(|agent| include_archived || !truthy_text(agent, "archivedAt"))
        .filter(|agent| label_matches(agent))
        .collect())
}

/// `buildProjectPlacementForWorkspace`. Without a workspace git service the
/// legacy branch and worktree-root fallbacks are absent; records written by
/// this daemon always carry both.
fn project_placement(
    workspace: &PersistedWorkspaceRecord,
    project: &PersistedProjectRecord,
) -> JsValue {
    let mut placement = JsObject::new();
    placement.insert("projectKey", JsValue::String(project.project_id.clone()));
    placement.insert(
        "projectName",
        JsValue::String(resolve_project_display_name(project).to_owned()),
    );
    placement.insert(
        "workspaceName",
        JsValue::String(resolve_workspace_display_name(workspace).to_owned()),
    );
    placement.insert(
        "checkout",
        checkout_from_persisted_workspace_placement(workspace, None, None),
    );
    JsValue::Object(placement)
}

/// Placements by workspace id: active ones only for `scope: "active"`
/// (`buildActiveProjectPlacementsByWorkspaceId`), else every resolvable one
/// (`buildProjectPlacementForWorkspaceId`).
async fn placements(context: &RequestContext, active_only: bool) -> Vec<(String, JsValue)> {
    let workspaces = context.services.provisioning.workspaces.lock().await.list();
    let projects = context.services.provisioning.projects.lock().await.list();
    workspaces
        .iter()
        .filter(|workspace| !active_only || workspace.archived_at.is_none())
        .filter_map(|workspace| {
            let project = projects.iter().find(|project| {
                project.project_id == workspace.project_id
                    && (!active_only || project.archived_at.is_none())
            })?;
            Some((
                workspace.workspace_id.clone(),
                project_placement(workspace, project),
            ))
        })
        .collect()
}

/// `listFetchAgentsEntries` for `fetch_agents_request` (no history search).
async fn list_fetch_agents_entries(
    context: &RequestContext,
    request: &FetchAgentsRequest,
) -> Result<JsObject, (String, &'static str)> {
    let failed = |message: String| (message, "fetch_agents_failed");
    let sort = AGENTS.normalize_sort(&agent_sort(request.sort.as_deref()));
    let mut agents = list_agent_payloads(context, request)
        .await
        .map_err(failed)?;
    let active = request.scope.is_some();
    let placements = placements(context, active).await;
    let placement = |agent: &JsValue| {
        let workspace_id = agent.get("workspaceId").and_then(JsValue::as_str)?;
        placements
            .iter()
            .find(|(id, _)| id == workspace_id)
            .map(|(_, placement)| placement.clone())
    };
    if active {
        agents.retain(|agent| !truthy_text(agent, "archivedAt") && placement(agent).is_some());
    }
    agents.sort_by(|left, right| compare(&AGENTS, left, right, &sort));
    let cursor_token = request.page.as_ref().and_then(|page| page.cursor.as_ref());
    if let Some(token) = cursor_token {
        let cursor = decode_cursor(&AGENTS, token.as_str(), &sort)
            .map_err(|CursorError(message)| (message, "invalid_cursor"))?;
        agents.retain(|agent| compare_with_cursor(&AGENTS, agent, &cursor, &sort) > 0);
    }
    let limit = request
        .page
        .as_ref()
        .map_or(200, |page| usize::try_from(page.limit.get()).unwrap_or(200));
    let mut matched: Vec<(JsValue, JsValue)> = Vec::new();
    for agent in agents {
        if matched.len() > limit {
            break;
        }
        let Some(project) = placement(&agent) else {
            continue;
        };
        if matches_agent_updates_filter(&agent, &project, request.filter.as_ref()) {
            matched.push((agent, project));
        }
    }
    let has_more = matched.len() > limit;
    matched.truncate(limit);
    let next_cursor = match matched.last() {
        Some((agent, _)) if has_more => JsValue::String(encode_cursor(&AGENTS, agent, &sort)),
        _ => JsValue::Null,
    };
    let entries = matched
        .into_iter()
        .map(|(agent, project)| {
            let mut entry = JsObject::new();
            entry.insert("agent", agent);
            entry.insert("project", project);
            JsValue::Object(entry)
        })
        .collect();
    let mut page_info = JsObject::new();
    page_info.insert("nextCursor", next_cursor);
    page_info.insert(
        "prevCursor",
        cursor_token.map_or(JsValue::Null, |token| {
            JsValue::String(token.as_str().to_owned())
        }),
    );
    page_info.insert("hasMore", JsValue::Bool(has_more));
    let mut payload = JsObject::new();
    payload.insert("entries", JsValue::Array(entries));
    payload.insert("pageInfo", JsValue::Object(page_info));
    Ok(payload)
}

/// `handleFetchAgents` without directory sync.
async fn fetch_agents(
    context: &RequestContext,
    request: FetchAgentsRequest,
    emit: &Emit,
) -> Result<(), JsText> {
    if request.sync.is_some() {
        return Err(JsText::new(
            "fetch_agents directory sync is not ported in spocky-daemon-app yet",
        ));
    }
    let owner = match &request.subscribe {
        Some(subscribe) => Some(
            context
                .updates
                .begin(
                    context.source,
                    context.modern,
                    !request.request_id.as_str().is_empty(),
                    subscribe.subscription_id.as_ref().map(JsText::as_str),
                    request.filter.clone(),
                )
                .map_err(|message| JsText::new(&message))?,
        ),
        None => None,
    };
    let outcome = list_fetch_agents_entries(context, &request).await;
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(&request.request_id));
    match outcome {
        Ok(listing) => {
            if let Some(owner) = &owner {
                payload.insert("subscriptionId", JsValue::String(owner.response_id.clone()));
            }
            let mut snapshot_updated_at = HashMap::new();
            for entry in listing
                .get("entries")
                .and_then(JsValue::as_array)
                .into_iter()
                .flatten()
            {
                let agent = entry.get("agent");
                if let (Some(id), Some(updated_at)) = (
                    agent
                        .and_then(|agent| agent.get("id"))
                        .and_then(JsValue::as_str),
                    agent
                        .and_then(|agent| agent.get("updatedAt"))
                        .and_then(JsValue::as_str)
                        .and_then(parse_iso_millis),
                ) {
                    snapshot_updated_at.insert(id.to_owned(), updated_at);
                }
            }
            for (key, value) in listing.iter() {
                payload.insert(key, value.clone());
            }
            let mut frame = JsObject::new();
            frame.insert("type", JsValue::String("fetch_agents_response".to_owned()));
            frame.insert("payload", JsValue::Object(payload));
            emit(to_frame(JsValue::Object(frame)));
            if let Some(owner) = &owner {
                context
                    .updates
                    .flush_bootstrapped(&owner.id, &snapshot_updated_at);
            }
        }
        Err((message, code)) => {
            if let Some(owner) = &owner {
                context.updates.clear(&owner.id);
            }
            let mut error = JsObject::new();
            error.insert("requestId", js_text(&request.request_id));
            error.insert(
                "requestType",
                JsValue::String("fetch_agents_request".to_owned()),
            );
            error.insert("error", JsValue::String(message));
            error.insert("code", JsValue::String(code.to_owned()));
            let mut frame = JsObject::new();
            frame.insert("type", JsValue::String("rpc_error".to_owned()));
            frame.insert("payload", JsValue::Object(error));
            emit(to_frame(JsValue::Object(frame)));
        }
    }
    Ok(())
}

pub(crate) fn frame(kind: &str, payload: JsObject) -> Value {
    let mut frame = JsObject::new();
    frame.insert("type", JsValue::String(kind.to_owned()));
    frame.insert("payload", JsValue::Object(payload));
    to_frame(JsValue::Object(frame))
}

fn text_or_null(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::Null, |text| JsValue::String(text.to_owned()))
}

/// `resolveAgentIdentifier` over the stored records and the live agents.
pub(crate) async fn resolve_agent(
    context: &RequestContext,
    identifier: &str,
) -> Result<String, String> {
    let stored = context.services.storage.list().await;
    let refs: Vec<StoredAgentRef<'_>> = stored
        .iter()
        .map(|record| StoredAgentRef {
            id: record.get("id").and_then(JsValue::as_str).unwrap_or(""),
            title: record.get("title").and_then(JsValue::as_str),
            internal: truthy_text(record, "internal"),
        })
        .collect();
    let live: Vec<String> = context
        .services
        .manager
        .list_agents()
        .into_iter()
        .map(|agent| agent.id.clone())
        .collect();
    let live: Vec<&str> = live.iter().map(String::as_str).collect();
    resolve_agent_identifier(identifier, &refs, &live)
}

/// `getAgentPayloadById`: the live payload, else the stored one, when the
/// client may see its provider.
async fn agent_payload_by_id(
    context: &RequestContext,
    agent_id: &str,
) -> Result<Option<JsValue>, JsText> {
    let visible = |payload: &JsValue| {
        context.provider_visible(
            payload
                .get("provider")
                .and_then(JsValue::as_str)
                .unwrap_or(""),
        )
    };
    if let Some(agent) = context.services.manager.get_agent(agent_id) {
        let payload = agent_payload(&context.services, &agent)
            .await
            .map_err(|error| JsText::new(&error))?;
        return Ok(visible(&payload).then_some(payload));
    }
    match context.services.storage.get(agent_id).await {
        Some(record) if !truthy_text(&record, "internal") => {
            let payload = stored_payload(context, &record).map_err(|error| JsText::new(&error))?;
            Ok(visible(&payload).then_some(payload))
        }
        _ => Ok(None),
    }
}

/// `buildStoredAgentPayload(record)` over the registered providers.
fn stored_payload(context: &RequestContext, record: &JsValue) -> Result<JsValue, String> {
    build_stored_agent_payload(record, &context.services.manager.registered_provider_ids())
        .map_err(|error| error.message)
}

/// `buildProjectPlacementForWorkspaceId`.
pub(crate) async fn placement_for_workspace(services: &Services, workspace_id: &str) -> JsValue {
    let workspace = services
        .provisioning
        .workspaces
        .lock()
        .await
        .get(workspace_id);
    let Some(workspace) = workspace else {
        return JsValue::Null;
    };
    let project = services
        .provisioning
        .projects
        .lock()
        .await
        .get(&workspace.project_id);
    project.map_or(JsValue::Null, |project| {
        project_placement(&workspace, &project)
    })
}

/// `handleFetchAgent`.
async fn fetch_agent(
    context: &RequestContext,
    request: FetchAgentRequest,
    emit: &Emit,
) -> Result<(), JsText> {
    let respond = |agent: JsValue, project: JsValue, error: JsValue| {
        let mut payload = JsObject::new();
        payload.insert("requestId", js_text(&request.request_id));
        payload.insert("agent", agent);
        payload.insert("project", project);
        payload.insert("error", error);
        emit(frame("fetch_agent_response", payload));
    };
    let agent_id = match resolve_agent(context, request.agent_id.as_str()).await {
        Ok(agent_id) => agent_id,
        Err(error) => {
            respond(JsValue::Null, JsValue::Null, JsValue::String(error));
            return Ok(());
        }
    };
    let Some(agent) = agent_payload_by_id(context, &agent_id).await? else {
        respond(
            JsValue::Null,
            JsValue::Null,
            JsValue::String(format!("Agent not found: {agent_id}")),
        );
        return Ok(());
    };
    let project = match agent.get("workspaceId").and_then(JsValue::as_str) {
        Some(workspace_id) if !workspace_id.is_empty() => {
            placement_for_workspace(&context.services, workspace_id).await
        }
        _ => JsValue::Null,
    };
    respond(agent, project, JsValue::Null);
    Ok(())
}

/// `waitForAgentEvent(agentId, { signal, waitForActive: true })`, aborted
/// with reason `"timeout"` after `timeout_ms` when that is positive.
async fn wait_with_timeout(
    context: &RequestContext,
    agent_id: &str,
    timeout_ms: Option<i64>,
) -> Result<WaitForAgentResult, AgentError> {
    // `AbortSignal.any([timeout, sourceSignal])`.
    let controller = AbortController::default();
    let source = {
        let controller = controller.clone();
        let signal = context.request_signal.clone();
        context.services.runtime.spawn(async move {
            signal.wait().await;
            if let Some(reason) = signal.reason() {
                controller.abort(reason.clone());
            }
        })
    };
    let timeout = timeout_ms.filter(|millis| *millis > 0).map(|millis| {
        let controller = controller.clone();
        context.services.runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                u64::try_from(millis).unwrap_or(u64::MAX),
            ))
            .await;
            controller.abort(AbortReason::Value(JsValue::String("timeout".to_owned())));
        })
    });
    let result = context
        .services
        .manager
        .wait_for_agent_event(
            agent_id,
            WaitForAgentOptions {
                signal: Some(controller.signal()),
                wait_for_active: true,
            },
        )
        .await;
    if let Some(timeout) = timeout {
        timeout.abort();
    }
    source.abort();
    result
}

/// A thrown abort's message: the reason's own, as `throwIfAborted` throws it.
fn abort_message(signal: &AbortSignal) -> String {
    match signal.reason() {
        Some(AbortReason::Error(error)) => error.message.clone(),
        Some(AbortReason::Value(JsValue::String(text))) => text.clone(),
        _ => "This operation was aborted".to_owned(),
    }
}

/// `resolveWaitForFinishError`.
fn wait_for_finish_error(status: &str, final_agent: Option<&JsValue>) -> JsValue {
    if status != "error" {
        return JsValue::Null;
    }
    match final_agent
        .and_then(|agent| agent.get("lastError"))
        .and_then(JsValue::as_str)
    {
        Some(message) if !spocky_contracts::text::js_trim(message).is_empty() => {
            JsValue::String(message.to_owned())
        }
        _ => JsValue::String("Agent failed".to_owned()),
    }
}

/// Registers the wait with the update service: updates its state changes
/// wake are held behind its reply (`handleWaitForFinish` replies within
/// microtasks of the change that settles it).
fn begin_reply_wait(context: &RequestContext, agent_id: &str) -> WaitGuard {
    let manager = &context.services.manager;
    let running = manager
        .get_agent(agent_id)
        .is_some_and(|agent| agent.lifecycle == AgentLifecycle::Running)
        || manager.has_in_flight_run(agent_id);
    context.updates.begin_wait(agent_id, running)
}

/// `handleWaitForFinish`. The request's own abort signal (released or
/// disconnected requests) arrives with `SessionDelivery`; until then only the
/// timeout aborts the wait.
async fn wait_for_finish(
    context: &RequestContext,
    request: WaitForFinishRequest,
    emit: &Emit,
) -> Result<(), JsText> {
    let respond = |status: &str, final_agent: JsValue, error: JsValue, last: JsValue| {
        let mut payload = JsObject::new();
        payload.insert("requestId", js_text(&request.request_id));
        payload.insert("status", JsValue::String(status.to_owned()));
        payload.insert("final", final_agent);
        payload.insert("error", error);
        payload.insert("lastMessage", last);
        emit(frame("wait_for_finish_response", payload));
    };
    // `sourceSignal.throwIfAborted()`.
    if context.request_signal.aborted() {
        return Err(JsText::new(&abort_message(&context.request_signal)));
    }
    let agent_id = match resolve_agent(context, request.agent_id.as_str()).await {
        Ok(agent_id) => agent_id,
        Err(error) => {
            respond(
                "error",
                JsValue::Null,
                JsValue::String(error),
                JsValue::Null,
            );
            return Ok(());
        }
    };
    if context.services.manager.get_agent(&agent_id).is_none() {
        let record = context.services.storage.get(&agent_id).await;
        return match record {
            Some(record) if !truthy_text(&record, "internal") => {
                let final_agent =
                    stored_payload(context, &record).map_err(|error| JsText::new(&error))?;
                let status = if record.get("attentionReason").and_then(JsValue::as_str)
                    == Some("permission")
                {
                    "permission"
                } else if record.get("lastStatus").and_then(JsValue::as_str) == Some("error") {
                    "error"
                } else {
                    "idle"
                };
                let error = wait_for_finish_error(status, Some(&final_agent));
                respond(status, final_agent, error, JsValue::Null);
                Ok(())
            }
            _ => {
                let error = format!("Agent not found: {agent_id}");
                respond(
                    "error",
                    JsValue::Null,
                    JsValue::String(error),
                    JsValue::Null,
                );
                Ok(())
            }
        };
    }
    // Updates that would settle this wait go out after its reply.
    let _reply_pending = begin_reply_wait(context, &agent_id);
    let result =
        wait_with_timeout(context, &agent_id, request.timeout_ms.map(PositiveInt::get)).await;
    let disappeared = || JsText::new(&format!("Agent {agent_id} disappeared while waiting"));
    match result {
        Ok(result) => {
            let final_agent = agent_payload_by_id(context, &agent_id)
                .await?
                .ok_or_else(disappeared)?;
            let status = if result.permission.is_some() {
                "permission"
            } else if result.status == AgentLifecycle::Error {
                "error"
            } else {
                "idle"
            };
            let error = wait_for_finish_error(status, Some(&final_agent));
            respond(
                status,
                final_agent,
                error,
                text_or_null(result.last_message.as_deref()),
            );
        }
        // The requesting socket detached: nothing answers.
        Err(_) if context.request_signal.aborted() => {}
        Err(error) => {
            let is_abort =
                error.name == "AbortError" || error.message.to_lowercase().contains("aborted");
            let final_agent = agent_payload_by_id(context, &agent_id).await?;
            if !is_abort {
                respond(
                    "error",
                    final_agent.unwrap_or(JsValue::Null),
                    JsValue::String(error.message),
                    JsValue::Null,
                );
                return Ok(());
            }
            let final_agent = final_agent.ok_or_else(disappeared)?;
            respond("timeout", final_agent, JsValue::Null, JsValue::Null);
        }
    }
    Ok(())
}

/// `ensureAgentLoaded(agentId, { agentManager, agentStorage, logger })`:
/// the live agent, or the stored one resumed from persistence and hydrated.
pub(crate) async fn ensure_agent_loaded(
    context: &RequestContext,
    agent_id: &str,
) -> Result<ManagedAgentSnapshot, String> {
    load_agent(
        agent_id,
        &EnsureAgentLoadedDeps {
            agent_manager: (*context.services.manager).clone(),
            agent_storage: (*context.services.storage).clone(),
            valid_providers: None,
            broadcast_timeline: false,
        },
    )
    .await
    .map_err(|error| error.message)
}

fn direction_text(direction: FetchDirection) -> JsValue {
    JsValue::String(direction.as_str().to_owned())
}

fn cursor_value(epoch: &str, seq: Option<i64>) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    seq.map_or(JsValue::Null, |seq| {
        let mut cursor = JsObject::new();
        cursor.insert("epoch", JsValue::String(epoch.to_owned()));
        cursor.insert("seq", JsValue::Number(seq as f64));
        JsValue::Object(cursor)
    })
}

#[allow(clippy::cast_precision_loss)]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

/// The response `entries`: rows the client can render, each with the agent's
/// provider, and `reasoning_merge` dropped for clients without
/// `reasoning_merge_enum`.
fn timeline_entries(
    context: &RequestContext,
    provider: &str,
    rows: &[spocky_session::timeline::ProjectedRow],
) -> Vec<JsValue> {
    let merge_enum = context.supports("reasoning_merge_enum");
    rows.iter()
        .filter(|row| match row.item.get("type").and_then(JsValue::as_str) {
            Some("notification") => context.supports("timeline_notifications"),
            Some("plugin") => context.supports("plugin_timeline_items"),
            _ => true,
        })
        .map(|row| {
            let mut entry = JsObject::new();
            entry.insert("provider", JsValue::String(provider.to_owned()));
            entry.insert("item", row.item.clone());
            entry.insert("timestamp", JsValue::String(row.timestamp.clone()));
            entry.insert("seqStart", number(row.seq_start));
            entry.insert("seqEnd", number(row.seq_end));
            let ranges = row
                .source_seq_ranges
                .iter()
                .map(|range| {
                    let mut value = JsObject::new();
                    value.insert("startSeq", number(range.start_seq));
                    value.insert("endSeq", number(range.end_seq));
                    JsValue::Object(value)
                })
                .collect();
            entry.insert("sourceSeqRanges", JsValue::Array(ranges));
            // `turnId: undefined` keeps its slot and is assigned after.
            entry.insert(
                "turnId",
                row.turn_id
                    .as_ref()
                    .map_or(JsValue::Undefined, |turn| JsValue::String(turn.clone())),
            );
            let collapsed = row
                .collapsed
                .iter()
                .map(|kind| kind.as_str())
                .filter(|kind| merge_enum || *kind != "reasoning_merge")
                .map(|kind| JsValue::String(kind.to_owned()))
                .collect();
            entry.insert("collapsed", JsValue::Array(collapsed));
            JsValue::Object(entry)
        })
        .collect()
}

/// `handleFetchAgentTimelineRequest`.
async fn fetch_agent_timeline(
    context: &RequestContext,
    request: FetchAgentTimelineRequest,
    emit: &Emit,
) {
    let direction = match request.direction {
        Some(TimelineDirection::Tail) => FetchDirection::Tail,
        Some(TimelineDirection::Before) => FetchDirection::Before,
        Some(TimelineDirection::After) => FetchDirection::After,
        None => {
            if request.cursor.is_some() {
                FetchDirection::After
            } else {
                FetchDirection::Tail
            }
        }
    };
    let limit = request.limit.map_or(
        if direction == FetchDirection::After {
            0
        } else {
            200
        },
        |limit| usize::try_from(limit.get()).unwrap_or(usize::MAX),
    );
    let cursor = request.cursor.as_ref().map(|cursor| TimelineCursor {
        epoch: cursor.epoch.as_str().to_owned(),
        seq: cursor.seq.get(),
    });
    let agent_id = request.agent_id.as_str().to_owned();
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(&request.request_id));
    payload.insert("agentId", js_text(&request.agent_id));
    let loaded = async {
        let snapshot = ensure_agent_loaded(context, &agent_id).await?;
        let agent = agent_payload(&context.services, &snapshot).await?;
        let fetched = context
            .services
            .manager
            .fetch_timeline(&agent_id, direction, cursor.as_ref(), Some(limit))
            .map_err(|error| error.message)?;
        Ok::<_, String>((snapshot, agent, fetched))
    }
    .await;
    match loaded {
        Ok((snapshot, agent, fetched)) => {
            payload.insert("agent", agent);
            payload.insert("direction", direction_text(direction));
            payload.insert("projection", JsValue::String("projected".to_owned()));
            payload.insert("epoch", JsValue::String(fetched.epoch.clone()));
            payload.insert("reset", JsValue::Bool(fetched.reset));
            payload.insert("staleCursor", JsValue::Bool(fetched.stale_cursor));
            payload.insert("gap", JsValue::Bool(fetched.gap));
            let mut window = JsObject::new();
            window.insert("minSeq", number(fetched.window.min_seq));
            window.insert("maxSeq", number(fetched.window.max_seq));
            window.insert("nextSeq", number(fetched.window.next_seq));
            payload.insert("window", JsValue::Object(window));
            payload.insert(
                "startCursor",
                cursor_value(&fetched.epoch, fetched.start_seq),
            );
            payload.insert("endCursor", cursor_value(&fetched.epoch, fetched.end_seq));
            payload.insert("hasOlder", JsValue::Bool(fetched.has_older));
            payload.insert("hasNewer", JsValue::Bool(fetched.has_newer));
            if request.merge_window == Some(true) {
                payload.insert("mergeWindow", JsValue::Bool(true));
            }
            let entries = timeline_entries(context, &snapshot.provider, &fetched.rows);
            payload.insert("entries", JsValue::Array(entries));
            payload.insert("error", JsValue::Null);
        }
        Err(error) => {
            payload.insert("agent", JsValue::Null);
            payload.insert("direction", direction_text(direction));
            payload.insert("projection", JsValue::String("projected".to_owned()));
            payload.insert("epoch", JsValue::String(String::new()));
            payload.insert("reset", JsValue::Bool(false));
            payload.insert("staleCursor", JsValue::Bool(false));
            payload.insert("gap", JsValue::Bool(false));
            let mut window = JsObject::new();
            window.insert("minSeq", number(0));
            window.insert("maxSeq", number(0));
            window.insert("nextSeq", number(0));
            payload.insert("window", JsValue::Object(window));
            payload.insert("startCursor", JsValue::Null);
            payload.insert("endCursor", JsValue::Null);
            payload.insert("hasOlder", JsValue::Bool(false));
            payload.insert("hasNewer", JsValue::Bool(false));
            if request.merge_window == Some(true) {
                payload.insert("mergeWindow", JsValue::Bool(true));
            }
            payload.insert("entries", JsValue::Array(Vec::new()));
            payload.insert("error", JsValue::String(error));
        }
    }
    emit(frame("fetch_agent_timeline_response", payload));
}

#[cfg(test)]
mod tests {
    use super::{
        MIN_VERSION_ALL_PROVIDERS, app_version_at_least, truthy_text, wait_for_finish_error,
    };
    use spocky_contracts::js_value::{JsValue, parse};

    #[test]
    fn stored_record_flags_use_javascript_truthiness() {
        let record =
            parse(r#"{"internal":false,"archivedAt":null,"title":"","labels":{},"other":true}"#)
                .unwrap();
        assert!(!truthy_text(&record, "internal"));
        assert!(!truthy_text(&record, "archivedAt"));
        assert!(!truthy_text(&record, "title"));
        assert!(!truthy_text(&record, "missing"));
        assert!(truthy_text(&record, "labels"));
        assert!(truthy_text(&record, "other"));
    }

    #[test]
    fn app_version_gate_matches_is_app_version_at_least() {
        let at_least = |version| app_version_at_least(version, MIN_VERSION_ALL_PROVIDERS);
        assert!(!at_least(None));
        assert!(!at_least(Some("")));
        assert!(at_least(Some("0.1.45")));
        assert!(at_least(Some("0.1.45-beta.4")));
        assert!(at_least(Some("0.2")));
        assert!(!at_least(Some("0.1.44")));
        assert!(at_least(Some("1")));
    }

    #[test]
    fn wait_error_uses_last_error_or_agent_failed() {
        assert_eq!(wait_for_finish_error("idle", None), JsValue::Null);
        assert_eq!(
            wait_for_finish_error("error", Some(&parse(r#"{"lastError":"boom"}"#).unwrap())),
            JsValue::String("boom".to_owned())
        );
        assert_eq!(
            wait_for_finish_error("error", Some(&parse(r#"{"lastError":"  "}"#).unwrap())),
            JsValue::String("Agent failed".to_owned())
        );
    }
}
