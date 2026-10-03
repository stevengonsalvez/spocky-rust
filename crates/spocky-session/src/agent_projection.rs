//! Stored agent record projection from pinned Paseo
//! `agent/agent-projections.ts` (`toStoredAgentRecord` and its sanitizers)
//! and `agent/agent-storage.ts` (`applySnapshot`).
//!
//! Provider-defined parts of a live agent (config, runtime info, features,
//! persistence handle, owner) stay JavaScript values, so the record keeps the
//! key order and `undefined` slots the baseline's object literals produce.

use spocky_store::js_value::{JsObject, JsValue};

use crate::clock::iso_from_millis;
use crate::timeline::JsTypeError;
use spocky_contracts::js::{spread, truthy};

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
    let (requires, reason, timestamp) = attention_fields(&agent.attention);
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

/// The extra live-agent fields `toAgentPayload` reads.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPayloadView {
    pub record: ManagedAgentRecordView,
    /// `agent.capabilities` (`AgentCapabilityFlags`).
    pub capabilities: JsValue,
    /// `agent.availableModes` (`AgentMode[]`).
    pub available_modes: Vec<JsValue>,
    /// `agent.pendingPermissions` values in map order.
    pub pending_permissions: Vec<JsValue>,
    pub active_turn_id: Option<String>,
    pub active_turn_started_at_millis: Option<i64>,
    /// `agent.lastUsage` (`AgentUsage`).
    pub last_usage: Option<JsValue>,
}

/// `normalizeThinkingOptionId`.
fn normalize_thinking_option_id(value: Option<&JsValue>) -> JsValue {
    value
        .and_then(JsValue::as_str)
        .map(crate::text::js_trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map_or(JsValue::Null, text)
}

/// `resolveEffectiveThinkingOptionId`.
fn resolve_effective_thinking_option_id(runtime_info: &JsValue, configured: &JsValue) -> JsValue {
    match runtime_info.get("thinkingOptionId") {
        Some(value) => normalize_thinking_option_id(Some(value)),
        None => normalize_thinking_option_id(Some(configured)),
    }
}

/// `sanitizeMetadataArray`.
fn sanitize_metadata_array(value: Option<&JsValue>) -> JsValue {
    let Some(JsValue::Array(items)) = value else {
        return JsValue::Undefined;
    };
    let sanitized: Vec<JsValue> = items
        .iter()
        .filter_map(|item| sanitize_metadata(Some(item)))
        .collect();
    if sanitized.is_empty() {
        JsValue::Undefined
    } else {
        JsValue::Array(sanitized)
    }
}

/// `sanitizePendingPermissions`: `Object.assign({}, request, { input,
/// suggestions, actions, metadata })`. Non-array `actions` throw the
/// baseline's `TypeError` from `request.actions?.map`.
fn sanitize_pending_permissions(pending: &[JsValue]) -> Result<JsValue, JsTypeError> {
    pending
        .iter()
        .map(|request| {
            let mut copy = spread(Some(request));
            copy.insert(
                "input",
                sanitize_metadata(request.get("input")).unwrap_or(JsValue::Undefined),
            );
            copy.insert(
                "suggestions",
                sanitize_metadata_array(request.get("suggestions")),
            );
            // `request.actions?.map(...)`: only nullish skips the map.
            let actions = match request.get("actions") {
                None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Undefined,
                Some(JsValue::Array(actions)) => JsValue::Array(
                    actions
                        .iter()
                        .map(|action| JsValue::Object(spread(Some(action))))
                        .collect(),
                ),
                Some(_) => {
                    return Err(JsTypeError(
                        "request.actions?.map is not a function".to_owned(),
                    ));
                }
            };
            copy.insert("actions", actions);
            copy.insert(
                "metadata",
                sanitize_metadata(request.get("metadata")).unwrap_or(JsValue::Undefined),
            );
            Ok(JsValue::Object(copy))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(JsValue::Array)
}

/// `projectPersistenceHandleForWire`: the sanitized handle without
/// `metadata.mcpServers`, and without `metadata` once that empties it.
fn project_persistence_handle_for_wire(handle: Option<&JsValue>) -> JsValue {
    let projected = sanitize_persistence_handle(handle);
    let JsValue::Object(object) = &projected else {
        return projected;
    };
    let Some(JsValue::Object(metadata)) = object.get("metadata") else {
        return projected;
    };
    let mut kept = JsObject::new();
    for (key, value) in metadata.iter() {
        if key != "mcpServers" {
            kept.insert(key, value.clone());
        }
    }
    let mut out = JsObject::new();
    for (key, value) in object.iter() {
        if key != "metadata" {
            out.insert(key, value.clone());
        } else if !kept.is_empty() {
            out.insert(key, JsValue::Object(kept.clone()));
        }
    }
    JsValue::Object(out)
}

/// `sanitizeUsage`: finite numeric fields in fixed order; any field that is
/// neither a finite number nor nullish voids the whole usage.
fn sanitize_usage(value: Option<&JsValue>) -> Option<JsValue> {
    let sanitized = value
        .and_then(sanitize_optional_json)
        .filter(JsValue::is_object)?;
    let mut result = JsObject::new();
    for field in [
        "inputTokens",
        "cachedInputTokens",
        "outputTokens",
        "totalCostUsd",
        "contextWindowMaxTokens",
        "contextWindowUsedTokens",
    ] {
        match sanitized.get(field) {
            Some(JsValue::Number(number)) if number.is_finite() => {
                result.insert(field, JsValue::Number(*number));
            }
            None | Some(JsValue::Null) => {}
            Some(_) => return None,
        }
    }
    (!result.is_empty()).then_some(JsValue::Object(result))
}

/// `requiresAttention`, `attentionReason`, `attentionTimestamp`.
fn attention_fields(attention: &AgentAttention) -> (bool, JsValue, JsValue) {
    match attention {
        AgentAttention::None => (false, JsValue::Null, JsValue::Null),
        AgentAttention::Required {
            reason,
            timestamp_millis,
        } => (
            true,
            text(reason),
            text(&iso_from_millis(*timestamp_millis)),
        ),
    }
}

/// `value ?? null`.
fn nullish_to_null(value: Option<&JsValue>) -> JsValue {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Null,
        Some(value) => value.clone(),
    }
}

/// `activeTurn`: `{ turnId, startedAt }` while a turn id is set.
fn active_turn(agent: &AgentPayloadView) -> JsValue {
    match agent.active_turn_id.as_ref().filter(|id| !id.is_empty()) {
        None => JsValue::Null,
        Some(turn_id) => {
            let mut active = JsObject::new();
            active.insert("turnId", text(turn_id));
            active.insert(
                "startedAt",
                agent
                    .active_turn_started_at_millis
                    .map_or(JsValue::Null, |millis| text(&iso_from_millis(millis))),
            );
            JsValue::Object(active)
        }
    }
}

/// `toAgentPayload(agent, { title })`.
///
/// # Errors
///
/// Returns the baseline's [`JsTypeError`] for a pending permission whose
/// `actions` is not an array.
pub fn to_agent_payload(
    agent: &AgentPayloadView,
    title: Option<&str>,
) -> Result<JsValue, JsTypeError> {
    let record = &agent.record;
    let runtime_info = sanitize_runtime_info(record.runtime_info.as_ref());
    let thinking_option_id = nullish_to_null(record.config.get("thinkingOptionId"));
    let effective = resolve_effective_thinking_option_id(&runtime_info, &thinking_option_id);
    let mut payload = JsObject::new();
    payload.insert("id", text(&record.id));
    payload.insert("provider", text(&record.provider));
    payload.insert("cwd", text(&record.cwd));
    if let Some(workspace_id) = record.workspace_id.as_ref().filter(|id| !id.is_empty()) {
        payload.insert("workspaceId", text(workspace_id));
    }
    payload.insert("model", nullish_to_null(record.config.get("model")));
    payload.insert("thinkingOptionId", thinking_option_id);
    payload.insert("effectiveThinkingOptionId", effective);
    if runtime_info.is_object() {
        payload.insert("runtimeInfo", runtime_info);
    }
    payload.insert(
        "createdAt",
        text(&iso_from_millis(record.created_at_millis)),
    );
    payload.insert(
        "updatedAt",
        text(&iso_from_millis(record.updated_at_millis)),
    );
    payload.insert(
        "lastUserMessageAt",
        record
            .last_user_message_at_millis
            .map_or(JsValue::Null, |millis| text(&iso_from_millis(millis))),
    );
    payload.insert("status", text(&record.lifecycle));
    payload.insert("activeTurn", active_turn(agent));
    payload.insert(
        "capabilities",
        JsValue::Object(spread(Some(&agent.capabilities))),
    );
    payload.insert(
        "currentModeId",
        record
            .current_mode_id
            .as_deref()
            .map_or(JsValue::Null, text),
    );
    payload.insert(
        "availableModes",
        JsValue::Array(
            agent
                .available_modes
                .iter()
                .map(|mode| JsValue::Object(spread(Some(mode))))
                .collect(),
        ),
    );
    payload.insert("features", normalize_features(record.features.as_ref()));
    payload.insert(
        "pendingPermissions",
        sanitize_pending_permissions(&agent.pending_permissions)?,
    );
    payload.insert(
        "persistence",
        project_persistence_handle_for_wire(record.persistence.as_ref()),
    );
    payload.insert("title", title.map_or(JsValue::Null, text));
    payload.insert("labels", record.labels.clone());
    if let Some(usage) = sanitize_usage(agent.last_usage.as_ref()) {
        payload.insert("lastUsage", usage);
    }
    if let Some(error) = &record.last_error {
        payload.insert("lastError", text(error));
    }
    let (requires, reason, timestamp) = attention_fields(&record.attention);
    payload.insert("requiresAttention", JsValue::Bool(requires));
    payload.insert("attentionReason", reason);
    payload.insert("attentionTimestamp", timestamp);
    Ok(JsValue::Object(payload))
}

/// `value ?? null`.
fn or_null(value: Option<&JsValue>) -> JsValue {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Null,
        Some(value) => value.clone(),
    }
}

/// `new Date(text).toISOString()` (`agent-projections.ts:214-216`), or its
/// `RangeError`; `new Date(text)` parses like `Date.parse`.
fn iso_date(value: &JsValue) -> Result<String, crate::agent_sdk::AgentError> {
    value
        .as_str()
        .and_then(spocky_contracts::js::date_parse)
        .map(iso_from_millis)
        .ok_or_else(|| {
            crate::agent_sdk::AgentError::named(
                "RangeError".to_owned(),
                "Invalid time value".to_owned(),
            )
        })
}

/// `buildStoredRuntimeInfo(record)`.
fn build_stored_runtime_info(record: &JsValue) -> Option<JsValue> {
    let runtime_info = record.get("runtimeInfo").filter(|ri| truthy(Some(ri)))?;
    let mut out = JsObject::new();
    out.insert("provider", field(runtime_info, "provider"));
    out.insert("sessionId", field(runtime_info, "sessionId"));
    for key in ["model", "thinkingOptionId", "modeId"] {
        if runtime_info.get(key).is_some() {
            out.insert(key, or_null(runtime_info.get(key)));
        }
    }
    if let Some(extra) = runtime_info
        .get("extra")
        .filter(|extra| truthy(Some(extra)))
    {
        out.insert("extra", extra.clone());
    }
    Some(JsValue::Object(out))
}

/// `normalizeLabels(labels)`: the string-valued labels.
fn normalize_labels(labels: Option<&JsValue>) -> JsValue {
    let mut out = JsObject::new();
    if let Some(JsValue::Object(labels)) = labels {
        for (key, value) in labels.iter() {
            if matches!(value, JsValue::String(_)) {
                out.insert(key, value.clone());
            }
        }
    }
    JsValue::Object(out)
}

/// `buildStoredAgentPayload(record, validProviders)`: the wire payload of a
/// stored agent that is not loaded.
///
/// # Errors
///
/// The `RangeError` of `toISOString` for an invalid stored date.
pub fn build_stored_agent_payload(
    record: &JsValue,
    valid_providers: &[String],
) -> Result<JsValue, crate::agent_sdk::AgentError> {
    use crate::persistence_hooks::{
        is_stored_agent_provider_available, resolve_stored_agent_updated_at,
        to_agent_persistence_handle,
    };
    let created_at = iso_date(&field(record, "createdAt"))?;
    let updated_at = iso_date(&resolve_stored_agent_updated_at(record))?;
    let last_user_message_at = match record
        .get("lastUserMessageAt")
        .filter(|at| truthy(Some(at)))
    {
        Some(at) => JsValue::String(iso_date(at)?),
        None => JsValue::Null,
    };
    let runtime_info = build_stored_runtime_info(record);
    let provider_available = is_stored_agent_provider_available(record, Some(valid_providers));
    let handle = provider_available
        .then(|| to_agent_persistence_handle(valid_providers, record.get("persistence")))
        .flatten();
    let config = record.get("config");
    let configured_thinking = or_null(config.and_then(|config| config.get("thinkingOptionId")));
    let mut payload = JsObject::new();
    payload.insert("id", field(record, "id"));
    payload.insert("provider", field(record, "provider"));
    payload.insert("cwd", field(record, "cwd"));
    if let Some(workspace_id) = record.get("workspaceId").filter(|id| truthy(Some(id))) {
        payload.insert("workspaceId", workspace_id.clone());
    }
    payload.insert(
        "model",
        or_null(config.and_then(|config| config.get("model"))),
    );
    payload.insert("thinkingOptionId", configured_thinking.clone());
    payload.insert(
        "effectiveThinkingOptionId",
        resolve_effective_thinking_option_id(
            runtime_info.as_ref().unwrap_or(&JsValue::Undefined),
            &configured_thinking,
        ),
    );
    if let Some(runtime_info) = runtime_info {
        payload.insert("runtimeInfo", runtime_info);
    }
    payload.insert("createdAt", text(&created_at));
    payload.insert("updatedAt", text(&updated_at));
    payload.insert("lastUserMessageAt", last_user_message_at);
    payload.insert("status", field(record, "lastStatus"));
    let mut capabilities = JsObject::new();
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
        capabilities.insert(key, JsValue::Bool(value));
    }
    payload.insert("capabilities", JsValue::Object(capabilities));
    payload.insert("currentModeId", or_null(record.get("lastModeId")));
    payload.insert("availableModes", JsValue::Array(Vec::new()));
    payload.insert("pendingPermissions", JsValue::Array(Vec::new()));
    payload.insert(
        "persistence",
        project_persistence_handle_for_wire(handle.as_ref()),
    );
    payload.insert("title", or_null(record.get("title")));
    payload.insert(
        "requiresAttention",
        match record.get("requiresAttention") {
            None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Bool(false),
            Some(value) => value.clone(),
        },
    );
    payload.insert("attentionReason", or_null(record.get("attentionReason")));
    payload.insert(
        "attentionTimestamp",
        or_null(record.get("attentionTimestamp")),
    );
    payload.insert("archivedAt", or_null(record.get("archivedAt")));
    payload.insert("labels", normalize_labels(record.get("labels")));
    if !provider_available {
        payload.insert("providerUnavailable", JsValue::Bool(true));
    }
    Ok(JsValue::Object(payload))
}
