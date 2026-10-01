//! `AgentStorage` from pinned Paseo `agent/agent-storage.ts`: the agent
//! record files behind the manager, with writes queued in call order.
//!
//! Records live in [`AgentRecordStore`]. Every mutation waits its turn on
//! one first-in first-out queue (the baseline queues per agent; one queue
//! keeps each agent's order and only serializes different agents' writes).
//! A snapshot reads the live agent when its turn comes, not when it is
//! queued, as the baseline's queued closure reads the agent object then.
//! File work runs on the blocking pool.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use spocky_store::StoreError;
use spocky_store::agent_record::AgentRecordStore;
use spocky_store::js_value::JsValue;

use crate::agent_projection::{ManagedAgentRecordView, SnapshotOverrides, apply_snapshot_record};
use crate::timeline::JsTypeError;

/// Why a storage call failed.
#[derive(Debug)]
pub enum StorageError {
    /// `toStoredAgentRecord` threw.
    Projection(JsTypeError),
    /// Writing the record file failed.
    Store(StoreError),
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

struct Inner {
    store: Mutex<AgentRecordStore>,
    queue: tokio::sync::Mutex<()>,
}

/// `AgentStorage`.
#[derive(Clone)]
pub struct AgentStorage {
    inner: Arc<Inner>,
}

fn lock(store: &Mutex<AgentRecordStore>) -> MutexGuard<'_, AgentRecordStore> {
    store.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AgentStorage {
    /// `new AgentStorage(baseDir)`.
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store: Mutex::new(AgentRecordStore::new(base_dir)),
                queue: tokio::sync::Mutex::new(()),
            }),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut AgentRecordStore) -> T + Send + 'static,
    ) -> T {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || work(&mut lock(&inner.store)))
            .await
            .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
    }

    /// `initialize()`: loads the record files once.
    pub async fn initialize(&self) {
        self.blocking(AgentRecordStore::initialize).await;
    }

    /// `list()`.
    pub async fn list(&self) -> Vec<JsValue> {
        self.blocking(AgentRecordStore::list).await
    }

    /// `get(agentId)`.
    pub async fn get(&self, agent_id: &str) -> Option<JsValue> {
        let agent_id = agent_id.to_owned();
        self.blocking(move |store| store.get(&agent_id)).await
    }

    /// Runs `mutate` on the existing record when its queue turn comes and
    /// writes the result, unless a delete of `agent_id` has begun.
    async fn queue_record_mutation(
        &self,
        agent_id: &str,
        mutate: impl FnOnce(Option<&JsValue>) -> Result<JsValue, StorageError> + Send + 'static,
    ) -> Result<(), StorageError> {
        self.initialize().await;
        let _turn = self.inner.queue.lock().await;
        let agent_id = agent_id.to_owned();
        self.blocking(move |store| {
            if store.is_deleting(&agent_id) {
                return Ok(());
            }
            let existing = store.get(&agent_id);
            let record = mutate(existing.as_ref())?;
            store.write(record).map(|_| ()).map_err(StorageError::Store)
        })
        .await
    }

    /// `upsert(record)`.
    ///
    /// # Errors
    ///
    /// Returns the write failure.
    pub async fn upsert(&self, record: JsValue) -> Result<(), StorageError> {
        let agent_id = record
            .get("id")
            .and_then(JsValue::as_str)
            .unwrap_or_default()
            .to_owned();
        self.queue_record_mutation(&agent_id, move |_| Ok(record))
            .await
    }

    /// `applySnapshot(agent, options)`: `agent` is read when the write's turn
    /// comes.
    ///
    /// # Errors
    ///
    /// Returns the projection's `TypeError` or the write failure.
    pub async fn apply_snapshot(
        &self,
        agent_id: &str,
        agent: impl FnOnce() -> ManagedAgentRecordView + Send + 'static,
        overrides: SnapshotOverrides,
    ) -> Result<(), StorageError> {
        self.queue_record_mutation(agent_id, move |existing| {
            apply_snapshot_record(&agent(), existing, &overrides).map_err(StorageError::Projection)
        })
        .await
    }

    /// `beginDelete(agentId)`.
    pub fn begin_delete(&self, agent_id: &str) {
        lock(&self.inner.store).begin_delete(agent_id);
    }

    /// `remove(agentId)`: waits for queued writes, then unlinks the files.
    /// Unlink failures other than not-found come back for the caller to log.
    pub async fn remove(&self, agent_id: &str) -> Vec<(PathBuf, io::Error)> {
        self.initialize().await;
        self.begin_delete(agent_id);
        let _turn = self.inner.queue.lock().await;
        let agent_id = agent_id.to_owned();
        self.blocking(move |store| store.remove(&agent_id)).await
    }

    /// `flush()`: resolves once every write queued before it has settled.
    pub async fn flush(&self) {
        drop(self.inner.queue.lock().await);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

        assert!(storage.remove("a1").await.is_empty());
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
}
