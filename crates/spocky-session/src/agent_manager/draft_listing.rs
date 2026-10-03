//! `listDraftCommands` and `listDraftFeatures` from pinned Paseo
//! `agent/agent-manager.ts`.

use std::sync::Arc;

use spocky_contracts::js::{js_string, truthy};
use spocky_store::js_value::{JsObject, JsValue};

use super::AgentManager;
use super::log_error::err_binding;
use crate::agent_sdk::{AgentClient, AgentError, AgentResumePurpose, AgentSession};

impl AgentManager {
    /// `listDraftCommands(config)`: the slash commands a not yet created agent
    /// would offer, from the client or from a short-lived session.
    ///
    /// # Errors
    ///
    /// A config that does not normalize, an unregistered or unavailable
    /// provider, a provider that cannot list commands, or the provider's own
    /// failure.
    pub async fn list_draft_commands(&self, config: &JsValue) -> Result<JsValue, AgentError> {
        let normalized = self
            .normalize_config_with(config, AgentResumePurpose::Interactive, false)
            .await?;
        let provider = js_string(normalized.get("provider"));
        let client = self.require_client(&provider)?;
        if !truthy(normalized.get("model")) {
            return Ok(JsValue::Array(Vec::new()));
        }
        require_draft_availability(&client, &provider).await?;
        if let Some(listing) = client.list_commands(normalized.clone()) {
            return listing.await;
        }
        let session = client.create_session(normalized, None, None).await?;
        let listed = match session.list_commands() {
            Some(listing) => listing.await,
            None => Err(AgentError::new(format!(
                "Provider '{provider}' does not support listing commands"
            ))),
        };
        self.close_draft_session(
            &session,
            &provider,
            "Failed to close draft command listing session",
        )
        .await;
        listed
    }

    /// `listDraftFeatures(config)`: the features a not yet created agent would
    /// offer, from the client or from a short-lived session.
    ///
    /// # Errors
    ///
    /// A config that does not normalize, an unregistered or unavailable
    /// provider, or the provider's own failure.
    pub async fn list_draft_features(&self, config: &JsValue) -> Result<JsValue, AgentError> {
        let normalized = self
            .normalize_config_with(config, AgentResumePurpose::Interactive, false)
            .await?;
        let provider = js_string(normalized.get("provider"));
        let client = self.require_client(&provider)?;
        // The listing is a future that does nothing until polled, so asking
        // for it here only tells whether the client has `listFeatures`.
        let listing = client.list_features(normalized.clone());
        if !truthy(normalized.get("model")) && listing.is_none() {
            return Ok(JsValue::Array(Vec::new()));
        }
        require_draft_availability(&client, &provider).await?;
        if let Some(listing) = listing {
            return listing.await;
        }
        let session = client.create_session(normalized, None, None).await?;
        let features = session
            .features()
            .filter(|features| !matches!(features, JsValue::Undefined | JsValue::Null))
            .unwrap_or_else(|| JsValue::Array(Vec::new()));
        self.close_draft_session(
            &session,
            &provider,
            "Failed to close draft feature listing session",
        )
        .await;
        Ok(features)
    }

    /// `requireClient(provider)`.
    fn require_client(&self, provider: &str) -> Result<Arc<dyn AgentClient>, AgentError> {
        self.lock().client(provider).ok_or_else(|| {
            AgentError::new(format!("No client registered for provider '{provider}'"))
        })
    }

    /// Closes a draft listing session; a failure is only logged.
    async fn close_draft_session(
        &self,
        session: &Arc<dyn AgentSession>,
        provider: &str,
        message: &str,
    ) {
        if let Err(error) = session.close().await {
            let mut bindings = JsObject::new();
            bindings.insert("err", err_binding(&error));
            bindings.insert("provider", JsValue::String(provider.to_owned()));
            self.emit_warn(JsValue::Object(bindings), message);
        }
    }
}

/// The availability check both draft listings make before asking a provider.
async fn require_draft_availability(
    client: &Arc<dyn AgentClient>,
    provider: &str,
) -> Result<(), AgentError> {
    if client.is_available(None, None).await? {
        Ok(())
    } else {
        Err(AgentError::new(format!(
            "Provider '{provider}' is not available. Please ensure the CLI is installed."
        )))
    }
}
