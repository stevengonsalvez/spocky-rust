//! `registerClient` and `updateProviderRegistry` from pinned Paseo
//! `agent/agent-manager.ts`.

use std::sync::Arc;

use spocky_store::js_value::{JsObject, JsValue};

use super::{AgentManager, ProviderDefinition};
use crate::agent_sdk::AgentClient;

/// `updateProviderRegistry`'s input.
#[derive(Clone, Default)]
pub struct ProviderRegistryUpdate {
    pub provider_definitions: Vec<(String, ProviderDefinition)>,
    pub clients: Vec<(String, Arc<dyn AgentClient>)>,
    /// `retiredProviders`: agents of these providers are closed.
    pub retired_providers: Vec<String>,
}

/// `Map.prototype.set`: a known key keeps its place.
fn map_set<T>(entries: &mut Vec<(String, T)>, key: String, value: T) {
    match entries.iter_mut().find(|(existing, _)| *existing == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key, value)),
    }
}

impl AgentManager {
    /// `registerClient(provider, client)`.
    pub fn register_client(&self, provider: &str, client: Arc<dyn AgentClient>) {
        map_set(&mut self.lock().clients, provider.to_owned(), client);
    }

    /// `updateProviderRegistry(input)`: replaces the provider definitions and
    /// clients, then closes, without waiting, every agent of a retired
    /// provider, warning when one fails to close.
    pub fn update_provider_registry(&self, input: ProviderRegistryUpdate) {
        let retired: Vec<(String, String)> = {
            let mut state = self.lock();
            state.provider_enabled.clear();
            state.provider_definitions.clear();
            for (provider, definition) in input.provider_definitions {
                map_set(
                    &mut state.provider_enabled,
                    provider.clone(),
                    definition.enabled,
                );
                map_set(&mut state.provider_definitions, provider, definition);
            }
            state.clients.clear();
            for (provider, client) in input.clients {
                map_set(&mut state.clients, provider, client);
            }
            input
                .retired_providers
                .iter()
                .flat_map(|provider| {
                    state
                        .agents
                        .iter()
                        .filter(|(_, agent)| agent.snapshot.provider == *provider)
                        .map(|(id, _)| (id.clone(), provider.clone()))
                })
                .collect()
        };
        for (agent_id, provider) in retired {
            let manager = self.clone();
            tokio::spawn(async move {
                if manager.close_agent(&agent_id).await.is_err() {
                    // pino prints the `err` binding, an `Error`, as `{}`.
                    let mut bindings = JsObject::new();
                    bindings.insert("err", JsValue::Object(JsObject::new()));
                    bindings.insert("agentId", JsValue::String(agent_id));
                    bindings.insert("provider", JsValue::String(provider));
                    manager.emit_warn(
                        JsValue::Object(bindings),
                        "Failed to close agent after provider retirement",
                    );
                }
            });
        }
    }
}
