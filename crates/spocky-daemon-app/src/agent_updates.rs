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
use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::request::AgentDirectoryFilter;
use spocky_daemon::session_api::{SessionSink, SocketId};
use spocky_session::agent_manager::ManagedAgentSnapshot;
use spocky_session::clock::random_uuid;
use spocky_store::time::parse_iso_millis;
use tokio::sync::{mpsc, oneshot};

use crate::agent_directory::matches_agent_updates_filter;
use crate::authorization::SessionAuthorization;
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

/// Which `wait_for_finish` requests are in flight per agent. Pinned
/// `session.ts` emits a wait's reply within a few microtasks of the state
/// change that settles it, while that change's `agent_update` goes through
/// storage, placement and registry awaits, so the reply always comes first.
/// Rust's scheduling carries no such ordering, so an update that would
/// settle a wait is held until the waits on its agent have replied.
#[derive(Default)]
struct ReplyBarrier {
    waits: Mutex<HashMap<String, usize>>,
    replied: tokio::sync::Notify,
}

/// How long a settling update waits for a reply that may not be coming (a
/// wait that this change does not wake).
// ponytail: fixed bound; a wait that the change does not settle delays the
// update by up to this long. Tie the hold to the wait's own wake if that bites.
const REPLY_HOLD: std::time::Duration = std::time::Duration::from_secs(2);

impl ReplyBarrier {
    fn begin(&self, agent_id: &str) {
        *lock(&self.waits).entry(agent_id.to_owned()).or_default() += 1;
    }

    fn end(&self, agent_id: &str) {
        let mut waits = lock(&self.waits);
        if let Some(count) = waits.get_mut(agent_id) {
            *count -= 1;
            if *count == 0 {
                waits.remove(agent_id);
            }
        }
        drop(waits);
        self.replied.notify_waiters();
    }

    fn pending(&self, agent_id: &str) -> bool {
        lock(&self.waits).contains_key(agent_id)
    }

    /// Returns once no wait on `agent_id` is in flight, or after `bound`.
    async fn hold(&self, agent_id: &str, bound: std::time::Duration) {
        let held = async {
            loop {
                let released = self.replied.notified();
                if !self.pending(agent_id) {
                    return;
                }
                released.await;
            }
        };
        let _ = tokio::time::timeout(bound, held).await;
    }
}

/// A `wait_for_finish` in flight; dropping it is its reply being out.
pub struct WaitGuard {
    barrier: Arc<ReplyBarrier>,
    agent_id: String,
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.barrier.end(&self.agent_id);
    }
}

/// Whether an agent payload is a state that settles a `wait_for_finish`:
/// idle, error, or waiting on a permission.
fn settles_a_wait(payload: &JsValue) -> bool {
    let status = payload.get("status").and_then(JsValue::as_str);
    let permissions = payload
        .get("pendingPermissions")
        .and_then(JsValue::as_array)
        .is_some_and(|permissions| !permissions.is_empty());
    matches!(status, Some("idle" | "error")) || permissions
}

struct Shared {
    barrier: Arc<ReplyBarrier>,
    subscriptions: Mutex<Vec<Subscription>>,
    sink: Arc<dyn SessionSink>,
    authorization: Arc<SessionAuthorization>,
    provider_visible: ProviderVisible,
}

/// The session's `AgentUpdatesService`.
pub struct AgentUpdates {
    shared: Arc<Shared>,
    /// `liveAgentUpdateTails`: one worker drains updates in arrival order.
    // ponytail: one queue for every agent, where the baseline chains per
    // agent; split by agent id if one slow enrichment must not delay others.
    queue: mpsc::UnboundedSender<Queued>,
}

/// A live agent to publish (none for a flush marker), and who waits for it.
type Queued = (Option<ManagedAgentSnapshot>, Option<oneshot::Sender<()>>);

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
        if settles_a_wait(&payload) && self.barrier.pending(&agent.id) {
            self.barrier.hold(&agent.id, REPLY_HOLD).await;
        }
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
            barrier: Arc::new(ReplyBarrier::default()),
            subscriptions: Mutex::new(Vec::new()),
            sink,
            authorization,
            provider_visible,
        });
        let (queue, mut updates) = mpsc::unbounded_channel::<Queued>();
        let worker = Arc::clone(&shared);
        let worker_services = Arc::clone(services);
        services.runtime.spawn(async move {
            while let Some((agent, done)) = updates.recv().await {
                if let Some(agent) = agent {
                    worker.publish(&worker_services, &agent).await;
                }
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
        });
        Self { shared, queue }
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
                    .and_then(parse_iso_millis)
                    .is_some_and(|updated_at| updated_at < *snapshot_at)
            {
                continue;
            }
            self.shared.buffer_or_emit(subscription, payload);
        }
    }

    /// Registers a `wait_for_finish` on `agent_id`; hold it until the reply
    /// is sent.
    #[must_use]
    pub fn begin_wait(&self, agent_id: &str) -> WaitGuard {
        self.shared.barrier.begin(agent_id);
        WaitGuard {
            barrier: Arc::clone(&self.shared.barrier),
            agent_id: agent_id.to_owned(),
        }
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

    /// `forwardLiveAgent(agent)`: queued only while someone observes, as the
    /// workspace update the baseline queues otherwise has no observer here.
    pub fn forward_live_agent(&self, agent: &ManagedAgentSnapshot) {
        if !lock(&self.shared.subscriptions).is_empty() {
            let _ = self.queue.send((Some(agent.clone()), None));
        }
    }

    /// `await forwardLiveAgent(agent)`: returns once the update is out.
    pub async fn forward_live_agent_and_wait(&self, agent: &ManagedAgentSnapshot) {
        if lock(&self.shared.subscriptions).is_empty() {
            return;
        }
        let (done, published) = oneshot::channel();
        if self.queue.send((Some(agent.clone()), Some(done))).is_ok() {
            let _ = published.await;
        }
    }

    /// Returns once every update queued so far is out. The baseline
    /// publishes a live update within the microtasks that follow the
    /// agent's state change, before a request handler awaiting the same
    /// change replies; a reply that waits here keeps that order.
    pub async fn flush(&self) {
        if lock(&self.shared.subscriptions).is_empty() {
            return;
        }
        let (done, flushed) = oneshot::channel();
        if self.queue.send((None, Some(done))).is_ok() {
            let _ = flushed.await;
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
    use tokio::sync::mpsc;

    use super::{AgentUpdates, ReplyBarrier, Shared, settles_a_wait};
    use crate::authorization::SessionAuthorization;

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
                barrier: Arc::new(ReplyBarrier::default()),
                subscriptions: Mutex::new(Vec::new()),
                sink,
                authorization: Arc::new(SessionAuthorization::new(&DaemonPermission::ALL)),
                provider_visible: Arc::new(|provider| provider != "hidden"),
            }),
            queue: mpsc::unbounded_channel().0,
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
    async fn a_settling_update_waits_for_the_reply() {
        let barrier = Arc::new(ReplyBarrier::default());
        barrier.begin("a");
        let held = {
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move { barrier.hold("a", Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!held.is_finished());
        barrier.end("a");
        held.await.unwrap();
        assert!(!barrier.pending("a"));
    }

    #[tokio::test]
    async fn the_hold_is_bounded_and_other_agents_are_free() {
        let barrier = ReplyBarrier::default();
        barrier.begin("a");
        tokio::time::timeout(
            Duration::from_secs(5),
            barrier.hold("b", Duration::from_secs(30)),
        )
        .await
        .expect("no wait on b");
        barrier.hold("a", Duration::from_millis(30)).await;
        assert!(
            barrier.pending("a"),
            "the bound passed with the wait still open"
        );
    }

    #[test]
    fn idle_error_and_permission_states_settle_a_wait() {
        let payload = |text| parse(text).unwrap();
        assert!(settles_a_wait(&payload(
            r#"{"status":"idle","pendingPermissions":[]}"#
        )));
        assert!(settles_a_wait(&payload(r#"{"status":"error"}"#)));
        assert!(settles_a_wait(&payload(
            r#"{"status":"running","pendingPermissions":[{"id":"p"}]}"#
        )));
        assert!(!settles_a_wait(&payload(
            r#"{"status":"running","pendingPermissions":[]}"#
        )));
    }

    #[test]
    fn modern_sockets_get_host_ids_and_legacy_ids_are_honored() {
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

    #[test]
    fn bootstrapped_updates_flush_in_first_arrival_order_skipping_stale_upserts() {
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

    #[test]
    fn legacy_payloads_carry_no_subscription_id_and_detach_stops_delivery() {
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
