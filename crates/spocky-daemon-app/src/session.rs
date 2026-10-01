//! The `spocky-daemon` session backend: one [`DaemonSession`] per client
//! session (`new Session(...)` in `websocket-server.ts`), routing each inbound
//! request through [`handle_request`] to its `session.ts` handler.
//!
//! Replies go to the socket that sent the request. `SessionDelivery`'s
//! owned-subscription rules (which frames a modern socket may receive) are
//! the session lane's port and are not applied here yet.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use spocky_contracts::frame::{FrameError, parse_frame};
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::json::{JsonValue, js_wire_text};
use spocky_contracts::request::FetchAgentsRequest;
use spocky_contracts::session::SessionInbound;
use spocky_contracts::text::JsText;
use spocky_contracts::ws::DaemonPermission;
use spocky_daemon::session_api::{
    ProtocolFailure, SessionBackend, SessionHandle, SessionOpen, SessionSink, SocketId,
};
use spocky_session::agent_manager::AgentManager;
use spocky_session::agent_projection::to_agent_payload;
use spocky_session::agent_storage::AgentStorage;
use spocky_session::clock::random_uuid;
use spocky_session::provisioning::WorkspaceProvisioning;
use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, resolve_project_display_name,
    resolve_workspace_display_name,
};

use crate::agent_directory::{
    CursorError, checkout_from_persisted_workspace_placement, compare, compare_with_cursor,
    decode_cursor, encode_cursor, matches_agent_updates_filter, normalize_sort,
};
use crate::authorization::SessionAuthorization;
use crate::request::{Emit, handle_request, now_millis, pong, request_type};

/// `LEGACY_PROVIDER_IDS`: providers every client may see.
const LEGACY_PROVIDER_IDS: [&str; 3] = ["claude", "codex", "opencode"];

/// What every session of the daemon shares: the agent manager, agent
/// records, and the project and workspace registries.
pub struct Services {
    pub runtime: tokio::runtime::Handle,
    pub manager: Arc<AgentManager>,
    pub storage: Arc<AgentStorage>,
    pub provisioning: Arc<WorkspaceProvisioning>,
    pub paseo_home: PathBuf,
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
        Arc::new(DaemonSession {
            id: random_uuid(),
            authorization: Arc::new(SessionAuthorization::new(&open.permissions)),
            permissions: open.permissions,
            capabilities: Mutex::new(open.client_capabilities),
            sink: open.sink,
            services: Arc::clone(&self.services),
        })
    }

    fn validate_inbound(&self, message: &Value) -> Result<(), String> {
        inbound(message)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// One client session.
pub struct DaemonSession {
    id: String,
    authorization: Arc<SessionAuthorization>,
    permissions: Vec<DaemonPermission>,
    capabilities: Mutex<Option<Value>>,
    sink: Arc<dyn SessionSink>,
    services: Arc<Services>,
}

impl DaemonSession {
    /// `supports(CLIENT_CAPS.allProviders)`.
    fn supports_all_providers(&self) -> bool {
        self.capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|caps| caps.get("all_providers"))
            == Some(&Value::Bool(true))
    }
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
        _source: SocketId,
        _app_version: Option<&str>,
    ) {
        *self
            .capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = capabilities.cloned();
    }

    fn update_app_version(&self, _app_version: &str) {}

    fn handle_message(&self, message: Value, source: SocketId) {
        let Ok(message) = inbound(&message) else {
            return;
        };
        let sink = Arc::clone(&self.sink);
        let emit: Emit = Arc::new(move |frame| sink.send_to_source(source, &frame));
        let context = Arc::new(RequestContext {
            services: Arc::clone(&self.services),
            all_providers: self.supports_all_providers(),
        });
        self.services.runtime.spawn(handle_request(
            Arc::clone(&self.authorization),
            message,
            emit,
            move |message, emit| route(context, message, emit),
        ));
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

    fn socket_detached(&self, _source: SocketId) {}

    fn cleanup(&self) {}
}

/// What one request's handler can reach.
struct RequestContext {
    services: Arc<Services>,
    all_providers: bool,
}

impl RequestContext {
    /// `isProviderVisibleToClient`.
    fn provider_visible(&self, provider: &str) -> bool {
        self.all_providers || LEGACY_PROVIDER_IDS.contains(&provider)
    }
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
        SessionInbound::FetchAgents(request) => {
            fetch_agents(&context, request, &emit).await;
            Ok(())
        }
        other => Err(JsText::new(&format!(
            "{} is not ported in spocky-daemon-app yet",
            request_type(&other)
        ))),
    }
}

fn js_text(value: &JsText) -> JsValue {
    JsValue::String(value.as_str().to_owned())
}

fn to_frame(value: JsValue) -> Value {
    serde_json::to_value(JsonValue(value)).expect("payload objects serialize to JSON")
}

/// `buildAgentPayload`: `toAgentPayload(agent)` with the stored title and
/// archive time (`enrichAgentPayload`).
async fn live_agent_payloads(context: &RequestContext) -> Result<Vec<JsValue>, String> {
    let mut payloads = Vec::new();
    for agent in context.services.manager.list_agents() {
        let mut payload =
            to_agent_payload(&agent.payload_view(), None).map_err(|error| error.to_string())?;
        let stored = context.services.storage.get(&agent.id).await;
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
        payloads.push(payload);
    }
    Ok(payloads)
}

fn truthy_text(record: &JsValue, key: &str) -> bool {
    match record.get(key) {
        Some(JsValue::String(text)) => !text.is_empty(),
        Some(JsValue::Null | JsValue::Undefined) | None => false,
        Some(_) => true,
    }
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
        .filter(|record| {
            let provider = record
                .get("provider")
                .and_then(JsValue::as_str)
                .unwrap_or("");
            registered.iter().any(|id| id == provider)
        })
        .count();
    if persisted > 0 {
        // `buildStoredAgentPayload` (agent-projections.ts) is the session
        // lane's port and is not on main yet.
        return Err("Stored agent payloads are not ported in spocky-daemon-app yet".to_owned());
    }
    Ok(live
        .into_iter()
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
    let sort = normalize_sort(request.sort.as_deref());
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
    agents.sort_by(|left, right| compare(left, right, &sort));
    let cursor_token = request.page.as_ref().and_then(|page| page.cursor.as_ref());
    if let Some(token) = cursor_token {
        let cursor = decode_cursor(token.as_str(), &sort)
            .map_err(|CursorError(message)| (message, "invalid_cursor"))?;
        agents.retain(|agent| compare_with_cursor(agent, &cursor, &sort) > 0);
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
        Some((agent, _)) if has_more => JsValue::String(encode_cursor(agent, &sort)),
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

/// `handleFetchAgents` without a subscription or directory sync.
async fn fetch_agents(context: &RequestContext, request: FetchAgentsRequest, emit: &Emit) {
    let outcome = if request.subscribe.is_some() || request.sync.is_some() {
        Err((
            "fetch_agents subscriptions and sync are not ported in spocky-daemon-app yet"
                .to_owned(),
            "fetch_agents_failed",
        ))
    } else {
        list_fetch_agents_entries(context, &request).await
    };
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(&request.request_id));
    match outcome {
        Ok(listing) => {
            for (key, value) in listing.iter() {
                payload.insert(key, value.clone());
            }
            let mut frame = JsObject::new();
            frame.insert("type", JsValue::String("fetch_agents_response".to_owned()));
            frame.insert("payload", JsValue::Object(payload));
            emit(to_frame(JsValue::Object(frame)));
        }
        Err((message, code)) => {
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
}
