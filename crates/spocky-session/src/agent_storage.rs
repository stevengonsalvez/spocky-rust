//! `AgentStorage` from pinned Paseo `agent/agent-storage.ts`: the agent
//! record files behind the manager, with writes queued per agent.
//!
//! Records live in [`AgentRecordStore`]. Each agent's mutations form one
//! chain, as the baseline's `pendingWrites` map chains
//! `prev.then(...)` with no rejection handler: a mutation runs once the
//! previous one has settled, and when the previous one failed it fails with
//! that same error without running. The chain entry is dropped when its
//! last link settles, so a mutation queued after that starts fresh.
//! Different agents never wait for each other.
//!
//! A snapshot reads the live agent when its turn comes, not when it is
//! queued, as the baseline's queued closure reads the agent object then.
//! Links run as spawned tasks, so a dropped caller does not cancel a queued
//! write, as a JS promise is not cancelled. File work runs on the blocking
//! pool.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use spocky_store::StoreError;
use spocky_store::agent_record::AgentRecordStore;
use spocky_store::js_value::JsValue;
use tokio::sync::watch;

use crate::agent_projection::{ManagedAgentRecordView, SnapshotOverrides, apply_snapshot_record};
use crate::timeline::JsTypeError;

/// Why a storage call failed. Cloned to every mutation that short-circuits
/// on it, as the baseline hands them the same rejection.
#[derive(Debug, Clone)]
pub enum StorageError {
    /// `toStoredAgentRecord` threw.
    Projection(JsTypeError),
    /// Writing the record file failed.
    Store(Arc<StoreError>),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Projection(error) => error.fmt(formatter),
            Self::Store(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StorageError {}

/// A link's outcome: `None` until it settles.
type Settled = Option<Result<(), StorageError>>;

/// The `tracked` promise of the newest link in one agent's chain.
struct Link {
    id: u64,
    settled: watch::Receiver<Settled>,
}

struct Inner {
    store: Mutex<AgentRecordStore>,
    loaded: AtomicBool,
    pending_writes: Mutex<HashMap<String, Link>>,
    next_link: AtomicU64,
}

/// `AgentStorage`.
#[derive(Clone)]
pub struct AgentStorage {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Waits for a link to settle and returns its outcome.
async fn settled(mut link: watch::Receiver<Settled>) -> Result<(), StorageError> {
    // The sender only drops unsettled when the link task panicked, and that
    // panic already reaches the link's own caller.
    let outcome = link.wait_for(Option::is_some).await.map_or_else(
        |_| panic!("an earlier agent record write panicked"),
        |outcome| outcome.clone(),
    );
    outcome.unwrap_or(Ok(()))
}

impl AgentStorage {
    /// `new AgentStorage(baseDir)`.
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store: Mutex::new(AgentRecordStore::new(base_dir)),
                loaded: AtomicBool::new(false),
                pending_writes: Mutex::new(HashMap::new()),
                next_link: AtomicU64::new(0),
            }),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Inner) -> T + Send + 'static,
    ) -> T {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || work(&inner))
            .await
            .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
    }

    /// `initialize()`: loads the record files once. Once loaded it returns
    /// without suspending, as the baseline's `load()` does no I/O then, so
    /// calls made together queue in call order.
    pub async fn initialize(&self) {
        if self.inner.loaded.load(Ordering::Acquire) {
            return;
        }
        self.blocking(|inner| lock(&inner.store).initialize()).await;
        self.inner.loaded.store(true, Ordering::Release);
    }

    /// `list()`.
    pub async fn list(&self) -> Vec<JsValue> {
        self.initialize().await;
        self.blocking(|inner| lock(&inner.store).list()).await
    }

    /// `get(agentId)`.
    pub async fn get(&self, agent_id: &str) -> Option<JsValue> {
        self.initialize().await;
        let agent_id = agent_id.to_owned();
        self.blocking(move |inner| lock(&inner.store).get(&agent_id))
            .await
    }

    /// The newest link of `agent_id`'s chain, if one is pending.
    fn tail(&self, agent_id: &str) -> Option<watch::Receiver<Settled>> {
        lock(&self.inner.pending_writes)
            .get(agent_id)
            .map(|link| link.settled.clone())
    }

    /// `queueRecordMutation`: chains `mutate` behind `agent_id`'s pending
    /// writes. When its turn comes it is skipped if a delete has begun;
    /// otherwise it builds the record from the existing one and writes it.
    ///
    /// The link joins the chain when this is called, as the baseline's
    /// calls queue in call order, and runs whether or not the returned
    /// future is polled.
    fn queue_record_mutation<M>(
        &self,
        agent_id: &str,
        mutate: M,
    ) -> impl Future<Output = Result<(), StorageError>> + Send + 'static + use<M>
    where
        M: FnOnce(Option<&JsValue>) -> Result<JsValue, StorageError> + Send + 'static,
    {
        let id = self.inner.next_link.fetch_add(1, Ordering::Relaxed);
        let (settle, settled_link) = watch::channel(None);
        let prev = lock(&self.inner.pending_writes)
            .insert(
                agent_id.to_owned(),
                Link {
                    id,
                    settled: settled_link,
                },
            )
            .map(|link| link.settled);
        let storage = self.clone();
        let agent_id = agent_id.to_owned();
        let link = tokio::spawn(async move {
            storage.initialize().await;
            let outcome = match prev {
                Some(prev) => settled(prev).await,
                None => Ok(()),
            };
            let outcome = match outcome {
                Ok(()) => storage.run_mutation(agent_id.clone(), mutate).await,
                Err(error) => Err(error),
            };
            {
                let mut pending = lock(&storage.inner.pending_writes);
                if pending.get(&agent_id).is_some_and(|link| link.id == id) {
                    pending.remove(&agent_id);
                }
            }
            settle.send_replace(Some(outcome.clone()));
            outcome
        });
        async move {
            link.await
                .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
        }
    }

    /// The body of one link: the delete check and the projection run
    /// together, then the record is written. The store lock is not held
    /// while `mutate` reads the live agent.
    async fn run_mutation(
        &self,
        agent_id: String,
        mutate: impl FnOnce(Option<&JsValue>) -> Result<JsValue, StorageError> + Send + 'static,
    ) -> Result<(), StorageError> {
        self.blocking(move |inner| {
            let existing = {
                let mut store = lock(&inner.store);
                if store.is_deleting(&agent_id) {
                    return Ok(());
                }
                store.get(&agent_id)
            };
            let record = mutate(existing.as_ref())?;
            lock(&inner.store)
                .write_record(record)
                .map(|_| ())
                .map_err(|error| StorageError::Store(Arc::new(error)))
        })
        .await
    }

    /// `upsert(record)`: queued when called.
    ///
    /// # Errors
    ///
    /// The future returns the write failure, or the failure of an earlier
    /// write of the same agent that was still pending when this one was
    /// queued.
    pub fn upsert(
        &self,
        record: JsValue,
    ) -> impl Future<Output = Result<(), StorageError>> + Send + 'static + use<> {
        let agent_id = record
            .get("id")
            .and_then(JsValue::as_str)
            .unwrap_or_default()
            .to_owned();
        self.queue_record_mutation(&agent_id, move |_| Ok(record))
    }

    /// `applySnapshot(agent, options)`: queued when called; `agent` is read
    /// when the write's turn comes.
    ///
    /// # Errors
    ///
    /// The future returns the projection's `TypeError` or the write failure,
    /// or the failure of an earlier write of the same agent that was still
    /// pending when this one was queued.
    pub fn apply_snapshot<A>(
        &self,
        agent_id: &str,
        agent: A,
        overrides: SnapshotOverrides,
    ) -> impl Future<Output = Result<(), StorageError>> + Send + 'static + use<A>
    where
        A: FnOnce() -> ManagedAgentRecordView + Send + 'static,
    {
        self.queue_record_mutation(agent_id, move |existing| {
            apply_snapshot_record(&agent(), existing, &overrides).map_err(StorageError::Projection)
        })
    }

    /// `beginDelete(agentId)`.
    pub fn begin_delete(&self, agent_id: &str) {
        lock(&self.inner.store).begin_delete(agent_id);
    }

    /// `remove(agentId)`: marks the agent deleting and takes its pending
    /// writes when called, then waits for them and unlinks the files.
    /// Unlink failures other than not-found come back for the caller to log.
    ///
    /// # Errors
    ///
    /// The future returns the failure of the pending write it waited for;
    /// the files and the cached record then stay, as the baseline's `await`
    /// throws before the unlinks.
    pub fn remove(
        &self,
        agent_id: &str,
    ) -> impl Future<Output = Result<Vec<(PathBuf, io::Error)>, StorageError>> + Send + 'static + use<>
    {
        self.begin_delete(agent_id);
        let tail = self.tail(agent_id);
        let storage = self.clone();
        let agent_id = agent_id.to_owned();
        async move {
            storage.initialize().await;
            if let Some(tail) = tail {
                settled(tail).await?;
            }
            Ok(storage
                .blocking(move |inner| lock(&inner.store).remove(&agent_id))
                .await)
        }
    }

    /// `flush()`: loads, then resolves once every write pending when it
    /// was called has settled, failed or not.
    pub async fn flush(&self) {
        self.initialize().await;
        let tails: Vec<_> = lock(&self.inner.pending_writes)
            .values()
            .map(|link| link.settled.clone())
            .collect();
        for tail in tails {
            let _ = settled(tail).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use spocky_store::js_value::{JsValue, parse};

    use super::{AgentStorage, StorageError};
    use crate::agent_projection::{AgentAttention, ManagedAgentRecordView, SnapshotOverrides};

    struct Home(PathBuf);

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home(name: &str) -> Home {
        let path = std::env::temp_dir().join(format!(
            "spocky-agent-storage-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Home(path)
    }

    fn view(lifecycle: &str, config: &str) -> ManagedAgentRecordView {
        ManagedAgentRecordView {
            id: "a1".to_owned(),
            provider: "codex".to_owned(),
            cwd: "/w".to_owned(),
            workspace_id: None,
            created_at_millis: 1_700_000_000_000,
            updated_at_millis: 1_700_000_001_000,
            last_user_message_at_millis: None,
            labels: parse("{}").expect("labels"),
            lifecycle: lifecycle.to_owned(),
            current_mode_id: None,
            config: parse(config).expect("config"),
            runtime_info: None,
            features: None,
            persistence: None,
            last_error: None,
            attention: AgentAttention::None,
            internal: Some(false),
            owner: None,
        }
    }

    fn status(record: Option<JsValue>) -> Option<String> {
        record
            .and_then(|record| record.get("lastStatus").cloned())
            .and_then(|status| status.as_str().map(str::to_owned))
    }

    #[tokio::test]
    async fn snapshots_write_in_order_and_stop_after_delete() {
        let home = home("order");
        let storage = AgentStorage::new(&home.0);
        let config = r#"{"provider":"codex","cwd":"/w"}"#;
        storage
            .apply_snapshot(
                "a1",
                move || view("initializing", config),
                SnapshotOverrides::default(),
            )
            .await
            .expect("first snapshot");
        storage
            .apply_snapshot(
                "a1",
                move || view("idle", config),
                SnapshotOverrides {
                    title: Some(Some("T".to_owned())),
                    internal: None,
                },
            )
            .await
            .expect("second snapshot");
        storage.flush().await;
        let record = storage.get("a1").await;
        assert_eq!(status(record.clone()), Some("idle".to_owned()));
        assert_eq!(
            record.and_then(|record| record.get("title").cloned()),
            Some(JsValue::String("T".to_owned()))
        );
        // A fresh store reads the file the snapshots wrote.
        let reopened = AgentStorage::new(&home.0);
        assert_eq!(status(reopened.get("a1").await), Some("idle".to_owned()));
        assert_eq!(reopened.list().await.len(), 1);

        assert!(storage.remove("a1").await.expect("remove").is_empty());
        storage
            .apply_snapshot(
                "a1",
                || unreachable!("a deleting agent is never projected"),
                SnapshotOverrides::default(),
            )
            .await
            .expect("skipped while deleting");
        assert_eq!(storage.get("a1").await, None);
    }

    #[tokio::test]
    async fn projection_errors_reach_the_caller() {
        let home = home("error");
        let storage = AgentStorage::new(&home.0);
        let config = r#"{"provider":"codex","cwd":"/w","toolPolicy":{}}"#;
        let error = storage
            .apply_snapshot(
                "a1",
                move || view("idle", config),
                SnapshotOverrides::default(),
            )
            .await
            .expect_err("missing preapproved throws");
        assert!(matches!(error, StorageError::Projection(_)));
        assert_eq!(
            error.to_string(),
            "Cannot read properties of undefined (reading 'map')"
        );
        assert_eq!(storage.get("a1").await, None);
    }

    /// Lets a test hold a snapshot's projection: `entered` fires once the
    /// projection has started, and it returns once `open` is sent.
    struct Gate {
        entered: mpsc::Receiver<()>,
        open: mpsc::Sender<()>,
    }

    fn gated(
        id: &'static str,
        config: &'static str,
    ) -> (Gate, impl FnOnce() -> ManagedAgentRecordView + Send) {
        let (entered_tx, entered) = mpsc::channel();
        let (open, gate) = mpsc::channel::<()>();
        let agent = move || {
            entered_tx.send(()).expect("entered");
            gate.recv().expect("gate opened");
            ManagedAgentRecordView {
                id: id.to_owned(),
                ..view("idle", config)
            }
        };
        (Gate { entered, open }, agent)
    }

    impl Gate {
        async fn wait_entered(self) -> mpsc::Sender<()> {
            let Self { entered, open } = self;
            tokio::task::spawn_blocking(move || entered.recv())
                .await
                .expect("join")
                .expect("projection started");
            open
        }
    }

    const FAILING: &str = r#"{"provider":"codex","cwd":"/w","toolPolicy":{}}"#;
    const MAP_ERROR: &str = "Cannot read properties of undefined (reading 'map')";

    fn record(id: &str, title: &str) -> JsValue {
        parse(&format!(
            r#"{{"id":"{id}","provider":"codex","cwd":"/w","title":"{title}"}}"#
        ))
        .expect("record")
    }

    #[tokio::test]
    async fn queued_writes_share_an_earlier_failure_until_the_chain_drains() {
        let home = home("chain");
        let storage = AgentStorage::new(&home.0);
        storage.initialize().await;
        let (gate, agent) = gated("a1", FAILING);
        let snapshot = storage.apply_snapshot("a1", agent, SnapshotOverrides::default());
        let upsert = storage.upsert(record("a1", "queued"));
        gate.wait_entered().await.send(()).expect("open");
        assert_eq!(
            snapshot.await.expect_err("projection").to_string(),
            MAP_ERROR
        );
        // The upsert never ran: it fails with the snapshot's error.
        assert_eq!(
            upsert.await.expect_err("short-circuit").to_string(),
            MAP_ERROR
        );
        assert_eq!(storage.get("a1").await, None);
        // The chain has drained, so the next write starts fresh.
        storage
            .upsert(record("a1", "fresh"))
            .await
            .expect("fresh write");
        assert_eq!(
            storage
                .get("a1")
                .await
                .and_then(|record| record.get("title").cloned()),
            Some(JsValue::String("fresh".to_owned()))
        );
    }

    #[tokio::test]
    async fn remove_fails_with_the_pending_write_and_keeps_the_record() {
        let home = home("remove-chain");
        let storage = AgentStorage::new(&home.0);
        storage
            .upsert(record("a1", "kept"))
            .await
            .expect("first write");
        let (gate, agent) = gated("a1", FAILING);
        let snapshot = storage.apply_snapshot("a1", agent, SnapshotOverrides::default());
        let open = gate.wait_entered().await;
        let remove = storage.remove("a1");
        open.send(()).expect("open");
        assert_eq!(
            snapshot.await.expect_err("projection").to_string(),
            MAP_ERROR
        );
        assert_eq!(remove.await.expect_err("remove").to_string(), MAP_ERROR);
        // The throw came before the unlinks: the record and its file stay.
        assert_eq!(
            storage
                .get("a1")
                .await
                .and_then(|record| record.get("title").cloned()),
            Some(JsValue::String("kept".to_owned()))
        );
        let project = std::fs::read_dir(&home.0)
            .expect("home")
            .next()
            .expect("project directory")
            .expect("entry")
            .path();
        assert!(project.join("a1.json").is_file());
        // The delete has begun, so later writes are skipped.
        storage.upsert(record("a1", "late")).await.expect("skipped");
        assert_eq!(
            storage
                .get("a1")
                .await
                .and_then(|record| record.get("title").cloned()),
            Some(JsValue::String("kept".to_owned()))
        );
    }

    #[tokio::test]
    async fn remove_does_not_wait_for_other_agents() {
        let home = home("per-agent");
        let storage = AgentStorage::new(&home.0);
        storage.upsert(record("a1", "a")).await.expect("a1 write");
        let (gate, agent) = gated("b1", r#"{"provider":"codex","cwd":"/w"}"#);
        let other = storage.apply_snapshot("b1", agent, SnapshotOverrides::default());
        let open = gate.wait_entered().await;
        // b1's write is held open; removing a1 still finishes.
        let removed = tokio::time::timeout(Duration::from_secs(30), storage.remove("a1"))
            .await
            .expect("remove(a1) waited for b1");
        assert!(removed.expect("remove").is_empty());
        assert_eq!(storage.get("a1").await, None);
        open.send(()).expect("open");
        other.await.expect("b1 snapshot");
        storage.flush().await;
        assert_eq!(status(storage.get("b1").await), Some("idle".to_owned()));
    }
}
