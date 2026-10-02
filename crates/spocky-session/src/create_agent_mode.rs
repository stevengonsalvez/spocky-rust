//! `agent/create-agent-mode.ts` from pinned Paseo: the default create-config
//! resolution that providers without their own `resolveCreateConfig` use.

use spocky_contracts::js::js_string;
use spocky_store::js_value::JsValue;

use crate::agent_sdk::{
    AgentCreateConfigUnattendedInput, AgentError, ResolveAgentCreateConfigInput,
    ResolveAgentCreateConfigResult,
};

/// `listModes(modes)`.
fn list_modes(modes: Option<&[String]>) -> String {
    match modes {
        None => "unknown".to_owned(),
        Some([]) => "(none)".to_owned(),
        Some(modes) => modes.join(", "),
    }
}

/// `isUnattendedMode(mode)`.
fn is_unattended_mode(mode: &JsValue) -> bool {
    matches!(mode.get("isUnattended"), Some(JsValue::Bool(true)))
}

fn mode_list(modes: Option<&JsValue>) -> Option<&[JsValue]> {
    modes.and_then(JsValue::as_array)
}

/// `resolveAndValidateCreateAgentMode(input)`, with the parent as
/// `AgentCreateConfigParent` (`{ provider, modeId, isUnattended }`).
///
/// # Errors
///
/// Returns the baseline's error for a mode outside `available_modes`, or
/// for a parent mode that cannot be inherited.
pub fn resolve_and_validate_create_agent_mode(
    requested_mode: Option<&str>,
    target_provider: &str,
    parent: Option<&JsValue>,
    unattended: bool,
    available_modes: Option<&[String]>,
    target_unattended_mode: Option<&str>,
) -> Result<Option<String>, AgentError> {
    if let Some(requested) = requested_mode {
        if let Some(modes) = available_modes
            && !modes.iter().any(|mode| mode == requested)
        {
            return Err(AgentError::new(format!(
                "Invalid mode '{requested}' for provider '{target_provider}'. Available modes: {}",
                list_modes(available_modes)
            )));
        }
        return Ok(Some(requested.to_owned()));
    }
    let Some(parent) = parent else {
        if unattended && let Some(mode) = target_unattended_mode {
            return Ok(Some(mode.to_owned()));
        }
        return Ok(None);
    };
    let parent_provider = js_string(parent.get("provider"));
    let parent_mode = parent
        .get("modeId")
        .filter(|mode| !matches!(mode, JsValue::Null | JsValue::Undefined));
    if parent_provider == target_provider {
        return Ok(parent_mode.map(|mode| js_string(Some(mode))));
    }
    let parent_unattended = matches!(parent.get("isUnattended"), Some(JsValue::Bool(true)));
    if (unattended || parent_unattended)
        && let Some(mode) = target_unattended_mode
    {
        return Ok(Some(mode.to_owned()));
    }
    if available_modes.is_some_and(<[String]>::is_empty) {
        return Ok(None);
    }
    Err(AgentError::new(format!(
        "cannot inherit mode '{}' from caller (provider '{parent_provider}') for new agent (provider '{target_provider}'). Pass an explicit mode. Available modes for '{target_provider}': {}",
        parent_mode.map_or_else(|| "<none>".to_owned(), |mode| js_string(Some(mode))),
        list_modes(available_modes)
    )))
}

/// `resolveDefaultAgentCreateConfig(input)`.
///
/// # Errors
///
/// As [`resolve_and_validate_create_agent_mode`].
pub fn resolve_default_agent_create_config(
    input: &ResolveAgentCreateConfigInput,
) -> Result<ResolveAgentCreateConfigResult, AgentError> {
    let modes = mode_list(input.available_modes.as_ref());
    let ids: Option<Vec<String>> =
        modes.map(|modes| modes.iter().map(|mode| js_string(mode.get("id"))).collect());
    let unattended_mode = modes
        .and_then(|modes| modes.iter().find(|mode| is_unattended_mode(mode)))
        .map(|mode| js_string(mode.get("id")));
    Ok(ResolveAgentCreateConfigResult {
        mode_id: resolve_and_validate_create_agent_mode(
            input.requested_mode.as_deref(),
            &input.provider,
            input.parent.as_ref(),
            input.unattended,
            ids.as_deref(),
            unattended_mode.as_deref(),
        )?,
        feature_values: input.feature_values.clone(),
    })
}

/// `isDefaultAgentCreateConfigUnattended(input)`.
#[must_use]
pub fn is_default_agent_create_config_unattended(input: &AgentCreateConfigUnattendedInput) -> bool {
    let Some(mode_id) = &input.mode_id else {
        return false;
    };
    mode_list(Some(&input.available_modes)).is_some_and(|modes| {
        modes.iter().any(|mode| {
            mode.get("id").and_then(JsValue::as_str) == Some(mode_id.as_str())
                && is_unattended_mode(mode)
        })
    })
}
