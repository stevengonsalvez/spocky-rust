//! `agent_update` fan-out: `agent-updates-service.ts`
//! (`createAgentUpdatesService`) with the `"agents"` family of
//! `SessionDelivery.begin` (`owned-subscriptions/index.ts`) it is fed by.
//!
//! Directory sync (`sequenceAgentUpdate` with `includeSequence`) is not
//! ported: `fetch_agents_request` with `sync` is rejected, so every payload
//! goes out unsequenced, as the baseline sends it without sync. No session
//! here holds a workspace subscription, so `emitWorkspaceUpdateForWorkspaceId`
//! has no observer and is not modeled.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use spocky_contracts::js::date_parse;
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::request::AgentDirectoryFilter;
use spocky_daemon::session_api::{SessionSink, SocketId};
use spocky_session::agent_manager::{AgentLifecycle, ManagedAgentSnapshot};
use spocky_session::clock::random_uuid;
use tokio::sync::oneshot;

use crate::agent_directory::matches_agent_updates_filter;
use crate::authorization::SessionAuthorization;
pub use crate::reply_window::WaitGuard;
use crate::reply_window::{Dispatched, Job, Tails, Waits};
use crate::session::{Services, agent_payload, placement_for_workspace, to_frame};

/// `isProviderVisibleToClient` for the owning socket.
pub type ProviderVisible = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// One `AgentUpdatesSubscriptionState` and its `SubscriptionOwner`.
struct Subscription {
    id: String,
    response_id: String,
    source: SocketId,
    modern: bool,
    filter: Option<AgentDirectoryFilter>,
    bootstrapping: bool,
    /// `pendingUpdatesByAgentId`, in `Map` insertion order.
    pending: Vec<(String, JsObject)>,
}

/// A subscription [`AgentUpdates::begin`] opened.
pub struct Owner {
    pub id: String,
    pub response_id: String,
}

struct Shared {
    subscriptions: Mutex<Vec<Subscription>>,
    sink: Arc<dyn SessionSink>,
    authorization: Arc<SessionAuthorization>,
    provider_visible: ProviderVisible,
}

/// The session's `AgentUpdatesService`.
pub struct AgentUpdates {
    shared: Arc<Shared>,
    /// `liveAgentUpdateTails`: one ordered queue per agent.
    tails: Tails<ManagedAgentSnapshot>,
    /// The `wait_for_finish` requests in flight, whose replies the updates
    /// they wake are held behind.
    waits: Arc<Waits>,
}

/// How long an update held behind a woken wait's reply waits for it.
const REPLY_HOLD: std::time::Duration = std::time::Duration::from_secs(2);

/// What a dispatched agent state says about its agent.
fn dispatched(agent: &ManagedAgentSnapshot) -> Dispatched {
    let turn = agent.active_foreground_turn_id.is_some();
    let permission = !agent.pending_permissions.is_empty();
    Dispatched {
        agent_id: agent.id.clone(),
        active: agent.lifecycle == AgentLifecycle::Running || turn,
        settles: permission
            || (matches!(
                agent.lifecycle,
                AgentLifecycle::Idle | AgentLifecycle::Error
            ) && !turn),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn upsert_target(payload: &JsObject) -> String {
    let id = match payload.get("kind").and_then(JsValue::as_str) {
        Some("remove") => payload.get("agentId"),
        _ => payload.get("agent").and_then(|agent| agent.get("id")),
    };
    id.and_then(JsValue::as_str).unwrap_or_default().to_owned()
}

fn upsert_provider(payload: &JsObject) -> Option<&str> {
    if payload.get("kind").and_then(JsValue::as_str) != Some("upsert") {
        return None;
    }
    payload
        .get("agent")
        .and_then(|agent| agent.get("provider"))
        .and_then(JsValue::as_str)
}

impl Shared {
    /// `owner.emit({ type: "agent_update", payload })`: a modern socket gets
    /// the subscription id appended to the payload (`withSubscriptionId`).
    fn emit(&self, subscription: &Subscription, payload: &JsObject) {
        let mut payload = payload.clone();
        if subscription.modern {
            payload.insert(
                "subscriptionId",
                JsValue::String(subscription.response_id.clone()),
            );
        }
        let mut message = JsObject::new();
        message.insert("type", JsValue::String("agent_update".to_owned()));
        message.insert("payload", JsValue::Object(payload));
        let frame: Value = to_frame(JsValue::Object(message));
        if self.authorization.allows_outbound(&frame) {
            self.sink.send_to_source(subscription.source, &frame);
        }
    }

    /// `bufferOrEmit`.
    fn buffer_or_emit(&self, subscription: &mut Subscription, payload: JsObject) {
        if let Some(provider) = upsert_provider(&payload)
            && !(self.provider_visible)(provider)
        {
            return;
        }
        if subscription.bootstrapping {
            let target = upsert_target(&payload);
            match subscription
                .pending
                .iter_mut()
                .find(|(agent_id, _)| *agent_id == target)
            {
                Some(slot) => slot.1 = payload,
                None => subscription.pending.push((target, payload)),
            }
            return;
        }
        self.emit(subscription, &payload);
    }

    /// `publishPayload` for a live agent (`emitLiveAgentUpdate`).
    async fn publish(&self, services: &Services, agent: &ManagedAgentSnapshot) {
        let observers: Vec<String> = lock(&self.subscriptions)
            .iter()
            .map(|subscription| subscription.id.clone())
            .collect();
        if observers.is_empty() {
            return;
        }
        // The baseline logs and drops a failed update.
        let Ok(payload) = agent_payload(services, agent).await else {
            return;
        };
        let project = match payload.get("workspaceId").and_then(JsValue::as_str) {
            Some(workspace_id) if !workspace_id.is_empty() => {
                placement_for_workspace(services, workspace_id).await
            }
            _ => JsValue::Null,
        };
        let mut subscriptions = lock(&self.subscriptions);
        for id in observers {
            let Some(subscription) = subscriptions
                .iter_mut()
                .find(|subscription| subscription.id == id)
            else {
                continue;
            };
            let matches = !matches!(project, JsValue::Null)
                && matches_agent_updates_filter(&payload, &project, subscription.filter.as_ref());
            let mut update = JsObject::new();
            if matches {
                update.insert("kind", JsValue::String("upsert".to_owned()));
                update.insert("agent", payload.clone());
                update.insert("project", project.clone());
            } else {
                update.insert("kind", JsValue::String("remove".to_owned()));
                update.insert(
                    "agentId",
                    payload.get("id").cloned().unwrap_or(JsValue::Undefined),
                );
            }
            self.buffer_or_emit(subscription, update);
        }
    }
}

impl AgentUpdates {
    /// Starts the update worker on the services' runtime.
    #[must_use]
    pub fn new(
        services: &Arc<Services>,
        sink: Arc<dyn SessionSink>,
        authorization: Arc<SessionAuthorization>,
        provider_visible: ProviderVisible,
    ) -> Self {
        let shared = Arc::new(Shared {
            subscriptions: Mutex::new(Vec::new()),
            sink,
            authorization,
            provider_visible,
        });
        let worker = Arc::clone(&shared);
        let worker_services = Arc::clone(services);
        let tails = Tails::new(
            services.runtime.clone(),
            REPLY_HOLD,
            Arc::new(move |agent: ManagedAgentSnapshot| {
                let shared = Arc::clone(&worker);
                let services = Arc::clone(&worker_services);
                Box::pin(async move { shared.publish(&services, &agent).await })
            }),
        );
        Self {
            shared,
            tails,
            waits: Arc::new(Waits::default()),
        }
    }

    /// `delivery.begin("agents", requestedId, ...)` then `beginSubscription`.
    ///
    /// # Errors
    ///
    /// The baseline's `begin` errors for a modern socket: no `requestId`, or
    /// a client-chosen subscription id.
    pub fn begin(
        &self,
        source: SocketId,
        modern: bool,
        has_request_id: bool,
        requested_id: Option<&str>,
        filter: Option<AgentDirectoryFilter>,
    ) -> Result<Owner, String> {
        if modern && !has_request_id {
            return Err("Owned subscriptions require a requestId".to_owned());
        }
        if modern && requested_id.is_some() {
            return Err("Subscription IDs are assigned by the host".to_owned());
        }
        let mut subscriptions = lock(&self.shared.subscriptions);
        // COMPAT(ownedSubscriptions): a legacy socket holds one subscription
        // per slot; a new one releases the prior.
        if !modern {
            subscriptions.retain(|subscription| subscription.source != source);
        }
        let id = random_uuid();
        let response_id = if modern {
            id.clone()
        } else {
            requested_id
                .map(str::trim)
                .filter(|requested| !requested.is_empty())
                .map_or_else(|| id.clone(), str::to_owned)
        };
        subscriptions.push(Subscription {
            id: id.clone(),
            response_id: response_id.clone(),
            source,
            modern,
            filter,
            bootstrapping: true,
            pending: Vec::new(),
        });
        Ok(Owner { id, response_id })
    }

    /// `flushBootstrapped(subscriptionId, { snapshotUpdatedAtByAgentId })`.
    pub fn flush_bootstrapped(&self, id: &str, snapshot_updated_at: &HashMap<String, i64>) {
        let mut subscriptions = lock(&self.shared.subscriptions);
        let Some(subscription) = subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == id && subscription.bootstrapping)
        else {
            return;
        };
        subscription.bootstrapping = false;
        for (agent_id, payload) in std::mem::take(&mut subscription.pending) {
            if payload.get("kind").and_then(JsValue::as_str) == Some("upsert")
                && let Some(snapshot_at) = snapshot_updated_at.get(&agent_id)
                && payload
                    .get("agent")
                    .and_then(|agent| agent.get("updatedAt"))
                    .and_then(JsValue::as_str)
                    .and_then(date_parse)
                    .is_some_and(|updated_at| updated_at < *snapshot_at)
            {
                continue;
            }
            self.shared.buffer_or_emit(subscription, payload);
        }
    }

    /// Registers a `wait_for_finish` on `agent_id`, running now or not; hold
    /// the guard until the reply is sent.
    #[must_use]
    pub fn begin_wait(&self, agent_id: &str, active: bool) -> WaitGuard {
        self.waits.begin(agent_id, active)
    }

    /// `owner.release()`: `clearSubscription(id)`.
    pub fn clear(&self, id: &str) {
        lock(&self.shared.subscriptions).retain(|subscription| subscription.id != id);
    }

    /// `delivery.detach(socket)` for this family.
    pub fn detach(&self, source: SocketId) {
        lock(&self.shared.subscriptions).retain(|subscription| subscription.source != source);
    }

    /// `dispose()`.
    pub fn dispose(&self) {
        lock(&self.shared.subscriptions).clear();
    }

    /// `forwardLiveAgent(agent)`, called inside the state change's dispatch.
    /// The waits this change wakes are found now, so the update is held
    /// behind exactly their replies; it is queued only while someone
    /// observes, as the workspace update the baseline queues otherwise has
    /// no observer here.
    pub fn forward_live_agent(&self, agent: &ManagedAgentSnapshot) {
        let holds = self.waits.dispatch(&dispatched(agent));
        if !lock(&self.shared.subscriptions).is_empty() {
            self.tails.submit(
                &agent.id,
                Job {
                    item: Some(agent.clone()),
                    holds,
                    done: None,
                },
            );
        }
    }

    /// `await forwardLiveAgent(agent)`: returns once the update is out.
    pub async fn forward_live_agent_and_wait(&self, agent: &ManagedAgentSnapshot) {
        let holds = self.waits.dispatch(&dispatched(agent));
        if lock(&self.shared.subscriptions).is_empty() {
            return;
        }
        let (done, published) = oneshot::channel();
        self.tails.submit(
            &agent.id,
            Job {
                item: Some(agent.clone()),
                holds,
                done: Some(done),
            },
        );
        let _ = published.await;
    }

    /// Returns once every update queued for `agent_id` so far is out, so a
    /// reply that follows the agent's state changes goes after their updates.
    pub async fn flush(&self, agent_id: &str) {
        if !lock(&self.shared.subscriptions).is_empty() {
            self.tails.flush(agent_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::Value;
    use spocky_contracts::js_value::{JsObject, parse};
    use spocky_contracts::ws::DaemonPermission;
    use spocky_daemon::session_api::{SessionSink, SocketId};

    use super::{AgentUpdates, Shared};
    use crate::authorization::SessionAuthorization;
    use crate::reply_window::{Tails, Waits};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(SocketId, String)>>);

    impl SessionSink for Recorder {
        fn send_to_connection(&self, _message: &Value) {}
        fn send_to_source(&self, source: SocketId, message: &Value) {
            self.0.lock().unwrap().push((source, message.to_string()));
        }
        fn buffered_amount(&self, _source: Option<SocketId>) -> Option<usize> {
            None
        }
    }

    fn updates(sink: &Arc<Recorder>) -> AgentUpdates {
        let sink: Arc<dyn SessionSink> = Arc::clone(sink) as Arc<dyn SessionSink>;
        AgentUpdates {
            shared: Arc::new(Shared {
                subscriptions: Mutex::new(Vec::new()),
                sink,
                authorization: Arc::new(SessionAuthorization::new(&DaemonPermission::ALL)),
                provider_visible: Arc::new(|provider| provider != "hidden"),
            }),
            tails: Tails::new(
                tokio::runtime::Handle::current(),
                Duration::from_secs(1),
                Arc::new(|_| Box::pin(async {})),
            ),
            waits: Arc::new(Waits::default()),
        }
    }

    fn upsert(id: &str, provider: &str, updated_at: &str) -> JsObject {
        let text = format!(
            r#"{{"kind":"upsert","agent":{{"id":"{id}","provider":"{provider}","updatedAt":"{updated_at}"}},"project":{{}}}}"#
        );
        parse(&text).unwrap().as_object().unwrap().clone()
    }

    fn publish(updates: &AgentUpdates, payload: &JsObject) {
        let mut subscriptions = updates.shared.subscriptions.lock().unwrap();
        for subscription in subscriptions.iter_mut() {
            updates.shared.buffer_or_emit(subscription, payload.clone());
        }
    }

    #[tokio::test]
    async fn modern_sockets_get_host_ids_and_legacy_ids_are_honored() {
        let sink = Arc::new(Recorder::default());
        let updates = updates(&sink);
        assert_eq!(
            updates.begin(1, true, false, None, None).err().as_deref(),
            Some("Owned subscriptions require a requestId")
        );
        assert_eq!(
            updates
                .begin(1, true, true, Some("mine"), None)
                .err()
                .as_deref(),
            Some("Subscription IDs are assigned by the host")
        );
        let modern = updates.begin(1, true, true, None, None).unwrap();
        assert_eq!(modern.response_id, modern.id);
        let legacy = updates.begin(2, false, true, Some(" mine "), None).unwrap();
        assert_eq!(legacy.response_id, "mine");
        let blank = updates.begin(2, false, true, Some("  "), None).unwrap();
        assert_eq!(blank.response_id, blank.id);
        assert_eq!(updates.shared.subscriptions.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn bootstrapped_updates_flush_in_first_arrival_order_skipping_stale_upserts() {
        let sink = Arc::new(Recorder::default());
        let updates = updates(&sink);
        let owner = updates.begin(7, true, true, None, None).unwrap();
        publish(&updates, &upsert("a", "codex", "2026-01-01T00:00:01.000Z"));
        publish(&updates, &upsert("b", "codex", "2026-01-01T00:00:00.000Z"));
        publish(&updates, &upsert("a", "codex", "2026-01-01T00:00:03.000Z"));
        publish(&updates, &upsert("c", "hidden", "2026-01-01T00:00:00.000Z"));
        assert!(sink.0.lock().unwrap().is_empty());
        let snapshot = HashMap::from([("b".to_owned(), 1_767_225_601_000_i64)]);
        updates.flush_bootstrapped(&owner.id, &snapshot);
        publish(&updates, &upsert("b", "codex", "2026-01-01T00:00:05.000Z"));
        let frames = sink.0.lock().unwrap().clone();
        let id = &owner.response_id;
        assert_eq!(
            frames,
            [
                (
                    7,
                    format!(
                        r#"{{"type":"agent_update","payload":{{"kind":"upsert","agent":{{"id":"a","provider":"codex","updatedAt":"2026-01-01T00:00:03.000Z"}},"project":{{}},"subscriptionId":"{id}"}}}}"#
                    )
                ),
                (
                    7,
                    format!(
                        r#"{{"type":"agent_update","payload":{{"kind":"upsert","agent":{{"id":"b","provider":"codex","updatedAt":"2026-01-01T00:00:05.000Z"}},"project":{{}},"subscriptionId":"{id}"}}}}"#
                    )
                ),
            ]
        );
    }

    #[tokio::test]
    async fn pending_updates_are_compared_as_date_parse_reads_them() {
        let sink = Arc::new(Recorder::default());
        let updates = updates(&sink);
        let owner = updates.begin(5, true, true, None, None).unwrap();
        // Printed by node 22: Date.parse("Fri, 02 Oct 2026 12:00:00 GMT") is
        // 1790942400000 and "... 12:00:01 GMT" is 1790942401000.
        publish(
            &updates,
            &upsert("a", "codex", "Fri, 02 Oct 2026 12:00:00 GMT"),
        );
        publish(
            &updates,
            &upsert("a", "codex", "Fri, 02 Oct 2026 12:00:01 GMT"),
        );
        let snapshot = HashMap::from([("a".to_owned(), 1_790_942_400_500_i64)]);
        updates.flush_bootstrapped(&owner.id, &snapshot);
        let frames = sink.0.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "{frames:?}");
        assert!(frames[0].1.contains("12:00:01 GMT"), "{frames:?}");
    }

    #[tokio::test]
    async fn legacy_payloads_carry_no_subscription_id_and_detach_stops_delivery() {
        let sink = Arc::new(Recorder::default());
        let updates = updates(&sink);
        let owner = updates.begin(3, false, true, None, None).unwrap();
        updates.flush_bootstrapped(&owner.id, &HashMap::new());
        publish(&updates, &upsert("a", "codex", "2026-01-01T00:00:00.000Z"));
        updates.detach(3);
        publish(&updates, &upsert("a", "codex", "2026-01-01T00:00:09.000Z"));
        let frames = sink.0.lock().unwrap().clone();
        assert_eq!(
            frames,
            [(
                3,
                r#"{"type":"agent_update","payload":{"kind":"upsert","agent":{"id":"a","provider":"codex","updatedAt":"2026-01-01T00:00:00.000Z"},"project":{}}}"#
                    .to_owned()
            )]
        );
    }
}
