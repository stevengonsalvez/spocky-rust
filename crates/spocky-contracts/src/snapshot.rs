//! `AgentSnapshotPayload` as the pinned daemon builds it (emit-only).
//!
//! Two construction sites in `packages/server/src/server/agent/
//! agent-projections.ts` give two key orders: `toAgentPayload` for a live
//! agent, followed by `enrichAgentPayload` in `session.ts`, and
//! `buildStoredAgentPayload` for an agent that is only on disk.

use serde::{Serialize, Serializer};

use crate::agent::{AgentStatus, AttentionReason};
use crate::field::{Nullable, optional};
use crate::json::{JsRecord, JsonValue, serialize_passthrough};
use crate::number::JsNumber;

/// `AgentCapabilityFlags`, a `.catchall(z.boolean())` object. `cloneCapabilities`
/// copies the provider's object, so known flags keep the provider's order
/// (`codex-app-server-agent.ts:213-224`, which is the schema order) and other
/// flags follow.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct CapabilityFlagsKnown {
    #[serde(rename = "supportsStreaming")]
    pub supports_streaming: bool,
    #[serde(rename = "supportsSessionPersistence")]
    pub supports_session_persistence: bool,
    #[serde(
        rename = "supportsSessionListing",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub supports_session_listing: Option<bool>,
    #[serde(rename = "supportsDynamicModes")]
    pub supports_dynamic_modes: bool,
    #[serde(rename = "supportsMcpServers")]
    pub supports_mcp_servers: bool,
    #[serde(rename = "supportsReasoningStream")]
    pub supports_reasoning_stream: bool,
    #[serde(rename = "supportsToolInvocations")]
    pub supports_tool_invocations: bool,
    #[serde(rename = "supportsRewindConversation")]
    pub supports_rewind_conversation: bool,
    #[serde(rename = "supportsRewindFiles")]
    pub supports_rewind_files: bool,
    #[serde(rename = "supportsRewindBoth")]
    pub supports_rewind_both: bool,
}

/// Capability flags with any provider-specific extra booleans.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityFlags {
    pub known: CapabilityFlagsKnown,
    pub extra: JsRecord<JsonValue>,
}

impl CapabilityFlags {
    /// `buildStoredAgentPayload`'s `defaultCapabilities`, which has no
    /// `supportsSessionListing`.
    #[must_use]
    pub fn stored_default() -> Self {
        Self {
            known: CapabilityFlagsKnown {
                supports_streaming: false,
                supports_session_persistence: true,
                supports_session_listing: None,
                supports_dynamic_modes: false,
                supports_mcp_servers: false,
                supports_reasoning_stream: false,
                supports_tool_invocations: true,
                supports_rewind_conversation: false,
                supports_rewind_files: false,
                supports_rewind_both: false,
            },
            extra: JsRecord::new(),
        }
    }
}

impl Serialize for CapabilityFlags {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_passthrough(&self.known, &self.extra, serializer)
    }
}

/// `AgentMode` as `{ ...mode }` copies it; providers build it in schema order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentMode {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub icon: Option<String>,
    #[serde(
        rename = "colorTier",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub color_tier: Option<String>,
}

/// `AgentSelectOptionSchema`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentSelectOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub description: Option<String>,
    #[serde(
        rename = "isDefault",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub is_default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub metadata: Option<JsRecord<JsonValue>>,
}

/// `AgentFeatureSchema`, built as in `codex-feature-definitions.ts:17-70`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum AgentFeature {
    #[serde(rename = "toggle")]
    Toggle {
        id: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        tooltip: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        icon: Option<String>,
        value: bool,
    },
    #[serde(rename = "select")]
    Select {
        id: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        tooltip: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
        icon: Option<String>,
        value: Option<String>,
        options: Vec<AgentSelectOption>,
    },
}

/// `AgentPersistenceHandle` after `projectPersistenceHandleForWire`:
/// `metadata` loses `mcpServers` and is dropped when empty.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersistenceHandle {
    pub provider: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(
        rename = "nativeHandle",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub native_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub metadata: Option<JsRecord<JsonValue>>,
}

/// `AgentRuntimeInfo` after `sanitizeRuntimeInfo`: each optional key is
/// present when the provider set it, possibly to `null`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuntimeInfo {
    pub provider: String,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub model: Option<Nullable<String>>,
    #[serde(
        rename = "thinkingOptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub thinking_option_id: Option<Nullable<String>>,
    #[serde(
        rename = "modeId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub mode_id: Option<Nullable<String>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub extra: Option<JsRecord<JsonValue>>,
}

/// `AgentUsage` after `sanitizeUsage`, in its fixed field order.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentUsage {
    #[serde(
        rename = "inputTokens",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub input_tokens: Option<JsNumber>,
    #[serde(
        rename = "cachedInputTokens",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub cached_input_tokens: Option<JsNumber>,
    #[serde(
        rename = "outputTokens",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub output_tokens: Option<JsNumber>,
    #[serde(
        rename = "totalCostUsd",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub total_cost_usd: Option<JsNumber>,
    #[serde(
        rename = "contextWindowMaxTokens",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub context_window_max_tokens: Option<JsNumber>,
    #[serde(
        rename = "contextWindowUsedTokens",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub context_window_used_tokens: Option<JsNumber>,
}

/// `activeTurn` in a live snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveTurn {
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
}

/// Snapshot of a loaded agent: `toAgentPayload` then `enrichAgentPayload`,
/// which overwrites `title` in place and appends `archivedAt`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveAgentSnapshot {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    /// Spread in only when the agent has a workspace id.
    #[serde(
        rename = "workspaceId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub workspace_id: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "thinkingOptionId")]
    pub thinking_option_id: Option<String>,
    #[serde(rename = "effectiveThinkingOptionId")]
    pub effective_thinking_option_id: Option<String>,
    #[serde(
        rename = "runtimeInfo",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub runtime_info: Option<RuntimeInfo>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "lastUserMessageAt")]
    pub last_user_message_at: Option<String>,
    pub status: AgentStatus,
    #[serde(rename = "activeTurn")]
    pub active_turn: Option<ActiveTurn>,
    pub capabilities: CapabilityFlags,
    #[serde(rename = "currentModeId")]
    pub current_mode_id: Option<String>,
    #[serde(rename = "availableModes")]
    pub available_modes: Vec<AgentMode>,
    pub features: Vec<AgentFeature>,
    /// `sanitizePendingPermissions` output, built by the provider.
    #[serde(rename = "pendingPermissions")]
    pub pending_permissions: Vec<JsonValue>,
    pub persistence: Option<PersistenceHandle>,
    pub title: Option<String>,
    pub labels: JsRecord<String>,
    #[serde(
        rename = "lastUsage",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub last_usage: Option<AgentUsage>,
    #[serde(
        rename = "lastError",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub last_error: Option<String>,
    #[serde(rename = "requiresAttention")]
    pub requires_attention: bool,
    #[serde(rename = "attentionReason")]
    pub attention_reason: Option<AttentionReason>,
    #[serde(rename = "attentionTimestamp")]
    pub attention_timestamp: Option<String>,
    #[serde(rename = "archivedAt")]
    pub archived_at: Option<String>,
}

/// Snapshot of an agent read from its stored record (`buildStoredAgentPayload`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredAgentSnapshot {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    #[serde(
        rename = "workspaceId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub workspace_id: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "thinkingOptionId")]
    pub thinking_option_id: Option<String>,
    #[serde(rename = "effectiveThinkingOptionId")]
    pub effective_thinking_option_id: Option<String>,
    #[serde(
        rename = "runtimeInfo",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub runtime_info: Option<RuntimeInfo>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "lastUserMessageAt")]
    pub last_user_message_at: Option<String>,
    pub status: AgentStatus,
    pub capabilities: CapabilityFlags,
    #[serde(rename = "currentModeId")]
    pub current_mode_id: Option<String>,
    /// Always `[]` for a stored agent.
    #[serde(rename = "availableModes")]
    pub available_modes: Vec<AgentMode>,
    /// Always `[]` for a stored agent.
    #[serde(rename = "pendingPermissions")]
    pub pending_permissions: Vec<JsonValue>,
    pub persistence: Option<PersistenceHandle>,
    pub title: Option<String>,
    #[serde(rename = "requiresAttention")]
    pub requires_attention: bool,
    #[serde(rename = "attentionReason")]
    pub attention_reason: Option<AttentionReason>,
    #[serde(rename = "attentionTimestamp")]
    pub attention_timestamp: Option<String>,
    #[serde(rename = "archivedAt")]
    pub archived_at: Option<String>,
    pub labels: JsRecord<String>,
    /// Spread in as `true` only when the provider is unavailable.
    #[serde(
        rename = "providerUnavailable",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub provider_unavailable: Option<bool>,
}

/// `AgentSnapshotPayload` in either construction order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AgentSnapshot {
    Live(Box<LiveAgentSnapshot>),
    Stored(Box<StoredAgentSnapshot>),
}
