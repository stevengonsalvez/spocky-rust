//! Agent enums shared by requests and snapshots.

use serde::{Deserialize, Serialize};

/// `AgentStatusSchema` over `AGENT_LIFECYCLE_STATUSES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Initializing,
    Idle,
    Running,
    Error,
    Closed,
}

/// `attentionReason` values: `"finished" | "error" | "permission"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionReason {
    Finished,
    Error,
    Permission,
}
