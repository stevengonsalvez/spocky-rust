//! `workspace.label.*` requests: `dispatchWorkspaceLabelMessage` and
//! `handleWorkspaceLabel*` (`session.ts:2932`, `6375-6516`), on the
//! `spocky-workspace-labels` service the daemon creates at startup
//! (`createWorkspaceLabelService`, `bootstrap.ts:880`).
//!
//! A list request with `subscribe` opens an owned subscription of the
//! `labels` family (`SessionDelivery.begin`, `owned-subscriptions/index.ts`):
//! a modern socket gets a host-assigned id and `subscriptionId` on every
//! frame, a legacy socket keeps one `labels` subscription at a time under the
//! id it asked for. A subscription ends when its socket detaches.
//! `subscription.release.request` is not routed in this crate yet, so it does
//! not end one. Workspaces a label commit rewrites are not published as
//! `workspace_update`: this crate has no workspace subscription to publish to
//! (`agent_updates.rs`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::{JsText, js_trim};
use spocky_contracts::workspace_labels::{
    RemoveKind, UpsertKind, WorkspaceLabelAssignmentSetResponse, WorkspaceLabelColor,
    WorkspaceLabelCountResponse, WorkspaceLabelDefinition as WireDefinition, WorkspaceLabelInbound,
    WorkspaceLabelListRequest, WorkspaceLabelListResponse, WorkspaceLabelOutbound,
    WorkspaceLabelRemoval, WorkspaceLabelSyncMetadata, WorkspaceLabelSyncMode,
    WorkspaceLabelUpdate, WorkspaceLabelUpdateResponse,
};
use spocky_daemon::session_api::SocketId;
use spocky_session::clock::random_uuid;
use spocky_session::provisioning::WorkspaceProvisioning;
use spocky_store::registry::WorkspaceRegistry;
use spocky_workspace_labels::sequence::{
    SequencedChange, SubscriberError, SyncMode, WorkspaceLabelChange, WorkspaceLabelCursor,
    WorkspaceLabelSync,
};
use spocky_workspace_labels::{
    CatalogIo, LabelError, RegistryAccess, WorkspaceLabelColor as ServiceColor,
    WorkspaceLabelDefinition as ServiceDefinition, WorkspaceLabelService,
    WorkspaceLabelServiceOptions, create_workspace_label_service,
};

use crate::request::Emit;
use crate::session::{RequestContext, frame, js_text};

/// The workspace registry the daemon shares with workspace provisioning.
pub struct ProvisioningRegistry(Arc<WorkspaceProvisioning>);

impl RegistryAccess for ProvisioningRegistry {
    /// Must not run on an async task: it waits for the registry lock.
    fn with_registry<T>(&self, operation: impl FnOnce(&mut WorkspaceRegistry) -> T) -> T {
        operation(&mut self.0.workspaces.blocking_lock())
    }
}

type LabelService = WorkspaceLabelService<ProvisioningRegistry>;

/// One owned `labels` subscription.
struct Subscription {
    id: String,
    source: SocketId,
    /// The service's subscriber id, once the service has accepted it.
    service_id: Arc<Mutex<Option<u64>>>,
    /// `owner.signal.aborted`.
    aborted: Arc<AtomicBool>,
}

/// The daemon's label service and the subscriptions open on it.
pub struct WorkspaceLabels {
    service: Arc<LabelService>,
    subscriptions: Mutex<Vec<Subscription>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl WorkspaceLabels {
    /// `createWorkspaceLabelService({ paseoHome, workspaceRegistry })`.
    #[must_use]
    pub fn new(provisioning: Arc<WorkspaceProvisioning>, paseo_home: &Path) -> Self {
        Self {
            service: Arc::new(create_workspace_label_service(
                WorkspaceLabelServiceOptions {
                    paseo_home: paseo_home.to_path_buf(),
                    workspace_registry: ProvisioningRegistry(provisioning),
                    io: CatalogIo::default(),
                    publisher: None,
                    journal_limit: None,
                },
            )),
            subscriptions: Mutex::new(Vec::new()),
        }
    }

    /// `workspaceLabelService.initialize()`, which bootstrap awaits before
    /// listening.
    ///
    /// # Errors
    ///
    /// Returns the failed catalog read or journal recovery.
    pub fn initialize(&self) -> Result<(), LabelError> {
        self.service.initialize()
    }

    /// Ends every subscription of a detached socket (`delivery.detach`).
    pub fn detach(&self, source: SocketId) {
        let ended: Vec<Subscription> = {
            let mut open = lock(&self.subscriptions);
            let (ended, kept) = std::mem::take(&mut *open)
                .into_iter()
                .partition(|owner| owner.source == source);
            *open = kept;
            ended
        };
        for owner in ended {
            self.stop(&owner);
        }
    }

    fn stop(&self, subscription: &Subscription) {
        subscription.aborted.store(true, Ordering::SeqCst);
        if let Some(id) = lock(&subscription.service_id).take() {
            self.service.unsubscribe(id);
        }
    }

    fn forget(&self, id: &str) {
        lock(&self.subscriptions).retain(|subscription| subscription.id != id);
    }
}

/// `WSInboundMessageSchema` for a label request: the zod error text the
/// daemon sends after `Invalid message: `, or `None` for a valid message or
/// any other type.
#[must_use]
pub fn invalid_message(message: &Value) -> Option<String> {
    let value = JsValue::from(message);
    match spocky_contracts::workspace_labels::check_session_message(&value)? {
        spocky_contracts::zod::Outcome::Invalid(text) => Some(text),
        _ => None,
    }
}

// ---- conversions ----------------------------------------------------------

const COLORS: [(WorkspaceLabelColor, ServiceColor); 10] = [
    (WorkspaceLabelColor::Violet, ServiceColor::Violet),
    (WorkspaceLabelColor::Sky, ServiceColor::Sky),
    (WorkspaceLabelColor::Emerald, ServiceColor::Emerald),
    (WorkspaceLabelColor::Orange, ServiceColor::Orange),
    (WorkspaceLabelColor::Pink, ServiceColor::Pink),
    (WorkspaceLabelColor::Indigo, ServiceColor::Indigo),
    (WorkspaceLabelColor::Teal, ServiceColor::Teal),
    (WorkspaceLabelColor::Red, ServiceColor::Red),
    (WorkspaceLabelColor::Amber, ServiceColor::Amber),
    (WorkspaceLabelColor::Blue, ServiceColor::Blue),
];

fn service_color(color: WorkspaceLabelColor) -> ServiceColor {
    COLORS
        .iter()
        .find(|(wire, _)| *wire == color)
        .map_or(ServiceColor::Violet, |(_, service)| *service)
}

fn wire_color(color: ServiceColor) -> WorkspaceLabelColor {
    COLORS
        .iter()
        .find(|(_, service)| *service == color)
        .map_or(WorkspaceLabelColor::Violet, |(wire, _)| *wire)
}

fn wire_definition(label: &ServiceDefinition) -> WireDefinition {
    WireDefinition {
        name: JsText::from_js(label.name.clone()),
        color: wire_color(label.color),
    }
}

fn text(value: &str) -> JsText {
    JsText::from_js(value.to_owned())
}

fn wire_sync(sync: &WorkspaceLabelSync) -> WorkspaceLabelSyncMetadata {
    WorkspaceLabelSyncMetadata {
        mode: match sync.sync.mode {
            SyncMode::Snapshot => WorkspaceLabelSyncMode::Snapshot,
            SyncMode::Changes => WorkspaceLabelSyncMode::Changes,
        },
        generation: text(&sync.sync.generation),
        head_seq: sync.sync.head_seq,
        removals: sync
            .sync
            .removals
            .iter()
            .map(|removal| WorkspaceLabelRemoval {
                name: text(&removal.name),
                seq: removal.seq,
            })
            .collect(),
    }
}

fn outbound(message: &WorkspaceLabelOutbound) -> Value {
    serde_json::to_value(message).expect("contract messages serialize to JSON")
}

/// `emitWorkspaceLabelError`: `rpc_error` with `requestId, requestType, code,
/// error`.
fn error_frame(request: &WorkspaceLabelInbound, error: &LabelError) -> Value {
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(request.request_id()));
    payload.insert(
        "requestType",
        JsValue::String(request.request_type().to_owned()),
    );
    payload.insert(
        "code",
        JsValue::String(
            error
                .code()
                .unwrap_or_else(|| "workspace_label_failed".to_owned()),
        ),
    );
    payload.insert("error", JsValue::String(error.to_string()));
    frame("rpc_error", payload)
}

/// The change as the baseline's `onChange` hands it on: `{ ...change,
/// generation, seq }`, plus `subscriptionId` for a modern socket.
fn update_frame(entry: &SequencedChange, subscription_id: Option<&str>) -> Value {
    let subscription_id = subscription_id.map(text);
    let payload = match &entry.change {
        WorkspaceLabelChange::Upsert {
            label,
            previous_name,
        } => WorkspaceLabelUpdate::Upsert {
            kind: UpsertKind::Upsert,
            label: wire_definition(label),
            previous_name: previous_name.as_deref().map(text),
            generation: text(&entry.generation),
            seq: entry.seq,
            subscription_id,
        },
        WorkspaceLabelChange::Remove { name } => WorkspaceLabelUpdate::Remove {
            kind: RemoveKind::Remove,
            name: text(name),
            generation: text(&entry.generation),
            seq: entry.seq,
            subscription_id,
        },
    };
    outbound(&WorkspaceLabelOutbound::Update { payload })
}

// ---- handlers -------------------------------------------------------------

/// Runs a blocking service call off the async runtime, since the registry lock
/// it takes is an async mutex.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, LabelError> + Send + 'static,
) -> Result<T, LabelError> {
    tokio::task::spawn_blocking(call)
        .await
        .unwrap_or_else(|error| Err(LabelError::Storage(error.to_string())))
}

/// `dispatchWorkspaceLabelMessage`.
///
/// # Errors
///
/// Returns the text of an error the baseline throws outside its `try`, which
/// the request handler turns into `rpc_error`.
#[allow(clippy::too_many_lines)]
pub(crate) async fn workspace_label(
    context: &Arc<RequestContext>,
    request: WorkspaceLabelInbound,
    emit: &Emit,
) -> Result<(), JsText> {
    let labels = &context.services.labels;
    let service = Arc::clone(&labels.service);
    let request_id = request.request_id().clone();
    let outcome: Result<WorkspaceLabelOutbound, LabelError> = match &request {
        WorkspaceLabelInbound::List(list) if list.subscribe.is_some() => {
            return list_subscribe(context, list, &request, emit).await;
        }
        WorkspaceLabelInbound::List(list) => {
            let cursor = list.sync.as_ref().map(cursor_of);
            blocking(move || service.list(cursor.as_ref()))
                .await
                .map(|sync| WorkspaceLabelOutbound::ListResponse {
                    payload: list_response(request_id, None, &sync),
                })
        }
        WorkspaceLabelInbound::AssignmentSet(set) => {
            let workspace_id = set.workspace_id.as_str().to_owned();
            let definition = ServiceDefinition {
                name: set.label.name.as_str().to_owned(),
                color: service_color(set.label.color),
            };
            let assigned = set.assigned;
            blocking(move || service.set_assignment(&workspace_id, &definition, assigned))
                .await
                .map(|result| WorkspaceLabelOutbound::AssignmentSetResponse {
                    payload: WorkspaceLabelAssignmentSetResponse {
                        request_id,
                        label: wire_definition(&result.label),
                        workspace_labels: result.workspace_labels.iter().map(|l| text(l)).collect(),
                    },
                })
        }
        WorkspaceLabelInbound::Update(update) => {
            let name = update.name.as_str().to_owned();
            let new_name = update
                .new_name
                .as_ref()
                .map(|name| name.as_str().to_owned());
            let color = update.color.map(service_color);
            blocking(move || service.update(&name, new_name.as_deref(), color))
                .await
                .map(|result| WorkspaceLabelOutbound::UpdateResponse {
                    payload: WorkspaceLabelUpdateResponse {
                        request_id,
                        label: wire_definition(&result.label),
                        affected_workspace_count: result.affected_workspace_count as u64,
                    },
                })
        }
        WorkspaceLabelInbound::Delete(delete) => {
            let name = delete.name.as_str().to_owned();
            blocking(move || service.delete(&name))
                .await
                .map(|affected| WorkspaceLabelOutbound::DeleteResponse {
                    payload: WorkspaceLabelCountResponse {
                        request_id,
                        affected_workspace_count: affected as u64,
                    },
                })
        }
        WorkspaceLabelInbound::DeleteInspect(inspect) => {
            let name = inspect.name.as_str().to_owned();
            blocking(move || service.count_affected_workspaces(&name))
                .await
                .map(|affected| WorkspaceLabelOutbound::DeleteInspectResponse {
                    payload: WorkspaceLabelCountResponse {
                        request_id,
                        affected_workspace_count: affected as u64,
                    },
                })
        }
    };
    match outcome {
        Ok(message) => emit(outbound(&message)),
        Err(error) => emit(error_frame(&request, &error)),
    }
    Ok(())
}

fn cursor_of(
    sync: &spocky_contracts::workspace_labels::WorkspaceLabelSyncCursor,
) -> WorkspaceLabelCursor {
    WorkspaceLabelCursor {
        generation: sync.generation.as_str().to_owned(),
        after_seq: u64::try_from(sync.after_seq.get()).unwrap_or(0),
    }
}

fn list_response(
    request_id: JsText,
    subscription_id: Option<JsText>,
    sync: &WorkspaceLabelSync,
) -> WorkspaceLabelListResponse {
    WorkspaceLabelListResponse {
        request_id,
        subscription_id,
        labels: sync.labels.iter().map(wire_definition).collect(),
        sync: wire_sync(sync),
    }
}

/// The buffer between `service.subscribe` registering and the list response
/// going out: a change that lands in between waits, keyed by label name, and
/// goes out after the response if the snapshot did not already include it.
struct Bootstrap {
    ready: bool,
    pending: Vec<(String, SequencedChange)>,
}

/// `handleWorkspaceLabelList` with `subscribe`.
#[allow(clippy::too_many_lines)]
async fn list_subscribe(
    context: &Arc<RequestContext>,
    list: &WorkspaceLabelListRequest,
    request: &WorkspaceLabelInbound,
    emit: &Emit,
) -> Result<(), JsText> {
    let modern = context.modern;
    let requested = list
        .subscribe
        .as_ref()
        .and_then(|subscribe| subscribe.subscription_id.as_ref());
    // `delivery.begin("labels", ...)`.
    if modern && list.request_id.as_str().is_empty() {
        return Err(JsText::new("Owned subscriptions require a requestId"));
    }
    if modern && requested.is_some() {
        return Err(JsText::new("Subscription IDs are assigned by the host"));
    }
    let labels = &context.services.labels;
    if !modern {
        // One `labels` subscription per legacy socket: a new one replaces it.
        let prior: Vec<Subscription> = {
            let mut open = lock(&labels.subscriptions);
            let (prior, kept) = std::mem::take(&mut *open)
                .into_iter()
                .partition(|owner| owner.source == context.source);
            *open = kept;
            prior
        };
        for owner in prior {
            labels.stop(&owner);
        }
    }
    let id = random_uuid();
    let response_id = if modern {
        id.clone()
    } else {
        requested
            .map(|requested| js_trim(requested.as_str()))
            .filter(|trimmed| !trimmed.is_empty())
            .map_or_else(|| id.clone(), str::to_owned)
    };
    let aborted = Arc::new(AtomicBool::new(false));
    let service_id = Arc::new(Mutex::new(None));
    lock(&labels.subscriptions).push(Subscription {
        id: id.clone(),
        source: context.source,
        service_id: Arc::clone(&service_id),
        aborted: Arc::clone(&aborted),
    });

    let bootstrap = Arc::new(Mutex::new(Bootstrap {
        ready: false,
        pending: Vec::new(),
    }));
    let tag = modern.then(|| response_id.clone());
    let on_change = {
        let (bootstrap, aborted, emit, tag) = (
            Arc::clone(&bootstrap),
            Arc::clone(&aborted),
            Arc::clone(emit),
            tag.clone(),
        );
        Box::new(move |entry: &SequencedChange| {
            if aborted.load(Ordering::SeqCst) {
                return Ok(());
            }
            let mut state = lock(&bootstrap);
            if state.ready {
                emit(update_frame(entry, tag.as_deref()));
            } else {
                let key = match &entry.change {
                    WorkspaceLabelChange::Upsert { label, .. } => label.name.clone(),
                    WorkspaceLabelChange::Remove { name } => name.clone(),
                };
                // `Map.set`: a repeated key keeps its place with the newer change.
                match state.pending.iter_mut().find(|(known, _)| *known == key) {
                    Some(slot) => slot.1 = entry.clone(),
                    None => state.pending.push((key, entry.clone())),
                }
            }
            Ok::<(), SubscriberError>(())
        })
    };
    let service = Arc::clone(&labels.service);
    let cursor = list.sync.as_ref().map(cursor_of);
    let opened = blocking(move || service.subscribe(cursor.as_ref(), on_change)).await;
    let subscription = match opened {
        Ok(subscription) => subscription,
        Err(error) => {
            labels.forget(&id);
            aborted.store(true, Ordering::SeqCst);
            emit(error_frame(request, &error));
            return Ok(());
        }
    };
    *lock(&service_id) = Some(subscription.id);
    if aborted.load(Ordering::SeqCst) {
        // Released while bootstrapping: the service subscription goes with it.
        labels.service.unsubscribe(subscription.id);
        return Ok(());
    }
    let snapshot = subscription.snapshot;
    emit(outbound(&WorkspaceLabelOutbound::ListResponse {
        payload: list_response(list.request_id.clone(), Some(text(&response_id)), &snapshot),
    }));
    let mut state = lock(&bootstrap);
    state.ready = true;
    for (_, change) in state.pending.drain(..) {
        if change.seq > snapshot.sync.head_seq {
            emit(update_frame(&change, tag.as_deref()));
        }
    }
    Ok(())
}
