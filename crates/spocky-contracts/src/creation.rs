//! Creation lifecycle snapshots (emit-only).
//!
//! `initialRecord` in `packages/server/src/server/creation/index.ts` builds
//! `kind, idempotencyKey, revision, phase, error, workspaceId, agentId`.
//! Each `publish` spreads the old snapshot and the update, so existing keys
//! keep their slot and new keys append in first-set order: `workspace` and
//! `setupSkippedReason` (workspace provisioned), `agent` (agent ready), and
//! on failure `errorCode`, `failedStage`, `outcomeUnknown`.
//!
//! Receipts read back from disk pass `RecordSchema.parse` and take schema
//! order instead; that replay path is not modeled here.

use serde::Serialize;

use crate::field::optional;
use crate::number::NonNegativeInt;
use crate::request::CreationKind;
use crate::snapshot::AgentSnapshot;
use crate::workspace::WorkspaceDescriptor;

/// `CreationSnapshotSchema.phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreationPhase {
    Accepted,
    WorkspaceReady,
    AgentReady,
    PromptStarted,
    Completed,
    Failed,
}

/// `CreationSnapshotSchema.failedStage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FailedStage {
    Workspace,
    Agent,
    Prompt,
}

/// A creation snapshot in publish order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreationSnapshot {
    pub kind: CreationKind,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
    pub revision: NonNegativeInt,
    pub phase: CreationPhase,
    pub error: Option<String>,
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub workspace: Option<Box<WorkspaceDescriptor>>,
    #[serde(
        rename = "setupSkippedReason",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub setup_skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub agent: Option<AgentSnapshot>,
    #[serde(
        rename = "errorCode",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub error_code: Option<String>,
    #[serde(
        rename = "failedStage",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub failed_stage: Option<FailedStage>,
    #[serde(
        rename = "outcomeUnknown",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub outcome_unknown: Option<bool>,
}
