//! G1 session response payloads in daemon construction order (emit-only).
//!
//! Sources: `packages/server/src/server/session.ts` at Paseo `5de45e2`.

use serde::Serialize;

use crate::creation::CreationSnapshot;
use crate::field::optional;
use crate::json::JsonValue;
use crate::request::{TimelineDirection, TimelineProjection};
use crate::snapshot::AgentSnapshot;
use crate::text::JsText;
use crate::timeline::{TimelineCursor, TimelineEntry, TimelineWindow};
use crate::workspace::{ProjectPlacement, WorkspaceDescriptor};

/// `workspace.create.response`: `requestId, workspace, agent?, creation?,
/// setupSkippedReason?, error, errorCode?, setupTerminalId` (the success and
/// error paths of `handleWorkspaceCreation` share this order).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceCreateResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub workspace: Option<Box<WorkspaceDescriptor>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub agent: Option<AgentSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub creation: Option<Box<CreationSnapshot>>,
    #[serde(
        rename = "setupSkippedReason",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub setup_skipped_reason: Option<JsText>,
    pub error: Option<JsText>,
    #[serde(
        rename = "errorCode",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub error_code: Option<JsText>,
    /// Always `null` from this handler.
    #[serde(rename = "setupTerminalId")]
    pub setup_terminal_id: Option<JsText>,
}

/// `agent.create.response`: `requestId, agent, error, creation?`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentCreateResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub agent: Option<AgentSnapshot>,
    pub error: Option<JsText>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub creation: Option<Box<CreationSnapshot>>,
}

/// `creation.subscribe.response`: `requestId, subscriptionId?, snapshot, error`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreationSubscribeResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
    pub snapshot: Option<Box<CreationSnapshot>>,
    pub error: Option<JsText>,
}

/// `fetch_agent_response`: `requestId, agent, project, error`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchAgentResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub agent: Option<AgentSnapshot>,
    pub project: Option<ProjectPlacement>,
    pub error: Option<JsText>,
}

/// `{ agent, project }` directory entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentDirectoryEntry {
    pub agent: AgentSnapshot,
    pub project: ProjectPlacement,
}

/// `{ nextCursor, prevCursor, hasMore }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageInfo {
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<JsText>,
    #[serde(rename = "prevCursor")]
    pub prev_cursor: Option<JsText>,
    #[serde(rename = "hasMore")]
    pub has_more: bool,
}

/// `fetch_agents_response` without directory sync:
/// `requestId, subscriptionId?, entries, pageInfo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchAgentsResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
    pub entries: Vec<AgentDirectoryEntry>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// `fetch_workspaces_response` without directory sync:
/// `requestId, subscriptionId?, entries, emptyProjects, pageInfo`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchWorkspacesResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
    pub entries: Vec<WorkspaceDescriptor>,
    /// Workspace directory project descriptors, carried as built.
    #[serde(rename = "emptyProjects")]
    pub empty_projects: Vec<JsonValue>,
    #[serde(rename = "pageInfo")]
    pub page_info: PageInfo,
}

/// `fetch_agent_timeline_response` (success and error paths share order).
/// The booleans mirror wire fields one to one.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct FetchAgentTimelineResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(rename = "agentId")]
    pub agent_id: JsText,
    pub agent: Option<AgentSnapshot>,
    pub direction: TimelineDirection,
    pub projection: TimelineProjection,
    pub epoch: JsText,
    pub reset: bool,
    #[serde(rename = "staleCursor")]
    pub stale_cursor: bool,
    pub gap: bool,
    pub window: TimelineWindow,
    #[serde(rename = "startCursor")]
    pub start_cursor: Option<TimelineCursor>,
    #[serde(rename = "endCursor")]
    pub end_cursor: Option<TimelineCursor>,
    #[serde(rename = "hasOlder")]
    pub has_older: bool,
    #[serde(rename = "hasNewer")]
    pub has_newer: bool,
    /// Spread in as `true` only when the request set `mergeWindow: true`.
    #[serde(
        rename = "mergeWindow",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub merge_window: Option<bool>,
    pub entries: Vec<TimelineEntry>,
    pub error: Option<JsText>,
}

/// `wait_for_finish_response.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitStatus {
    Idle,
    Error,
    Permission,
    Timeout,
}

/// `wait_for_finish_response`: `requestId, status, final, error, lastMessage`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WaitForFinishResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    pub status: WaitStatus,
    #[serde(rename = "final")]
    pub final_agent: Option<AgentSnapshot>,
    pub error: Option<JsText>,
    #[serde(rename = "lastMessage")]
    pub last_message: Option<JsText>,
}

/// `send_agent_message_response`: `requestId, agentId, accepted, error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendAgentMessageResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(rename = "agentId")]
    pub agent_id: JsText,
    pub accepted: bool,
    pub error: Option<JsText>,
}

/// `session.events.set_subscription.response`: `requestId, subscriptionId?`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEventsSetSubscriptionResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
}

/// `agent.timeline.set_subscription.response`: `agentIds` (deduplicated and
/// sorted by the daemon), `requestId, subscriptionId?`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetAgentTimelineSubscriptionResponse {
    #[serde(rename = "agentIds")]
    pub agent_ids: Vec<JsText>,
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(
        rename = "subscriptionId",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<JsText>,
}

/// `subscription.release.response`: `requestId, subscriptionId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubscriptionReleaseResponse {
    #[serde(rename = "requestId")]
    pub request_id: JsText,
    #[serde(rename = "subscriptionId")]
    pub subscription_id: JsText,
}
