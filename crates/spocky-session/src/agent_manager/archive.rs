//! Archive and unarchive from pinned Paseo `agent/agent-manager.ts`
//! (`archiveAgent`, `archiveSnapshot`, `unarchiveSnapshot`,
//! `unarchiveSnapshotByHandle`, the child cascade and `detachAgent`) and
//! `agent/agent-archive.ts` (`buildArchivedAgentRecord`). Each public
//! member runs on the agent's lifecycle lane.
//!
//! The `agent.archived` plugin hook is not emitted: the manager has no
//! plugin runtime to deliver it to.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use spocky_contracts::js::{js_string, spread, truthy};
use spocky_store::js_value::{JsObject, JsValue};
use spocky_store::time::parse_iso_millis;

use super::create::touch_updated_at;
use super::{AgentLifecycle, AgentManager, AgentManagerEvent, ManagedAgentSnapshot};
use crate::agent_labels::{
    PARENT_AGENT_ID_LABEL, has_open_agent_tab, is_open_agent_tab_label, parent_agent_id_from_labels,
};
use crate::agent_projection::{AgentAttention, SnapshotOverrides};
use crate::agent_sdk::{AgentError, BoxFuture};
use crate::agent_storage::AgentStorage;
use crate::clock::{iso_from_millis, now_iso, now_millis};
use crate::persistence_hooks::extract_attention;
use crate::runtime_mcp_config::strip_internal_paseo_mcp_server;

/// `onAgentArchived(agentId)`; its failure is only logged.
pub type AgentArchivedCallback =
    Arc<dyn Fn(String) -> BoxFuture<'static, Result<(), AgentError>> + Send + Sync>;

/// `unarchiveSnapshot`'s `updates`.
#[derive(Debug, Clone, Default)]
pub struct UnarchiveUpdates {
    pub workspace_id: Option<String>,
    /// `AgentLabelPatch`: `null` removes a label.
    pub labels: Option<JsValue>,
}

/// `detachAgent`'s result.
#[derive(Debug, Clone, PartialEq)]
pub struct DetachedAgent {
    pub record: JsValue,
    pub live: bool,
    pub previous_parent_agent_id: Option<String>,
}

/// `STORED_AGENT_CAPABILITIES`.
fn stored_agent_capabilities() -> JsValue {
    let mut flags = JsObject::new();
    for (key, value) in [
        ("supportsStreaming", false),
        ("supportsSessionPersistence", true),
        ("supportsDynamicModes", false),
        ("supportsMcpServers", false),
        ("supportsReasoningStream", false),
        ("supportsToolInvocations", true),
        ("supportsRewindConversation", false),
        ("supportsRewindFiles", false),
        ("supportsRewindBoth", false),
    ] {
        flags.insert(key, JsValue::Bool(value));
    }
    JsValue::Object(flags)
}

fn is_nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined | JsValue::Null))
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `buildStoredAgentConfig(record)`.
fn build_stored_agent_config(record: &JsValue) -> JsValue {
    let mut config = JsObject::new();
    config.insert(
        "provider",
        record
            .get("provider")
            .cloned()
            .unwrap_or(JsValue::Undefined),
    );
    config.insert(
        "cwd",
        record.get("cwd").cloned().unwrap_or(JsValue::Undefined),
    );
    if let Some(stored) = record.get("config").filter(|config| truthy(Some(config))) {
        for key in [
            "modeId",
            "model",
            "thinkingOptionId",
            "featureValues",
            "providerOptions",
            "toolPolicy",
            "systemPrompt",
            "mcpServers",
        ] {
            if !is_nullish(stored.get(key)) {
                config.insert(key, stored.get(key).cloned().unwrap_or(JsValue::Undefined));
            }
        }
    }
    strip_internal_paseo_mcp_server(&JsValue::Object(config))
}

/// `buildArchivedAgentRecord(record, { archivedAt, updatedAt })`.
#[must_use]
pub fn build_archived_agent_record(
    record: &JsValue,
    archived_at: &str,
    updated_at: Option<&str>,
) -> JsValue {
    let mut out = spread(Some(record));
    out.insert("archivedAt", text(archived_at));
    out.insert(
        "updatedAt",
        updated_at.map_or_else(
            || {
                record
                    .get("updatedAt")
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            },
            text,
        ),
    );
    let status = record
        .get("lastStatus")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    out.insert(
        "lastStatus",
        match status.as_str() {
            Some("running" | "initializing") => text("idle"),
            _ => status,
        },
    );
    out.insert("requiresAttention", JsValue::Bool(false));
    out.insert("attentionReason", JsValue::Null);
    out.insert("attentionTimestamp", JsValue::Null);
    JsValue::Object(out)
}

/// `applyLabelPatch(labels, patch)`.
#[must_use]
pub fn apply_label_patch(labels: Option<&JsValue>, patch: &JsValue) -> JsValue {
    let mut out = spread(labels);
    if let JsValue::Object(patch) = patch {
        for (key, value) in patch.iter() {
            if matches!(value, JsValue::Null) {
                let mut kept = JsObject::new();
                for (existing, item) in out.iter() {
                    if existing != key {
                        kept.insert(existing, item.clone());
                    }
                }
                out = kept;
            } else {
                out.insert(key, value.clone());
            }
        }
    }
    JsValue::Object(out)
}

/// `shouldDetachFromArchivedParent(parent, child)`.
fn should_detach_from_archived_parent(parent: &JsValue, child: &JsValue) -> bool {
    let defined = |record: &JsValue| {
        record
            .get("workspaceId")
            .filter(|id| !matches!(id, JsValue::Undefined))
            .cloned()
    };
    let cross_workspace = match (defined(parent), defined(child)) {
        (Some(parent), Some(child)) => parent != child,
        _ => false,
    };
    cross_workspace || has_open_agent_tab(child.get("labels"))
}

/// `detachedAgentLabelPatch(labels)`.
fn detached_agent_label_patch(labels: Option<&JsValue>) -> JsValue {
    let mut patch = JsObject::new();
    patch.insert(PARENT_AGENT_ID_LABEL, JsValue::Null);
    if let Some(JsValue::Object(labels)) = labels {
        for (label, _) in labels.iter() {
            if is_open_agent_tab_label(label) {
                patch.insert(label, JsValue::Null);
            }
        }
    }
    JsValue::Object(patch)
}

fn is_child_of(record: &JsValue, parent_agent_id: &str) -> bool {
    record
        .get("labels")
        .and_then(|labels| labels.get(PARENT_AGENT_ID_LABEL))
        .and_then(JsValue::as_str)
        == Some(parent_agent_id)
}

fn not_found(agent_id: &str) -> AgentError {
    AgentError::new(format!("Agent not found: {agent_id}"))
}

fn storage_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::new(error.to_string())
}

/// `new Date(text)` in epoch milliseconds, or the `RangeError` its
/// `toISOString` throws.
fn date_millis(value: Option<&JsValue>) -> Result<i64, AgentError> {
    value
        .and_then(JsValue::as_str)
        .and_then(parse_iso_millis)
        .ok_or_else(|| AgentError {
            name: "RangeError".to_owned(),
            message: "Invalid time value".to_owned(),
        })
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = Result<T, AgentError>> + Send + 'a>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeArchive {
    Archive,
    Restore,
}

impl AgentManager {
    fn require_registry(&self) -> Result<AgentStorage, AgentError> {
        self.inner
            .registry
            .clone()
            .ok_or_else(|| AgentError::new("Agent storage unavailable"))
    }

    async fn lane_lock(&self, agent_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lane = Self::lane(&mut self.lock().lifecycle_lanes, agent_id);
        lane.lock_owned().await
    }

    /// `archiveAgent(agentId)`: the record's `archivedAt`.
    ///
    /// # Errors
    ///
    /// An unknown agent, missing storage, or a failed snapshot, close or
    /// cascade.
    pub async fn archive_agent(&self, agent_id: &str) -> Result<String, AgentError> {
        let _turn = self.lane_lock(agent_id).await;
        self.archive_agent_unlocked(agent_id, None).await
    }

    fn archive_agent_unlocked<'a>(
        &'a self,
        agent_id: &'a str,
        requested_archived_at: Option<String>,
    ) -> Boxed<'a, String> {
        Box::pin(async move {
            let agent = {
                let state = self.lock();
                Self::require_agent(&state, agent_id)?.snapshot.clone()
            };
            let Some(registry) = self.inner.registry.clone() else {
                return Err(AgentError::new("Agent storage is not configured"));
            };
            let view = agent.record_view();
            registry
                .apply_snapshot(
                    agent_id,
                    move || view,
                    SnapshotOverrides {
                        internal: Some(Some(agent.internal)),
                        ..SnapshotOverrides::default()
                    },
                )
                .await
                .map_err(storage_error)?;
            let Some(stored) = registry.get(agent_id).await else {
                return Err(AgentError::new(format!(
                    "Agent {agent_id} not found in storage after snapshot"
                )));
            };
            let archived = self
                .mark_record_archived(&stored, requested_archived_at)
                .await?;
            let archived_at = js_string(archived.get("archivedAt"));
            let millis = date_millis(archived.get("archivedAt"))?;
            if let Some(live) = self.lock().agent_mut(agent_id) {
                live.snapshot.updated_at_millis = millis;
            }
            self.close_agent_runtime(agent_id).await?;
            self.sync_native_archive_state(&stored, NativeArchive::Archive)
                .await?;
            self.discard_retained_agent_state(agent_id);
            self.cascade_archive_children(agent_id).await?;
            Ok(archived_at)
        })
    }

    /// `cascadeArchiveChildren(parentAgentId)`: children created through
    /// the MCP `create_agent` tool are archived with their parent, or
    /// detached when they live in another workspace or an open tab.
    fn cascade_archive_children<'a>(&'a self, parent_agent_id: &'a str) -> Boxed<'a, ()> {
        Box::pin(async move {
            let Some(registry) = self.inner.registry.clone() else {
                return Ok(());
            };
            let records = registry.list().await;
            let Some(parent) = records
                .iter()
                .find(|record| record.get("id").and_then(JsValue::as_str) == Some(parent_agent_id))
                .cloned()
            else {
                return Err(AgentError::new(format!(
                    "Archived parent {parent_agent_id} not found in storage"
                )));
            };
            for record in &records {
                if truthy(record.get("archivedAt")) || !is_child_of(record, parent_agent_id) {
                    continue;
                }
                let child_id = js_string(record.get("id"));
                let Some(child) = registry.get(&child_id).await else {
                    continue;
                };
                if truthy(child.get("archivedAt")) || !is_child_of(&child, parent_agent_id) {
                    continue;
                }
                let child_id = js_string(child.get("id"));
                let _turn = self.lane_lock(&child_id).await;
                let Some(current) = registry.get(&child_id).await else {
                    continue;
                };
                if truthy(current.get("archivedAt")) || !is_child_of(&current, parent_agent_id) {
                    continue;
                }
                if should_detach_from_archived_parent(&parent, &current) {
                    self.detach_agent_unlocked(&child_id).await?;
                } else if self.lock().agent(&child_id).is_some() {
                    self.archive_agent_unlocked(&child_id, None).await?;
                } else {
                    self.archive_snapshot_unlocked(&child_id, now_iso()).await?;
                }
            }
            Ok(())
        })
    }

    /// `markRecordArchived(record, archivedAt)`.
    async fn mark_record_archived(
        &self,
        record: &JsValue,
        archived_at: Option<String>,
    ) -> Result<JsValue, AgentError> {
        let archived_at = archived_at.unwrap_or_else(now_iso);
        let archived = self
            .persist_archived_record(record, &archived_at, Some(&archived_at))
            .await?;
        let id = js_string(record.get("id"));
        if self.lock().agent(&id).is_some() {
            self.notify_agent_state(&id);
        } else if !truthy(archived.get("internal")) {
            self.dispatch_stored_agent_state(&archived)?;
        }
        self.fire_agent_archived(&id).await;
        Ok(archived)
    }

    /// `persistArchivedRecord(record, options)`.
    async fn persist_archived_record(
        &self,
        record: &JsValue,
        archived_at: &str,
        updated_at: Option<&str>,
    ) -> Result<JsValue, AgentError> {
        let archived = build_archived_agent_record(record, archived_at, updated_at);
        self.require_registry()?
            .upsert(archived.clone())
            .await
            .map_err(storage_error)?;
        Ok(archived)
    }

    /// `fireAgentArchived(agentId)`.
    async fn fire_agent_archived(&self, agent_id: &str) {
        if let Some(callback) = &self.inner.on_agent_archived {
            let _ = callback(agent_id.to_owned()).await;
        }
    }

    /// `dispatchStoredAgentState(record)`: a closed `agent_state` for an
    /// agent that only exists in storage.
    fn dispatch_stored_agent_state(&self, record: &JsValue) -> Result<(), AgentError> {
        let present = |key: &str| {
            record
                .get(key)
                .filter(|value| !matches!(value, JsValue::Undefined))
                .cloned()
        };
        let snapshot = ManagedAgentSnapshot {
            id: js_string(record.get("id")),
            provider: js_string(record.get("provider")),
            cwd: js_string(record.get("cwd")),
            workspace_id: record
                .get("workspaceId")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            owner: present("owner"),
            capabilities: stored_agent_capabilities(),
            config: build_stored_agent_config(record),
            runtime_info: None,
            created_at_millis: date_millis(record.get("createdAt"))?,
            updated_at_millis: date_millis(record.get("updatedAt"))?,
            available_modes: Vec::new(),
            features: present("features"),
            current_mode_id: record
                .get("lastModeId")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            pending_permissions: Vec::new(),
            pending_replacement: false,
            persistence: record
                .get("persistence")
                .filter(|handle| !is_nullish(Some(handle)))
                .cloned(),
            history_primed: true,
            last_user_message_at_millis: match record
                .get("lastUserMessageAt")
                .filter(|at| truthy(Some(at)))
            {
                Some(at) => Some(date_millis(Some(at))?),
                None => None,
            },
            active_turn_id: None,
            active_turn_started_at_millis: None,
            last_usage: None,
            last_error: record
                .get("lastError")
                .and_then(JsValue::as_str)
                .map(str::to_owned),
            attention: extract_attention(record),
            internal: truthy(record.get("internal")),
            labels: record.get("labels").cloned().unwrap_or(JsValue::Undefined),
            lifecycle: AgentLifecycle::Closed,
            active_foreground_turn_id: None,
        };
        let state = self.lock();
        self.dispatch(&state, AgentManagerEvent::AgentState(Box::new(snapshot)));
        Ok(())
    }

    /// `archiveSnapshot(agentId, archivedAt)`: the archived record.
    ///
    /// # Errors
    ///
    /// Missing storage or record, or a failed write, close or cascade.
    pub async fn archive_snapshot(
        &self,
        agent_id: &str,
        archived_at: String,
    ) -> Result<JsValue, AgentError> {
        let _turn = self.lane_lock(agent_id).await;
        self.archive_snapshot_unlocked(agent_id, archived_at).await
    }

    fn archive_snapshot_unlocked<'a>(
        &'a self,
        agent_id: &'a str,
        archived_at: String,
    ) -> Boxed<'a, JsValue> {
        Box::pin(async move {
            let registry = self.require_registry()?;
            // A stored-only archive can have waited behind a persisted
            // resume: reuse the live transition so that runtime closes too.
            if self.lock().agent(agent_id).is_some() {
                self.archive_agent_unlocked(agent_id, Some(archived_at))
                    .await?;
                return registry
                    .get(agent_id)
                    .await
                    .ok_or_else(|| not_found(agent_id));
            }
            let record = registry
                .get(agent_id)
                .await
                .ok_or_else(|| not_found(agent_id))?;
            let next = self
                .persist_archived_record(&record, &archived_at, None)
                .await?;
            self.sync_native_archive_state(&record, NativeArchive::Archive)
                .await?;
            self.discard_retained_agent_state(agent_id);
            if !truthy(next.get("internal")) {
                self.dispatch_stored_agent_state(&next)?;
            }
            self.fire_agent_archived(agent_id).await;
            self.cascade_archive_children(agent_id).await?;
            Ok(next)
        })
    }

    /// `unarchiveSnapshot(agentId, updates)`: whether an archived record was
    /// restored.
    ///
    /// # Errors
    ///
    /// Missing storage, a failed close, a failed native restore, or a
    /// failed write.
    pub async fn unarchive_snapshot(
        &self,
        agent_id: &str,
        updates: Option<UnarchiveUpdates>,
    ) -> Result<bool, AgentError> {
        let _turn = self.lane_lock(agent_id).await;
        let registry = self.require_registry()?;
        let Some(record) = registry.get(agent_id).await else {
            return Ok(false);
        };
        if !truthy(record.get("archivedAt")) {
            return Ok(false);
        }
        // Close and native restore share the lifecycle lane with persisted
        // resume, so no runtime can take the writer between them.
        if self.lock().agent(agent_id).is_some() {
            self.close_agent_runtime(agent_id).await?;
        }
        self.sync_native_archive_state(&record, NativeArchive::Restore)
            .await?;
        let updates = updates.unwrap_or_default();
        let mut next = spread(Some(&record));
        if let Some(workspace_id) = updates.workspace_id.filter(|id| !id.is_empty()) {
            next.insert("workspaceId", JsValue::String(workspace_id));
        }
        if let Some(patch) = updates.labels.filter(|patch| truthy(Some(patch))) {
            next.insert("labels", apply_label_patch(record.get("labels"), &patch));
        }
        next.insert("archivedAt", JsValue::Null);
        next.insert("updatedAt", JsValue::String(now_iso()));
        registry
            .upsert(JsValue::Object(next))
            .await
            .map_err(storage_error)?;
        if self.get_agent(agent_id).is_some() {
            self.notify_agent_state(agent_id);
        }
        Ok(true)
    }

    /// `unarchiveSnapshotByHandle(handle)`.
    ///
    /// # Errors
    ///
    /// As [`Self::unarchive_snapshot`].
    pub async fn unarchive_snapshot_by_handle(&self, handle: &JsValue) -> Result<(), AgentError> {
        let registry = self.require_registry()?;
        let records = registry.list().await;
        let matched = records.iter().find(|record| {
            let persistence = record.get("persistence");
            persistence.and_then(|p| p.get("provider")) == handle.get("provider")
                && persistence.and_then(|p| p.get("sessionId")) == handle.get("sessionId")
        });
        if let Some(matched) = matched {
            self.unarchive_snapshot(&js_string(matched.get("id")), None)
                .await?;
        }
        Ok(())
    }

    /// `notifyAgentState(agentId)`.
    pub fn notify_agent_state(&self, agent_id: &str) {
        let mut state = self.lock();
        let Some(agent) = state.agent_mut(agent_id) else {
            return;
        };
        if agent.snapshot.internal {
            return;
        }
        touch_updated_at(&mut agent.snapshot);
        self.emit_state_locked(&mut state, agent_id, true);
    }

    /// `clearAgentAttention(agentId)`.
    ///
    /// # Errors
    ///
    /// An unknown agent, or the snapshot's persist error.
    pub async fn clear_agent_attention(&self, agent_id: &str) -> Result<(), AgentError> {
        let cleared = {
            let mut state = self.lock();
            let id = Self::require_agent(&state, agent_id)?.snapshot.id.clone();
            let agent = state.agent_mut(&id);
            match agent {
                Some(agent) if !matches!(agent.snapshot.attention, AgentAttention::None) => {
                    agent.snapshot.attention = AgentAttention::None;
                    Some(id)
                }
                _ => None,
            }
        };
        if let Some(id) = cleared {
            self.persist_snapshot(&id, SnapshotOverrides::default())
                .await?;
            self.emit_state(&id, false);
        }
        Ok(())
    }

    /// `detachAgent(agentId)`.
    ///
    /// # Errors
    ///
    /// Missing storage or record, or a failed write.
    pub async fn detach_agent(&self, agent_id: &str) -> Result<DetachedAgent, AgentError> {
        let _turn = self.lane_lock(agent_id).await;
        self.detach_agent_unlocked(agent_id).await
    }

    async fn detach_agent_unlocked(&self, agent_id: &str) -> Result<DetachedAgent, AgentError> {
        let registry = self.require_registry()?;
        let after_detach = || {
            AgentError::new(format!(
                "Agent not found in storage after detach: {agent_id}"
            ))
        };
        if let Some(live) = self.get_agent(agent_id) {
            let Some(previous) = parent_agent_id_from_labels(Some(&live.labels)) else {
                self.persist_snapshot(agent_id, SnapshotOverrides::default())
                    .await?;
                let record = registry.get(agent_id).await.ok_or_else(after_detach)?;
                return Ok(DetachedAgent {
                    record,
                    live: true,
                    previous_parent_agent_id: None,
                });
            };
            let record = self
                .write_labels(agent_id, &detached_agent_label_patch(Some(&live.labels)))
                .await?
                .ok_or_else(after_detach)?;
            return Ok(DetachedAgent {
                record,
                live: true,
                previous_parent_agent_id: Some(previous),
            });
        }
        let record = registry
            .get(agent_id)
            .await
            .ok_or_else(|| not_found(agent_id))?;
        let Some(previous) = parent_agent_id_from_labels(record.get("labels")) else {
            return Ok(DetachedAgent {
                record,
                live: false,
                previous_parent_agent_id: None,
            });
        };
        let record = self
            .write_labels(agent_id, &detached_agent_label_patch(record.get("labels")))
            .await?
            .ok_or_else(after_detach)?;
        Ok(DetachedAgent {
            record,
            live: false,
            previous_parent_agent_id: Some(previous),
        })
    }

    /// `writeLabels(agentId, patch)`: the stored record afterwards.
    async fn write_labels(
        &self,
        agent_id: &str,
        patch: &JsValue,
    ) -> Result<Option<JsValue>, AgentError> {
        let live = {
            let mut state = self.lock();
            match state.agent_mut(agent_id) {
                Some(agent) => {
                    agent.snapshot.labels = apply_label_patch(Some(&agent.snapshot.labels), patch);
                    touch_updated_at(&mut agent.snapshot);
                    true
                }
                None => false,
            }
        };
        if live {
            self.persist_snapshot(agent_id, SnapshotOverrides::default())
                .await?;
            self.emit_state(agent_id, false);
            return Ok(match &self.inner.registry {
                Some(registry) => registry.get(agent_id).await,
                None => None,
            });
        }
        let registry = self.require_registry()?;
        let record = registry
            .get(agent_id)
            .await
            .ok_or_else(|| not_found(agent_id))?;
        let mut next = spread(Some(&record));
        next.insert("labels", apply_label_patch(record.get("labels"), patch));
        next.insert(
            "updatedAt",
            JsValue::String(next_stored_updated_at(&record)?),
        );
        let next = JsValue::Object(next);
        registry.upsert(next.clone()).await.map_err(storage_error)?;
        Ok(Some(next))
    }

    /// `syncNativeArchiveState(provider, persistence, state)`: a failed
    /// native archive is best-effort; a failed restore fails the unarchive.
    async fn sync_native_archive_state(
        &self,
        record: &JsValue,
        state: NativeArchive,
    ) -> Result<(), AgentError> {
        let Some(persistence) = record
            .get("persistence")
            .filter(|handle| truthy(Some(handle)))
        else {
            return Ok(());
        };
        let Some(client) = self.lock().client(&js_string(record.get("provider"))) else {
            return Ok(());
        };
        let sync = match state {
            NativeArchive::Archive => client.archive_native_session(persistence.clone()),
            NativeArchive::Restore => client.unarchive_native_session(persistence.clone()),
        };
        let Some(sync) = sync else {
            return Ok(());
        };
        match (sync.await, state) {
            (Err(error), NativeArchive::Restore) => Err(error),
            _ => Ok(()),
        }
    }

    /// `discardRetainedAgentState(agentId)`.
    fn discard_retained_agent_state(&self, agent_id: &str) {
        let mut state = self.lock();
        state.timeline.delete(agent_id);
        state.paseo_tool_policies.remove(agent_id);
        let events = state.provider_subagents.delete_parent(agent_id);
        for event in events {
            self.dispatch(&state, AgentManagerEvent::ProviderSubagent(event));
        }
    }
}

/// `nextStoredUpdatedAt(record)`: now, or one millisecond past the stored
/// time when that is not in the past.
fn next_stored_updated_at(record: &JsValue) -> Result<String, AgentError> {
    let previous = date_millis(record.get("updatedAt"))?;
    let now = now_millis();
    Ok(iso_from_millis(if now > previous {
        now
    } else {
        previous + 1
    }))
}
