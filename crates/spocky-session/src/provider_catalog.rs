//! The catalogue half of `createRegistryEntry` in pinned Paseo
//! `agent/provider-registry.ts`: a provider definition's `fetchCatalog`
//! merges configured profile and additional models into the runtime
//! catalogue and decorates modes with the definition's icons and colour
//! tiers. [`registry_fetch_catalog`] builds it as the snapshot manager's
//! [`FetchCatalogHook`].

use std::sync::Arc;

use spocky_contracts::js::{spread, truthy};
use spocky_store::js_value::{JsObject, JsValue};

use crate::agent_sdk::{
    AgentClient, AgentError, AgentProvider, FetchCatalogOptions, ProviderRefreshContext,
    ResolveAgentDefaultModeInput, run_activity,
};
use crate::provider_snapshot_manager::FetchCatalogHook;

fn nullish(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
}

/// `normalizeAgentModelDefinition(model)`: fills `defaultThinkingOptionId`
/// from the default thinking option.
#[must_use]
pub fn normalize_agent_model_definition(model: &JsValue) -> JsValue {
    let explicit = nullish(model.get("defaultThinkingOptionId"));
    let default_id = explicit.cloned().or_else(|| {
        model
            .get("thinkingOptions")
            .and_then(JsValue::as_array)
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| truthy(option.get("isDefault")))
            })
            .and_then(|option| option.get("id").cloned())
    });
    match default_id {
        Some(id) if truthy(Some(&id)) && model.get("defaultThinkingOptionId") != Some(&id) => {
            let mut out = spread(Some(model));
            out.insert("defaultThinkingOptionId", id);
            JsValue::Object(out)
        }
        _ => model.clone(),
    }
}

/// `mapModel(provider, model)`.
#[must_use]
pub fn map_model(provider: &str, model: &JsValue) -> JsValue {
    let mut out = spread(Some(model));
    out.insert("provider", JsValue::String(provider.to_owned()));
    normalize_agent_model_definition(&JsValue::Object(out))
}

/// `resolveConfiguredModels(provider, client, models)`.
#[must_use]
pub fn resolve_configured_models(
    provider: &str,
    client: &dyn AgentClient,
    models: &[JsValue],
) -> Vec<JsValue> {
    models
        .iter()
        .map(|model| {
            let mapped = map_model(provider, model);
            client.resolve_configured_model(&mapped).unwrap_or(mapped)
        })
        .collect()
}

/// `mergeModelAdditions(provider, baseModels, modelAdditions)`.
fn merge_model_additions(
    provider: &str,
    base_models: Vec<JsValue>,
    additions: &[JsValue],
) -> Vec<JsValue> {
    if additions.is_empty() {
        return base_models;
    }
    let mut merged = base_models;
    let mut has_additional_default = false;
    for model in additions {
        let additional = map_model(provider, model);
        has_additional_default |= matches!(additional.get("isDefault"), Some(JsValue::Bool(true)));
        let id = model.get("id");
        let Some(index) = merged
            .iter()
            .position(|candidate| candidate.get("id") == id)
        else {
            merged.push(additional);
            continue;
        };
        let existing = &merged[index];
        let enables_compatibility_model =
            matches!(existing.get("isSelectable"), Some(JsValue::Bool(false)))
                && matches!(
                    additional.get("isSelectable"),
                    None | Some(JsValue::Undefined)
                );
        let mut combined = spread(Some(existing));
        if let JsValue::Object(object) = &additional {
            for (key, value) in object.iter() {
                combined.insert(key, value.clone());
            }
        }
        if enables_compatibility_model {
            combined.insert("isSelectable", JsValue::Bool(true));
        }
        merged[index] = JsValue::Object(combined);
    }
    if !has_additional_default {
        return merged;
    }
    let default_ids: Vec<Option<&JsValue>> = additions
        .iter()
        .filter(|model| matches!(model.get("isDefault"), Some(JsValue::Bool(true))))
        .map(|model| model.get("id"))
        .collect();
    merged
        .into_iter()
        .map(|model| {
            if default_ids.contains(&model.get("id")) {
                model
            } else {
                let mut out = spread(Some(&model));
                out.insert("isDefault", JsValue::Bool(false));
                JsValue::Object(out)
            }
        })
        .collect()
}

/// `mergeModels(provider, profileModels, additionalModels, runtimeModels,
/// { profileModelsAreAdditive })`.
#[must_use]
pub fn merge_models(
    provider: &str,
    profile_models: &[JsValue],
    additional_models: &[JsValue],
    runtime_models: &[JsValue],
    profile_models_are_additive: bool,
) -> Vec<JsValue> {
    if !profile_models.is_empty() && !profile_models_are_additive {
        return merge_model_additions(
            provider,
            profile_models
                .iter()
                .map(|model| map_model(provider, model))
                .collect(),
            additional_models,
        );
    }
    let base = runtime_models
        .iter()
        .map(|model| map_model(provider, model))
        .collect();
    let additions: Vec<JsValue> = profile_models
        .iter()
        .chain(additional_models)
        .cloned()
        .collect();
    merge_model_additions(provider, base, &additions)
}

/// `decorateModes(modes)`: a mode missing its icon or colour tier takes the
/// definition mode's.
#[must_use]
pub fn decorate_modes(modes: &[JsValue], definition_modes: &[JsValue]) -> Vec<JsValue> {
    modes
        .iter()
        .map(|mode| {
            if truthy(mode.get("icon")) && truthy(mode.get("colorTier")) {
                return mode.clone();
            }
            let Some(definition) = definition_modes
                .iter()
                .find(|candidate| candidate.get("id") == mode.get("id"))
            else {
                return mode.clone();
            };
            let pick = |key: &str| {
                nullish(mode.get(key))
                    .or_else(|| definition.get(key))
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            };
            let mut out = spread(Some(mode));
            out.insert("icon", pick("icon"));
            out.insert("colorTier", pick("colorTier"));
            JsValue::Object(out)
        })
        .collect()
}

/// The registry inputs of one provider's `fetchCatalog`.
#[derive(Debug, Clone, Default)]
pub struct RegistryCatalog {
    pub provider: AgentProvider,
    /// `resolved.definition.modes`.
    pub definition_modes: Vec<JsValue>,
    /// `resolveConfiguredModels(provider, modelClient, resolved.profileModels)`.
    pub profile_models: Vec<JsValue>,
    /// `resolveConfiguredModels(provider, modelClient, resolved.additionalModels)`.
    pub additional_models: Vec<JsValue>,
    pub profile_models_are_additive: bool,
}

/// `TypeError` of `decorateModes(undefined)`.
fn decorate_catalog_modes(
    catalog: &JsValue,
    definition_modes: &[JsValue],
) -> Result<JsValue, AgentError> {
    match catalog.get("modes") {
        Some(JsValue::Array(modes)) => Ok(JsValue::Array(decorate_modes(modes, definition_modes))),
        Some(JsValue::Null) => Err(AgentError::named(
            "TypeError".to_owned(),
            "Cannot read properties of null (reading 'map')".to_owned(),
        )),
        None | Some(JsValue::Undefined) => Err(AgentError::named(
            "TypeError".to_owned(),
            "Cannot read properties of undefined (reading 'map')".to_owned(),
        )),
        Some(_) => Err(AgentError::named(
            "TypeError".to_owned(),
            "modes.map is not a function".to_owned(),
        )),
    }
}

fn runtime_models(catalog: &JsValue) -> Result<&[JsValue], AgentError> {
    match catalog.get("models") {
        Some(JsValue::Array(models)) => Ok(models),
        Some(JsValue::Null) => Err(AgentError::named(
            "TypeError".to_owned(),
            "Cannot read properties of null (reading 'map')".to_owned(),
        )),
        None | Some(JsValue::Undefined) => Err(AgentError::named(
            "TypeError".to_owned(),
            "Cannot read properties of undefined (reading 'map')".to_owned(),
        )),
        Some(_) => Err(AgentError::named(
            "TypeError".to_owned(),
            "runtimeModels.map is not a function".to_owned(),
        )),
    }
}

/// `definition.fetchCatalog(options, client, context)` of a registry entry.
#[must_use]
pub fn registry_fetch_catalog(registry: RegistryCatalog) -> FetchCatalogHook {
    let registry = Arc::new(registry);
    Arc::new(
        move |options: FetchCatalogOptions,
              client: Arc<dyn AgentClient>,
              context: Arc<dyn ProviderRefreshContext>| {
            let registry = Arc::clone(&registry);
            Box::pin(async move { fetch(&registry, options, client, context).await })
        },
    )
}

async fn fetch(
    registry: &RegistryCatalog,
    options: FetchCatalogOptions,
    client: Arc<dyn AgentClient>,
    context: Arc<dyn ProviderRefreshContext>,
) -> Result<JsValue, AgentError> {
    let provider = registry.provider.as_str();
    let has_replacement_models =
        !registry.profile_models.is_empty() && !registry.profile_models_are_additive;
    if has_replacement_models {
        // Replacement models skip runtime model discovery; additional models
        // still merge on top. Static modes need no runtime at all.
        let replacement = registry
            .profile_models
            .iter()
            .map(|model| map_model(provider, model))
            .collect();
        let models = merge_model_additions(provider, replacement, &registry.additional_models);
        if !registry.definition_modes.is_empty() {
            let cwd = match &options {
                FetchCatalogOptions::Workspace { cwd, .. } => cwd.clone(),
                FetchCatalogOptions::Global { .. } => std::env::current_dir()
                    .map(|cwd| crate::text::path_text(&cwd))
                    .unwrap_or_default(),
            };
            let mut config = JsObject::new();
            config.insert("provider", JsValue::String(provider.to_owned()));
            config.insert("cwd", JsValue::String(cwd));
            let input = ResolveAgentDefaultModeInput {
                config: JsValue::Object(config),
                env: None,
                signal: Some(context.signal().clone()),
            };
            let default_mode_id = run_activity(context.as_ref(), "default-mode", async {
                match client.resolve_default_mode_id(input) {
                    Some(resolve) => resolve.await,
                    None => Ok(None),
                }
            })
            .await?;
            let mut out = JsObject::new();
            out.insert("models", JsValue::Array(models));
            out.insert(
                "modes",
                JsValue::Array(decorate_modes(
                    &registry.definition_modes,
                    &registry.definition_modes,
                )),
            );
            out.insert(
                "defaultModeId",
                default_mode_id.map_or(JsValue::Undefined, JsValue::String),
            );
            return Ok(JsValue::Object(out));
        }
        let catalog = client.fetch_catalog(options, Some(context)).await?;
        let decorated = decorate_catalog_modes(&catalog, &registry.definition_modes)?;
        let mut out = spread(Some(&catalog));
        out.insert("models", JsValue::Array(models));
        out.insert("modes", decorated);
        return Ok(JsValue::Object(out));
    }
    let catalog = client.fetch_catalog(options, Some(context)).await?;
    let models = merge_models(
        provider,
        &registry.profile_models,
        &registry.additional_models,
        runtime_models(&catalog)?,
        registry.profile_models_are_additive,
    );
    let decorated = decorate_catalog_modes(&catalog, &registry.definition_modes)?;
    let mut out = spread(Some(&catalog));
    out.insert("models", JsValue::Array(models));
    out.insert("modes", decorated);
    Ok(JsValue::Object(out))
}
