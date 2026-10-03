//! The stored-agent helpers of pinned Paseo `server/persistence-hooks.ts`
//! that agent loading uses: session config and overrides from a
//! `StoredAgentRecord`, its persistence handle, timestamps and attention.
//! Records stay JavaScript values, as `AgentStorage` returns them.

use spocky_contracts::js::{date_parse, js_string, truthy};
use spocky_store::js_value::{JsObject, JsValue};

use crate::agent_projection::AgentAttention;
use crate::runtime_mcp_config::strip_internal_paseo_mcp_server;

/// The config keys `buildConfigOverrides` copies, in order.
const CONFIG_KEYS: [&str; 8] = [
    "modeId",
    "model",
    "thinkingOptionId",
    "featureValues",
    "providerOptions",
    "toolPolicy",
    "systemPrompt",
    "mcpServers",
];

/// `isProviderRegistered(validProviders, provider)`: `None` registers all.
fn is_provider_registered(valid_providers: Option<&[String]>, provider: &str) -> bool {
    valid_providers.is_none_or(|providers| providers.iter().any(|known| known == provider))
}

/// `value ?? undefined`.
fn defined(value: Option<&JsValue>) -> JsValue {
    match value {
        Some(JsValue::Null | JsValue::Undefined) | None => JsValue::Undefined,
        Some(value) => value.clone(),
    }
}

fn provider_of(record: &JsValue) -> JsValue {
    record
        .get("provider")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

/// `buildConfigOverrides(record)`.
#[must_use]
pub fn build_config_overrides(record: &JsValue) -> JsValue {
    let config = record.get("config");
    let mut out = JsObject::new();
    out.insert("provider", provider_of(record));
    out.insert(
        "cwd",
        record.get("cwd").cloned().unwrap_or(JsValue::Undefined),
    );
    for key in CONFIG_KEYS {
        out.insert(key, defined(config.and_then(|config| config.get(key))));
    }
    strip_internal_paseo_mcp_server(&JsValue::Object(out))
}

/// `buildSessionConfig(record, { validProviders })`: `None` for a provider
/// that is not registered.
#[must_use]
pub fn build_session_config(
    record: &JsValue,
    valid_providers: Option<&[String]>,
) -> Option<JsValue> {
    if !is_stored_agent_provider_available(record, valid_providers) {
        return None;
    }
    let overrides = build_config_overrides(record);
    let mut out = JsObject::new();
    out.insert("provider", provider_of(record));
    out.insert(
        "cwd",
        record.get("cwd").cloned().unwrap_or(JsValue::Undefined),
    );
    for key in CONFIG_KEYS {
        out.insert(
            key,
            overrides.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    Some(strip_internal_paseo_mcp_server(&JsValue::Object(out)))
}

/// `isStoredAgentProviderAvailable(record, validProviders)`.
#[must_use]
pub fn is_stored_agent_provider_available(
    record: &JsValue,
    valid_providers: Option<&[String]>,
) -> bool {
    is_provider_registered(valid_providers, &js_string(record.get("provider")))
}

/// `resolveStoredAgentUpdatedAt(record)`: the later of `updatedAt` and
/// `lastActivityAt` by `Date.parse` (any form V8 reads, not only ISO), the
/// raw string of the first on a tie.
#[must_use]
pub fn resolve_stored_agent_updated_at(record: &JsValue) -> JsValue {
    let mut latest: Option<(i64, &JsValue)> = None;
    for key in ["updatedAt", "lastActivityAt"] {
        let Some(value) = record.get(key) else {
            continue;
        };
        let Some(parsed) = value
            .as_str()
            .filter(|text| !text.is_empty())
            .and_then(date_parse)
        else {
            continue;
        };
        if latest.is_none_or(|(best, _)| parsed > best) {
            latest = Some((parsed, value));
        }
    }
    latest.map_or_else(
        || {
            record
                .get("updatedAt")
                .cloned()
                .unwrap_or(JsValue::Undefined)
        },
        |(_, raw)| raw.clone(),
    )
}

/// `extractTimestamps(record)` as epoch milliseconds: `new Date(text)`,
/// `None` where it is an invalid date.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTimestamps {
    pub created_at_millis: Option<i64>,
    pub updated_at_millis: Option<i64>,
    /// `null` when the record has none.
    pub last_user_message_at_millis: Option<i64>,
    pub labels: Option<JsValue>,
    pub workspace_id: Option<String>,
    pub owner: Option<JsValue>,
}

/// `new Date(text)` (`persistence-hooks.ts:143-145`, `:164`): the parser
/// behind `Date.parse`, so every form V8 reads.
fn date_millis(value: &JsValue) -> Option<i64> {
    value.as_str().and_then(date_parse)
}

fn present(value: Option<&JsValue>) -> Option<JsValue> {
    value
        .filter(|value| !matches!(value, JsValue::Undefined))
        .cloned()
}

/// `extractTimestamps(record)`.
#[must_use]
pub fn extract_timestamps(record: &JsValue) -> StoredTimestamps {
    StoredTimestamps {
        created_at_millis: record.get("createdAt").and_then(date_millis),
        updated_at_millis: date_millis(&resolve_stored_agent_updated_at(record)),
        last_user_message_at_millis: record
            .get("lastUserMessageAt")
            .filter(|value| truthy(Some(value)))
            .and_then(date_millis),
        labels: present(record.get("labels")),
        workspace_id: record
            .get("workspaceId")
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        owner: present(record.get("owner")),
    }
}

/// `extractAttention(record)`: unread state survives a resume.
#[must_use]
pub fn extract_attention(record: &JsValue) -> AgentAttention {
    let reason = record
        .get("attentionReason")
        .filter(|value| truthy(Some(value)));
    let timestamp = record
        .get("attentionTimestamp")
        .filter(|value| truthy(Some(value)));
    match (truthy(record.get("requiresAttention")), reason, timestamp) {
        (true, Some(reason), Some(timestamp)) => match date_millis(timestamp) {
            Some(timestamp_millis) => AgentAttention::Required {
                reason: js_string(Some(reason)),
                timestamp_millis,
            },
            None => AgentAttention::None,
        },
        _ => AgentAttention::None,
    }
}

/// `toAgentPersistenceHandle(registeredProviders, handle)`.
#[must_use]
pub fn to_agent_persistence_handle(
    registered_providers: &[String],
    handle: Option<&JsValue>,
) -> Option<JsValue> {
    let handle = handle.filter(|handle| truthy(Some(handle)))?;
    let provider = handle
        .get("provider")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    if !is_provider_registered(Some(registered_providers), &js_string(Some(&provider))) {
        return None;
    }
    let session_id = handle.get("sessionId").filter(|id| truthy(Some(id)))?;
    let mut out = JsObject::new();
    out.insert("provider", provider);
    out.insert("sessionId", session_id.clone());
    for key in ["nativeHandle", "metadata"] {
        if let Some(value) = present(handle.get(key)) {
            out.insert(key, value);
        }
    }
    Some(JsValue::Object(out))
}
