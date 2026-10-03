//! `listImportableSessions` from pinned Paseo `agent/agent-manager.ts`.

use std::sync::Arc;
use std::time::Duration;

use spocky_contracts::js::truthy;
use spocky_store::js_value::{JsObject, JsValue};

use super::AgentManager;
use super::log_error::err_binding;
use crate::agent_sdk::{
    AgentClient, AgentError, ImportableProviderSession, ListImportableSessionsOptions,
};
use crate::paths::basename;
use crate::text::js_trim;

/// `IMPORTABLE_SESSION_LIST_TIMEOUT_MS`.
const IMPORTABLE_SESSION_LIST_TIMEOUT_MS: u64 = 90_000;

/// `ImportablePersistedAgentQueryOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportablePersistedAgentQueryOptions {
    pub list: ListImportableSessionsOptions,
    /// `providerFilter`: when set, only these providers are scanned.
    pub provider_filter: Option<Vec<String>>,
}

/// `ManagedImportableProviderSession`: the session plus its provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedImportableProviderSession {
    pub provider: String,
    pub session: ImportableProviderSession,
}

/// `ImportableSessionProviderError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportableSessionProviderError {
    pub provider: String,
    pub message: String,
}

/// `ManagedImportableSessionsResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManagedImportableSessionsResult {
    pub sessions: Vec<ManagedImportableProviderSession>,
    pub provider_errors: Vec<ImportableSessionProviderError>,
}

/// One provider's `{ sessions, error }`.
type ProviderListing = (
    Vec<ManagedImportableProviderSession>,
    Option<ImportableSessionProviderError>,
);

impl AgentManager {
    /// `listImportableSessions(options)`: every listing provider is asked on
    /// its own task, a failure or timeout becomes a provider error, and the
    /// merged sessions are ordered by last activity.
    pub async fn list_importable_sessions(
        &self,
        options: Option<ImportablePersistedAgentQueryOptions>,
    ) -> ManagedImportableSessionsResult {
        let options = options.unwrap_or_default();
        let entries: Vec<(String, Arc<dyn AgentClient>)> = {
            let state = self.lock();
            state
                .clients
                .iter()
                .filter(|(provider, client)| {
                    truthy(client.capabilities().get("supportsSessionListing"))
                        && is_provider_importable(
                            &state.provider_enabled,
                            provider,
                            options.provider_filter.as_deref(),
                        )
                })
                .cloned()
                .collect()
        };
        let tasks: Vec<_> = entries
            .into_iter()
            .map(|(provider, client)| {
                let manager = self.clone();
                let list = options.list.clone();
                tokio::spawn(async move {
                    manager
                        .list_provider_sessions(provider, &client, list)
                        .await
                })
            })
            .collect();
        let mut sessions = Vec::new();
        let mut provider_errors = Vec::new();
        for task in tasks {
            // A client without `listImportableSessions` is left out.
            let listing = match task.await {
                Ok(listing) => listing,
                Err(error) => std::panic::resume_unwind(error.into_panic()),
            };
            if let Some((found, error)) = listing {
                sessions.extend(found);
                provider_errors.extend(error);
            }
        }
        // `Array.prototype.sort` is stable, as is this.
        sessions.sort_by(|a, b| {
            let difference = b.session.last_activity_at_millis - a.session.last_activity_at_millis;
            if difference > 0.0 {
                std::cmp::Ordering::Greater
            } else if difference < 0.0 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        });
        sessions.truncate(js_slice_end(
            sessions.len(),
            options.list.limit.unwrap_or(20.0),
        ));
        ManagedImportableSessionsResult {
            sessions,
            provider_errors,
        }
    }

    /// One provider's entry in `listImportableSessions`, or `None` when the
    /// client has no `listImportableSessions`.
    async fn list_provider_sessions(
        &self,
        provider: String,
        client: &Arc<dyn AgentClient>,
        list: ListImportableSessionsOptions,
    ) -> Option<ProviderListing> {
        let query = list.query.clone();
        let listing = client.list_importable_sessions(Some(list))?;
        let timeout_message = format!(
            "Timed out listing importable sessions for provider '{provider}' after {IMPORTABLE_SESSION_LIST_TIMEOUT_MS}ms"
        );
        let outcome = match tokio::time::timeout(
            Duration::from_millis(IMPORTABLE_SESSION_LIST_TIMEOUT_MS),
            listing,
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(AgentError::new(timeout_message)),
        };
        Some(match outcome {
            Ok(found) => (
                found
                    .into_iter()
                    .filter(|session| matches_importable_session_query(session, query.as_deref()))
                    .map(|session| ManagedImportableProviderSession {
                        provider: provider.clone(),
                        session,
                    })
                    .collect(),
                None,
            ),
            Err(error) => {
                let message = error.message.clone();
                let mut bindings = JsObject::new();
                bindings.insert("err", err_binding(&error));
                bindings.insert("provider", JsValue::String(provider.clone()));
                self.emit_warn(
                    JsValue::Object(bindings),
                    "Failed to list importable sessions for provider",
                );
                (
                    Vec::new(),
                    Some(ImportableSessionProviderError { provider, message }),
                )
            }
        })
    }
}

/// `isProviderImportable`.
fn is_provider_importable(
    provider_enabled: &[(String, bool)],
    provider: &str,
    provider_filter: Option<&[String]>,
) -> bool {
    if provider_enabled
        .iter()
        .any(|(id, enabled)| id == provider && !*enabled)
    {
        return false;
    }
    provider_filter.is_none_or(|filter| filter.iter().any(|id| id == provider))
}

/// `matchesImportableSessionQuery`: a case-insensitive match over the title,
/// both prompt previews and the working directory's basename.
fn matches_importable_session_query(
    session: &ImportableProviderSession,
    raw_query: Option<&str>,
) -> bool {
    let query = raw_query.map(|raw| js_trim(raw).to_lowercase());
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return true;
    };
    let cwd_basename = basename(&session.cwd.replace('\\', "/"));
    [
        session.title.as_deref(),
        session.first_prompt_preview.as_deref(),
        session.last_prompt_preview.as_deref(),
        Some(cwd_basename.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_lowercase().contains(&query))
}

/// The end index of `array.slice(0, limit)` for an array of `len` items.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "array lengths are far below 2^52 and the end is clamped to 0..=len"
)]
fn js_slice_end(len: usize, limit: f64) -> usize {
    let end = if limit.is_nan() { 0.0 } else { limit.trunc() };
    let len = len as f64;
    (if end < 0.0 {
        (len + end).max(0.0)
    } else {
        end.min(len)
    }) as usize
}
