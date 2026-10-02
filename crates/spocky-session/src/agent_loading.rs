//! `ensureAgentLoaded` from pinned Paseo `agent/agent-loading.ts`: brings a
//! stored agent into the manager. It resumes the provider session from the
//! record's persistence handle, or starts a first session from the stored
//! config when there is none, then hydrates the timeline from the
//! provider.
//!
//! Concurrent loads of one agent share a single initialization through a
//! process-wide in-flight map, as the baseline's module-level map does. The
//! initialization runs in its own task, so it finishes whether or not its
//! callers wait; its map entry is cleared when it settles.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use spocky_contracts::js::{js_string, truthy};
use spocky_store::js_value::JsValue;
use tokio::sync::watch;

use crate::agent_manager::{
    AgentManager, CreateAgentOptions, HydrateBroadcast, HydrateTimelineOptions,
    ManagedAgentSnapshot, ResumeAgentOptions,
};
use crate::agent_sdk::{AgentError, AgentResumePurpose, AgentResumeSessionOptions};
use crate::agent_storage::AgentStorage;
use crate::persistence_hooks::{
    build_config_overrides, build_session_config, extract_attention, extract_timestamps,
    is_stored_agent_provider_available, to_agent_persistence_handle,
};

type LoadResult = Result<ManagedAgentSnapshot, AgentError>;

/// `PendingAgentInitialization`.
struct Pending {
    id: u64,
    done: watch::Receiver<Option<LoadResult>>,
    broadcast_timeline: Arc<AtomicBool>,
}

/// `pendingAgentInitializations`.
static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Mutex::default);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn pending() -> std::sync::MutexGuard<'static, HashMap<String, Pending>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `EnsureAgentLoadedDeps`.
#[derive(Clone)]
pub struct EnsureAgentLoadedDeps {
    pub agent_manager: AgentManager,
    pub agent_storage: AgentStorage,
    /// `validProviders`; the manager's registered providers when `None`.
    pub valid_providers: Option<Vec<String>>,
    pub broadcast_timeline: bool,
}

/// Joins an in-flight load, raising its timeline broadcast if asked.
fn join(agent_id: &str, broadcast_timeline: bool) -> Option<watch::Receiver<Option<LoadResult>>> {
    let map = pending();
    let inflight = map.get(agent_id)?;
    if broadcast_timeline {
        inflight.broadcast_timeline.store(true, Ordering::SeqCst);
    }
    Some(inflight.done.clone())
}

async fn settled(mut done: watch::Receiver<Option<LoadResult>>) -> LoadResult {
    match done.wait_for(Option::is_some).await {
        Ok(result) => result
            .clone()
            .unwrap_or_else(|| unreachable!("waited for Some")),
        Err(_) => Err(AgentError::new("agent load stopped")),
    }
}

/// `ensureAgentLoaded(agentId, deps)`.
///
/// # Errors
///
/// `Agent not found: <id>` without a stored record, `Agent <id> references
/// unavailable provider '<provider>'`, or the resume, create or hydrate
/// error.
pub async fn ensure_agent_loaded(agent_id: &str, deps: &EnsureAgentLoadedDeps) -> LoadResult {
    deps.agent_manager.wait_for_agent_close(agent_id).await;
    if let Some(done) = join(agent_id, deps.broadcast_timeline) {
        return settled(done).await;
    }
    if let Some(existing) = deps.agent_manager.get_agent(agent_id) {
        return Ok(existing);
    }
    // A close may have started after the first barrier observed no
    // in-flight work; this second barrier closes that gap before the
    // storage-backed resume begins.
    deps.agent_manager.wait_for_agent_close(agent_id).await;
    let done = {
        let mut map = pending();
        if let Some(inflight) = map.get(agent_id) {
            if deps.broadcast_timeline {
                inflight.broadcast_timeline.store(true, Ordering::SeqCst);
            }
            inflight.done.clone()
        } else {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let broadcast_timeline = Arc::new(AtomicBool::new(deps.broadcast_timeline));
            let (finished, done) = watch::channel(None);
            map.insert(
                agent_id.to_owned(),
                Pending {
                    id,
                    done: done.clone(),
                    broadcast_timeline: Arc::clone(&broadcast_timeline),
                },
            );
            let deps = deps.clone();
            let agent_id = agent_id.to_owned();
            tokio::spawn(async move {
                let result = initialize(&agent_id, &deps, broadcast_timeline).await;
                {
                    let mut map = pending();
                    if map.get(&agent_id).is_some_and(|current| current.id == id) {
                        map.remove(&agent_id);
                    }
                }
                let _ = finished.send(Some(result));
            });
            done
        }
    };
    settled(done).await
}

fn unavailable(agent_id: &str, record: &JsValue) -> AgentError {
    AgentError::new(format!(
        "Agent {agent_id} references unavailable provider '{}'",
        js_string(record.get("provider"))
    ))
}

/// The initialization the in-flight map shares.
#[allow(
    clippy::single_match_else,
    reason = "the two branches mirror the baseline's if/else"
)]
async fn initialize(
    agent_id: &str,
    deps: &EnsureAgentLoadedDeps,
    broadcast_timeline: Arc<AtomicBool>,
) -> LoadResult {
    let manager = &deps.agent_manager;
    let Some(record) = deps.agent_storage.get(agent_id).await else {
        return Err(AgentError::new(format!("Agent not found: {agent_id}")));
    };
    let valid_providers = deps
        .valid_providers
        .clone()
        .unwrap_or_else(|| manager.registered_provider_ids());
    if !is_stored_agent_provider_available(&record, Some(&valid_providers)) {
        return Err(unavailable(agent_id, &record));
    }
    let snapshot = match to_agent_persistence_handle(&valid_providers, record.get("persistence")) {
        Some(handle) => {
            let timestamps = extract_timestamps(&record);
            manager
                .resume_agent_from_persistence(
                    handle,
                    Some(build_config_overrides(&record)),
                    Some(agent_id.to_owned()),
                    ResumeAgentOptions {
                        created_at_millis: timestamps.created_at_millis,
                        updated_at_millis: timestamps.updated_at_millis,
                        last_user_message_at_millis: timestamps.last_user_message_at_millis,
                        labels: timestamps.labels,
                        workspace_id: timestamps.workspace_id,
                        owner: timestamps.owner,
                        attention: Some(extract_attention(&record)),
                    },
                    truthy(record.get("archivedAt")).then_some(AgentResumeSessionOptions {
                        purpose: Some(AgentResumePurpose::History),
                    }),
                )
                .await?
        }
        None => {
            // No provider handle: this starts the agent's first session, so
            // it stamps activity and carries no stored attention.
            let config = build_session_config(&record, Some(&valid_providers))
                .ok_or_else(|| unavailable(agent_id, &record))?;
            manager
                .create_agent(
                    config,
                    Some(agent_id.to_owned()),
                    CreateAgentOptions {
                        labels: record
                            .get("labels")
                            .filter(|labels| !matches!(labels, JsValue::Undefined))
                            .cloned(),
                        workspace_id: record
                            .get("workspaceId")
                            .and_then(JsValue::as_str)
                            .map(str::to_owned),
                        owner: record
                            .get("owner")
                            .filter(|owner| !matches!(owner, JsValue::Undefined))
                            .cloned(),
                        ..CreateAgentOptions::default()
                    },
                )
                .await?
        }
    };
    manager
        .hydrate_timeline_from_provider(
            agent_id,
            HydrateTimelineOptions {
                broadcast: Some(HydrateBroadcast::Deferred(Box::new(move || {
                    broadcast_timeline.load(Ordering::SeqCst)
                }))),
                ..HydrateTimelineOptions::default()
            },
        )
        .await?;
    Ok(manager.get_agent(agent_id).unwrap_or(snapshot))
}
