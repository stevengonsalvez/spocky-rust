//! Stored agent record projection from pinned Paseo
//! `agent/agent-projections.ts` (`toStoredAgentRecord` and its sanitizers)
//! and `agent/agent-storage.ts` (`applySnapshot`).
//!
//! Provider-defined parts of a live agent (config, runtime info, features,
//! persistence handle, owner) stay JavaScript values, so the record keeps the
//! key order and `undefined` slots the baseline's object literals produce.

use spocky_store::js_value::{JsObject, JsValue};

use crate::clock::iso_from_millis;
use crate::js::{spread, truthy};
use crate::timeline::JsTypeError;

/// `ManagedAgent.attention`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAttention {
    None,
    Required {
        reason: String,
        timestamp_millis: i64,
    },
}

/// The fields of a live `ManagedAgent` that the stored record reads.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedAgentRecordView {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    pub workspace_id: Option<String>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
    pub last_user_message_at_millis: Option<i64>,
    /// `agent.labels`, an object.
    pub labels: JsValue,
    /// `agent.lifecycle`.
    pub lifecycle: String,
    pub current_mode_id: Option<String>,
    /// `agent.config` (`AgentSessionConfig`), an object.
    pub config: JsValue,
    pub runtime_info: Option<JsValue>,
    pub features: Option<JsValue>,
    pub persistence: Option<JsValue>,
    pub last_error: Option<String>,
    pub attention: AgentAttention,
    pub internal: Option<bool>,
    pub owner: Option<JsValue>,
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn defined(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|value| !matches!(value, JsValue::Undefined))
}

fn field(value: &JsValue, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}

/// `sanitizeOptionalJson`: drops `undefined` members and empty objects.
#[must_use]
pub fn sanitize_optional_json(value: &JsValue) -> Option<JsValue> {
    match value {
        JsValue::Undefined => None,
        JsValue::Array(items) => Some(JsValue::Array(
            items.iter().filter_map(sanitize_optional_json).collect(),
        )),
        JsValue::Object(object) => {
            let mut result = JsObject::new();
            for (key, item) in object.iter() {
                if let Some(sanitized) = sanitize_optional_json(item) {
                    result.insert(key, sanitized);
                }
            }
            (!result.is_empty()).then_some(JsValue::Object(result))
        }
        scalar => Some(scalar.clone()),
    }
}

/// `sanitizeMetadata`: a non-empty object, else nothing.
#[must_use]
pub fn sanitize_metadata(value: Option<&JsValue>) -> Option<JsValue> {
    value
        .and_then(sanitize_optional_json)
        .filter(JsValue::is_object)
}

/// `sanitizeRuntimeInfo`.
fn sanitize_runtime_info(runtime_info: Option<&JsValue>) -> JsValue {
    let Some(info) = runtime_info.filter(|info| truthy(Some(info))) else {
        return JsValue::Undefined;
    };
    let mut sanitized = JsObject::new();
    sanitized.insert("provider", field(info, "provider"));
    sanitized.insert("sessionId", field(info, "sessionId"));
    for key in ["model", "thinkingOptionId", "modeId"] {
        if let Some(value) = defined(info.get(key)) {
            sanitized.insert(key, value.clone());
        }
    }
    if let Some(extra) = sanitize_metadata(info.get("extra")) {
        sanitized.insert("extra", extra);
    }
    JsValue::Object(sanitized)
}

/// `sanitizePersistenceHandle`.
#[must_use]
pub fn sanitize_persistence_handle(handle: Option<&JsValue>) -> JsValue {
    let Some(handle) = handle.filter(|handle| truthy(Some(handle))) else {
        return JsValue::Null;
    };
    let mut sanitized = JsObject::new();
    sanitized.insert("provider", field(handle, "provider"));
    sanitized.insert("sessionId", field(handle, "sessionId"));
    if let Some(native) = defined(handle.get("nativeHandle")) {
        sanitized.insert("nativeHandle", native.clone());
    }
    if let Some(metadata) = sanitize_metadata(handle.get("metadata")) {
        sanitized.insert("metadata", metadata);
    }
    JsValue::Object(sanitized)
}

/// `normalizeFeatures`: a shallow copy of each feature, or `[]`.
fn normalize_features(features: Option<&JsValue>) -> JsValue {
    let Some(JsValue::Array(items)) = features else {
        return JsValue::Array(Vec::new());
    };
    JsValue::Array(
        items
            .iter()
            .map(|feature| JsValue::Object(spread(Some(feature))))
            .collect(),
    )
}

/// `config.toolPolicy.preapproved.map((grant) => ({ ...grant }))`.
fn copy_grants(preapproved: Option<&JsValue>) -> Result<JsValue, JsTypeError> {
    match preapproved {
        Some(JsValue::Array(grants)) => Ok(JsValue::Array(
            grants
                .iter()
                .map(|grant| JsValue::Object(spread(Some(grant))))
                .collect(),
        )),
        None | Some(JsValue::Undefined) => Err(JsTypeError(
            "Cannot read properties of undefined (reading 'map')".to_owned(),
        )),
        Some(JsValue::Null) => Err(JsTypeError(
            "Cannot read properties of null (reading 'map')".to_owned(),
        )),
        Some(_) => Err(JsTypeError(
            "config.toolPolicy.preapproved.map is not a function".to_owned(),
        )),
    }
}

/// `buildSerializableConfig`: `None` when no field survives.
fn build_serializable_config(config: &JsValue) -> Result<Option<JsValue>, JsTypeError> {
    let mut serializable = JsObject::new();
    for key in ["modeId", "model", "thinkingOptionId"] {
        if let Some(value) = config.get(key).filter(|value| truthy(Some(value))) {
            serializable.insert(key, value.clone());
        }
    }
    // The baseline's `hasOwnProperty` guard changes nothing: a missing or
    // undefined `featureValues` sanitizes to nothing either way.
    if let Some(feature_values) = sanitize_metadata(config.get("featureValues")) {
        serializable.insert("featureValues", feature_values);
    }
    if let Some(options) = defined(config.get("providerOptions"))
        && let Some(sanitized) = sanitize_optional_json(options).filter(JsValue::is_object)
    {
        serializable.insert("providerOptions", sanitized);
    }
    if let Some(policy) = config.get("toolPolicy").filter(|value| truthy(Some(value))) {
        let mut tool_policy = JsObject::new();
        tool_policy.insert("preapproved", copy_grants(policy.get("preapproved"))?);
        serializable.insert("toolPolicy", JsValue::Object(tool_policy));
    }
    for key in ["systemPrompt", "mcpServers"] {
        if let Some(value) = config.get(key).filter(|value| truthy(Some(value))) {
            serializable.insert(key, value.clone());
        }
    }
    Ok((!serializable.is_empty()).then_some(JsValue::Object(serializable)))
}

fn optional_text(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Undefined, |value| text(value))
}

/// `toStoredAgentRecord(agent, { title, createdAt, internal })`.
///
/// # Errors
///
/// Returns the baseline's [`JsTypeError`] when the config's tool policy has
/// no `preapproved` array to copy.
pub fn to_stored_agent_record(
    agent: &ManagedAgentRecordView,
    title: Option<&str>,
    created_at: Option<&str>,
    internal: Option<bool>,
) -> Result<JsValue, JsTypeError> {
    let config = build_serializable_config(&agent.config)?;
    let updated_at = text(&iso_from_millis(agent.updated_at_millis));
    let last_mode_id = agent
        .current_mode_id
        .as_ref()
        .map(|mode| text(mode))
        .or_else(|| {
            config
                .as_ref()
                .and_then(|config| defined(config.get("modeId")).cloned())
        })
        .unwrap_or(JsValue::Null);
    let mut record = JsObject::new();
    record.insert("id", text(&agent.id));
    record.insert("provider", text(&agent.provider));
    record.insert("cwd", text(&agent.cwd));
    record.insert("workspaceId", optional_text(agent.workspace_id.as_ref()));
    record.insert(
        "createdAt",
        created_at.map_or_else(|| text(&iso_from_millis(agent.created_at_millis)), text),
    );
    record.insert("updatedAt", updated_at.clone());
    record.insert("lastActivityAt", updated_at);
    record.insert(
        "lastUserMessageAt",
        agent
            .last_user_message_at_millis
            .map_or(JsValue::Null, |millis| text(&iso_from_millis(millis))),
    );
    record.insert("title", title.map_or(JsValue::Null, text));
    record.insert("labels", agent.labels.clone());
    record.insert("lastStatus", text(&agent.lifecycle));
    record.insert("lastModeId", last_mode_id);
    record.insert("config", config.unwrap_or(JsValue::Null));
    record.insert(
        "runtimeInfo",
        sanitize_runtime_info(agent.runtime_info.as_ref()),
    );
    record.insert("features", normalize_features(agent.features.as_ref()));
    record.insert(
        "persistence",
        sanitize_persistence_handle(agent.persistence.as_ref()),
    );
    record.insert("lastError", optional_text(agent.last_error.as_ref()));
    let (requires, reason, timestamp) = match &agent.attention {
        AgentAttention::None => (false, JsValue::Null, JsValue::Null),
        AgentAttention::Required {
            reason,
            timestamp_millis,
        } => (
            true,
            text(reason),
            text(&iso_from_millis(*timestamp_millis)),
        ),
    };
    record.insert("requiresAttention", JsValue::Bool(requires));
    record.insert("attentionReason", reason);
    record.insert("attentionTimestamp", timestamp);
    record.insert(
        "internal",
        internal.map_or(JsValue::Undefined, JsValue::Bool),
    );
    record.insert("owner", agent.owner.clone().unwrap_or(JsValue::Undefined));
    Ok(JsValue::Object(record))
}

/// The `options` of `applySnapshot`: an override that is present wins even
/// when it holds `undefined` (`hasOwnProperty`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotOverrides {
    pub title: Option<Option<String>>,
    pub internal: Option<Option<bool>>,
}

/// The record `AgentStorage.applySnapshot` writes: the live agent projected
/// over the existing record (title, creation time, internal flag), with an
/// existing `archivedAt` carried over as the last key.
///
/// # Errors
///
/// As [`to_stored_agent_record`].
pub fn apply_snapshot_record(
    agent: &ManagedAgentRecordView,
    existing: Option<&JsValue>,
    overrides: &SnapshotOverrides,
) -> Result<JsValue, JsTypeError> {
    let existing_field = |key: &str| existing.and_then(|record| record.get(key));
    let title = match &overrides.title {
        Some(title) => title.clone(),
        None => existing_field("title")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
    };
    let created_at = existing_field("createdAt")
        .and_then(JsValue::as_str)
        .map(str::to_owned);
    let internal = match overrides.internal {
        Some(internal) => internal,
        None => agent
            .internal
            .or_else(|| existing_field("internal").and_then(JsValue::as_bool)),
    };
    let mut record =
        to_stored_agent_record(agent, title.as_deref(), created_at.as_deref(), internal)?;
    if let Some(archived_at) = defined(existing_field("archivedAt"))
        && let JsValue::Object(object) = &mut record
    {
        object.insert("archivedAt", archived_at.clone());
    }
    Ok(record)
}
