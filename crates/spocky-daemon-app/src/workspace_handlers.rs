//! `workspace.create.request` (directory source) and
//! `fetch_workspaces_request`, as `session.ts` handles them, over the
//! session crate's `CreationService` and `describe_workspace`.
//!
//! The workspace directory's status aggregation (agent buckets, terminal and
//! provider-subagent activity, `statusEnteredAt` history, git data) is not
//! ported: listed descriptors carry `describeWorkspaceRecord`'s values.

use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use spocky_contracts::frame::frame_text;
use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_contracts::json::js_wire_text;
use spocky_contracts::request::{
    CreationKind, FetchWorkspacesRequest, SortDirection, WorkspaceCreateRequest, WorkspaceSortKey,
    WorkspaceSource,
};
use spocky_contracts::text::{JsText, js_trim};
use spocky_session::agent_identity::resolve_create_agent_titles;
use spocky_session::creation::{
    CreationError, CreationFuture, CreationInput, CreationTarget, Exists, Observer, Provision,
    Provisioned, ValidateCompleted,
};
use spocky_session::paths::expand_tilde;
use spocky_session::provisioning::{WorkspaceCreateContext, WorkspaceProvisioning};
use spocky_session::workspace_descriptor::describe_workspace;
use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, resolve_project_display_name,
};
use spocky_store::time::parse_iso_millis;

use crate::agent_directory::{
    CursorError, Pager, SortSpec, SortValue, compare, compare_with_cursor, decode_cursor,
    encode_cursor,
};
use crate::request::Emit;
use crate::session::{RequestContext, Services, frame, js_text, to_frame};

/// `WORKSPACE_STATE_BUCKET_PRIORITY`.
fn bucket_priority(status: Option<&str>) -> f64 {
    match status {
        Some("needs_input") => 0.0,
        Some("failed") => 1.0,
        Some("running") => 2.0,
        Some("attention") => 3.0,
        _ => 4.0,
    }
}

fn workspace_sort_value(workspace: &JsValue, key: &str) -> SortValue {
    let text = |field: &str| workspace.get(field).and_then(JsValue::as_str);
    match key {
        "status_priority" => SortValue::Number(bucket_priority(text("status"))),
        // `workspace.activityAt ? Date.parse(workspace.activityAt) : null`.
        #[allow(clippy::cast_precision_loss)]
        "activity_at" => match text("activityAt").filter(|at| !at.is_empty()) {
            Some(at) => SortValue::Number(parse_iso_millis(at).map_or(f64::NAN, |ms| ms as f64)),
            None => SortValue::Null,
        },
        // ponytail: ASCII-lowercase stands in for toLocaleLowerCase.
        "name" => SortValue::Text(text("name").unwrap_or_default().to_ascii_lowercase()),
        _ => SortValue::Text(text("projectId").unwrap_or_default().to_ascii_lowercase()),
    }
}

/// The workspace directory's pager (`FETCH_WORKSPACES_SORT_KEYS`,
/// `activity_at desc`).
pub const WORKSPACES: Pager = Pager {
    label: "fetch_workspaces",
    keys: &["status_priority", "activity_at", "name", "project_id"],
    default_sort: SortSpec {
        key: "activity_at",
        ascending: false,
    },
    value: workspace_sort_value,
};

/// `describeWorkspaceRecord(workspace, project)` with no git snapshot and
/// no workspace scripts, as a payload object in wire key order.
fn describe(
    workspace: &PersistedWorkspaceRecord,
    project: Option<&PersistedProjectRecord>,
) -> JsValue {
    let descriptor = describe_workspace(workspace, project, None, Vec::new());
    let text = frame_text(&descriptor).expect("descriptors serialize");
    js_value::parse(&text).expect("frame_text writes JSON")
}

/// `activeWorkspaceRecords` described, `archivingAt` reset to `null`
/// (nothing is being archived in this port).
async fn list_descriptors(provisioning: &WorkspaceProvisioning) -> Vec<JsValue> {
    let workspaces = provisioning.workspaces.lock().await.list();
    let projects = provisioning.projects.lock().await.list();
    workspaces
        .iter()
        .filter(|workspace| workspace.archived_at.is_none())
        .filter(|workspace| {
            !projects.iter().any(|project| {
                project.project_id == workspace.project_id && project.archived_at.is_some()
            })
        })
        .map(|workspace| {
            let project = projects.iter().find(|project| {
                project.project_id == workspace.project_id && project.archived_at.is_none()
            });
            describe(workspace, project)
        })
        .collect()
}

/// `listEmptyProjects`: active projects with no active workspace.
async fn list_empty_projects(provisioning: &WorkspaceProvisioning) -> Vec<JsValue> {
    let workspaces = provisioning.workspaces.lock().await.list();
    let projects = provisioning.projects.lock().await.list();
    projects
        .iter()
        .filter(|project| project.archived_at.is_none())
        .filter(|project| {
            !workspaces.iter().any(|workspace| {
                workspace.archived_at.is_none() && workspace.project_id == project.project_id
            })
        })
        .map(|project| {
            let mut entry = JsObject::new();
            let text = |value: &str| JsValue::String(value.to_owned());
            entry.insert("projectId", text(&project.project_id));
            entry.insert(
                "projectKey",
                project
                    .project_key
                    .as_deref()
                    .map_or(JsValue::Undefined, text),
            );
            entry.insert(
                "projectDisplayName",
                text(resolve_project_display_name(project)),
            );
            entry.insert(
                "projectCustomName",
                project.custom_name.as_deref().map_or(JsValue::Null, text),
            );
            entry.insert(
                "projectCustomIconRevision",
                project
                    .custom_icon_revision
                    .as_deref()
                    .map_or(JsValue::Null, text),
            );
            entry.insert("projectRootPath", text(&project.root_path));
            entry.insert("projectKind", text(project.kind.as_str()));
            JsValue::Object(entry)
        })
        .collect()
}

/// `matchesFilter` for `fetch_workspaces_request`.
fn matches_filter(workspace: &JsValue, request: &FetchWorkspacesRequest) -> bool {
    let Some(filter) = request.filter.as_ref() else {
        return true;
    };
    let text = |field: &str| workspace.get(field).and_then(JsValue::as_str).unwrap_or("");
    if let Some(project_id) = filter.project_id.as_ref().map(|id| js_trim(id.as_str()))
        && !project_id.is_empty()
        && text("projectId") != project_id
    {
        return false;
    }
    if let Some(query) = filter.query.as_ref().map(|query| js_trim(query.as_str()))
        && !query.is_empty()
    {
        let query = query.to_ascii_lowercase();
        let hit = ["name", "projectId", "id"]
            .iter()
            .any(|field| text(field).to_ascii_lowercase().contains(&query));
        if !hit {
            return false;
        }
    }
    true
}

fn workspace_sort(request: &FetchWorkspacesRequest) -> Vec<SortSpec> {
    request
        .sort
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|entry| SortSpec {
            key: match entry.key {
                WorkspaceSortKey::StatusPriority => "status_priority",
                WorkspaceSortKey::ActivityAt => "activity_at",
                WorkspaceSortKey::Name => "name",
                WorkspaceSortKey::ProjectId => "project_id",
            },
            ascending: matches!(entry.direction, SortDirection::Asc),
        })
        .collect()
}

/// `WorkspaceDirectory.listFetchEntries`.
async fn list_fetch_entries(
    services: &Services,
    request: &FetchWorkspacesRequest,
) -> Result<JsObject, (String, &'static str)> {
    let sort = WORKSPACES.normalize_sort(&workspace_sort(request));
    let mut entries = list_descriptors(&services.provisioning).await;
    entries.retain(|workspace| matches_filter(workspace, request));
    entries.sort_by(|left, right| compare(&WORKSPACES, left, right, &sort));
    let cursor_token = request.page.as_ref().and_then(|page| page.cursor.as_ref());
    if let Some(token) = cursor_token {
        let cursor = decode_cursor(&WORKSPACES, token.as_str(), &sort)
            .map_err(|CursorError(message)| (message, "invalid_cursor"))?;
        entries.retain(|workspace| compare_with_cursor(&WORKSPACES, workspace, &cursor, &sort) > 0);
    }
    let limit = request
        .page
        .as_ref()
        .map_or(200, |page| usize::try_from(page.limit.get()).unwrap_or(200));
    let has_more = entries.len() > limit;
    entries.truncate(limit);
    let next_cursor = match entries.last() {
        Some(last) if has_more => JsValue::String(encode_cursor(&WORKSPACES, last, &sort)),
        _ => JsValue::Null,
    };
    let project_filter = request
        .filter
        .as_ref()
        .and_then(|filter| filter.project_id.as_ref())
        .map(|id| js_trim(id.as_str()).to_owned())
        .filter(|id| !id.is_empty());
    let empty_projects = if cursor_token.is_some() {
        Vec::new()
    } else {
        list_empty_projects(&services.provisioning)
            .await
            .into_iter()
            .filter(|project| {
                project_filter
                    .as_deref()
                    .is_none_or(|id| project.get("projectId").and_then(JsValue::as_str) == Some(id))
            })
            .collect()
    };
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
    payload.insert("emptyProjects", JsValue::Array(empty_projects));
    payload.insert("pageInfo", JsValue::Object(page_info));
    Ok(payload)
}

/// `handleFetchWorkspacesRequest` without a subscription or directory sync.
pub(crate) async fn fetch_workspaces(
    context: &RequestContext,
    request: FetchWorkspacesRequest,
    emit: &Emit,
) {
    let outcome = if request.subscribe.is_some() || request.sync.is_some() {
        Err((
            "fetch_workspaces subscriptions and sync are not ported in spocky-daemon-app yet"
                .to_owned(),
            "fetch_workspaces_failed",
        ))
    } else {
        list_fetch_entries(&context.services, &request).await
    };
    match outcome {
        Ok(listing) => {
            let mut payload = JsObject::new();
            payload.insert("requestId", js_text(&request.request_id));
            for (key, value) in listing.iter() {
                payload.insert(key, value.clone());
            }
            emit(frame("fetch_workspaces_response", payload));
        }
        Err((message, code)) => {
            let mut error = JsObject::new();
            error.insert("requestId", js_text(&request.request_id));
            error.insert(
                "requestType",
                JsValue::String("fetch_workspaces_request".to_owned()),
            );
            error.insert("error", JsValue::String(message));
            error.insert("code", JsValue::String(code.to_owned()));
            emit(frame("rpc_error", error));
        }
    }
}

fn boxed<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, CreationError>> + Send + 'static,
) -> CreationFuture<T> {
    Box::pin(future)
}

/// `creationResourceExists`.
#[must_use]
pub fn resource_exists(services: &Arc<Services>) -> Exists {
    let services = Arc::clone(services);
    Arc::new(move |kind, id| {
        let services = Arc::clone(&services);
        boxed(async move {
            Ok(match kind {
                CreationKind::Workspace => services
                    .provisioning
                    .workspaces
                    .lock()
                    .await
                    .get(&id)
                    .is_some(),
                CreationKind::Agent => {
                    services.manager.get_agent(&id).is_some()
                        || services.storage.get(&id).await.is_some()
                }
            })
        })
    })
}

/// Where `validateCompletedCreation` finds the daemon's services: the
/// creation service is built before them, so the slot is filled after.
pub type ServicesSlot = Arc<OnceLock<Weak<Services>>>;

/// `validateCompletedCreation` (`websocket-server.ts`): a completed receipt
/// whose workspace or agent is gone, or whose directory vanished, is stale.
#[must_use]
pub fn validate_completed(slot: ServicesSlot) -> ValidateCompleted {
    Arc::new(move |snapshot: JsValue| {
        let services = slot.get().and_then(Weak::upgrade);
        boxed(async move {
            let Some(services) = services else {
                return Ok(());
            };
            let workspace_id = (snapshot.get("kind").and_then(JsValue::as_str)
                == Some("workspace"))
            .then(|| {
                snapshot
                    .get("workspace")
                    .and_then(|w| w.get("id"))
                    .and_then(JsValue::as_str)
            })
            .flatten()
            .map(str::to_owned);
            if let Some(workspace_id) = workspace_id {
                let workspace = services
                    .provisioning
                    .workspaces
                    .lock()
                    .await
                    .get(&workspace_id);
                let Some(workspace) = workspace else {
                    return Err(CreationError::new(
                        "Previously created workspace no longer exists",
                    ));
                };
                if !Path::new(&workspace.cwd).is_dir() {
                    return Err(CreationError {
                        message: format!("Directory not found: {}", workspace.cwd),
                        code: Some("directory_not_found".to_owned()),
                    });
                }
            }
            if let Some(agent_id) = snapshot.get("agentId").and_then(JsValue::as_str)
                && services.manager.get_agent(agent_id).is_none()
                && services.storage.get(agent_id).await.is_none()
            {
                return Err(CreationError::new(
                    "Previously created agent no longer exists",
                ));
            }
            Ok(())
        })
    })
}

/// `creationUpdate(snapshot)` sent to the requesting socket.
fn creation_observer(emit: &Emit) -> Observer {
    let emit = Arc::clone(emit);
    Arc::new(move |snapshot: &JsValue| {
        let kind = match snapshot.get("kind").and_then(JsValue::as_str) {
            Some("workspace") => "workspace.create.update",
            _ => "agent.create.update",
        };
        let mut message = JsObject::new();
        message.insert("type", JsValue::String(kind.to_owned()));
        message.insert("payload", snapshot.clone());
        emit(to_frame(JsValue::Object(message)));
    })
}

/// The request without `requestId`, `type`, `subscribe`, and
/// `idempotencyKey`: the intent `CreationService` fingerprints, in zod key
/// order.
fn creation_intent(request: &WorkspaceCreateRequest) -> JsValue {
    let text = serde_json::to_string(request).expect("requests serialize");
    let parsed = js_value::parse(&js_wire_text(&text)).expect("serde_json writes JSON");
    let mut intent = JsObject::new();
    for (key, value) in parsed.as_object().map(JsObject::iter).into_iter().flatten() {
        if !matches!(key, "requestId" | "type" | "subscribe" | "idempotencyKey") {
            intent.insert(key, value.clone());
        }
    }
    JsValue::Object(intent)
}

/// `handleWorkspaceCreateLocal`, run as the creation's `provision` step.
fn provision_directory(
    services: Arc<Services>,
    request: WorkspaceCreateRequest,
    first_agent_prompt: Option<String>,
) -> Provision {
    Box::new(move |workspace_id: Option<String>| {
        boxed(async move {
            let WorkspaceSource::Directory { path, project_id } = &request.source else {
                return Err(CreationError::new("Unexpected workspace source"));
            };
            let cwd = expand_tilde(path.as_str(), &services.home);
            if !Path::new(&cwd).is_dir() {
                return Err(CreationError {
                    message: format!("Directory not found: {cwd}"),
                    code: Some("directory_not_found".to_owned()),
                });
            }
            let explicit_title = request
                .title
                .as_ref()
                .map(|title| js_trim(title.as_str()))
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            let prompt_title =
                resolve_create_agent_titles(None, first_agent_prompt.as_deref()).provisional_title;
            let created = services
                .provisioning
                .create_workspace_for_directory(
                    &cwd,
                    explicit_title.or(prompt_title).as_deref(),
                    project_id.as_ref().map(JsText::as_str),
                    WorkspaceCreateContext {
                        expects_initial_agent: request.first_agent_context.is_some(),
                        workspace_id,
                    },
                )
                .await
                .map_err(|error| CreationError::new(error.to_string()))?;
            Ok(Provisioned {
                workspace: describe(&created.workspace, Some(&created.project)),
                setup_skipped_reason: None,
            })
        })
    })
}

fn snapshot_field(snapshot: &JsValue, key: &str) -> JsValue {
    snapshot.get(key).cloned().unwrap_or(JsValue::Undefined)
}

/// `handleWorkspaceCreation` for a directory source without an initial
/// agent.
pub(crate) async fn workspace_create(
    context: &RequestContext,
    request: WorkspaceCreateRequest,
    emit: &Emit,
) {
    let request_id = js_text(&request.request_id);
    let respond = |payload: JsObject| emit(frame("workspace.create.response", payload));
    let services = Arc::clone(&context.services);
    let outcome: Result<JsValue, CreationError> = async {
        if request.agent.is_some() {
            return Err(CreationError::new(
                "workspace.create with an initial agent is not ported in spocky-daemon-app yet",
            ));
        }
        if !matches!(request.source, WorkspaceSource::Directory { .. }) {
            return Err(CreationError::new(
                "workspace.create from a worktree source is not ported in spocky-daemon-app yet",
            ));
        }
        let key = request.idempotency_key.as_ref().map_or_else(
            || request.request_id.as_str().to_owned(),
            |key| key.as_str().to_owned(),
        );
        let first_prompt = request
            .first_agent_context
            .as_ref()
            .and_then(|context| context.prompt.as_ref())
            .map(|prompt| prompt.as_str().to_owned());
        let input = CreationInput {
            target: CreationTarget::Workspace,
            key,
            request: creation_intent(&request),
            workspace_id: request
                .workspace_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            agent_id: None,
            has_agent: false,
            has_prompt: false,
            exists: resource_exists(&services),
            provision: Some(provision_directory(
                Arc::clone(&services),
                request.clone(),
                first_prompt,
            )),
            create_agent: None,
        };
        let observer = (request.subscribe == Some(true)).then(|| creation_observer(emit));
        services.creation.create(input, observer).await
    }
    .await;
    let mut payload = JsObject::new();
    payload.insert("requestId", request_id);
    match outcome {
        Ok(creation) => {
            let workspace = match snapshot_field(&creation, "workspace") {
                JsValue::Undefined => JsValue::Null,
                workspace => workspace,
            };
            payload.insert("workspace", workspace);
            payload.insert("agent", snapshot_field(&creation, "agent"));
            payload.insert("creation", creation.clone());
            payload.insert(
                "setupSkippedReason",
                snapshot_field(&creation, "setupSkippedReason"),
            );
            payload.insert("error", snapshot_field(&creation, "error"));
            payload.insert("errorCode", snapshot_field(&creation, "errorCode"));
            payload.insert("setupTerminalId", JsValue::Null);
        }
        Err(error) => {
            payload.insert("workspace", JsValue::Null);
            payload.insert("error", JsValue::String(error.message));
            payload.insert(
                "errorCode",
                error.code.map_or(JsValue::Undefined, JsValue::String),
            );
            payload.insert("setupTerminalId", JsValue::Null);
        }
    }
    respond(payload);
}

#[cfg(test)]
mod tests {
    use spocky_contracts::js_value::parse;

    use super::{WORKSPACES, workspace_sort_value};
    use crate::agent_directory::{SortValue, compare};

    #[test]
    fn workspaces_sort_by_activity_then_id() {
        let sort = WORKSPACES.normalize_sort(&[]);
        let recent = parse(r#"{"id":"wks_b","activityAt":"2026-10-02T00:00:00.000Z"}"#).unwrap();
        let none = parse(r#"{"id":"wks_a","activityAt":null}"#).unwrap();
        // desc: a missing activity (null) sorts after any timestamp.
        assert_eq!(
            compare(&WORKSPACES, &recent, &none, &sort),
            std::cmp::Ordering::Less
        );
        assert_eq!(workspace_sort_value(&none, "activity_at"), SortValue::Null);
        assert_eq!(
            workspace_sort_value(&parse(r#"{"status":"done"}"#).unwrap(), "status_priority"),
            SortValue::Number(4.0)
        );
    }
}
