//! G1 session requests (client to daemon), without the `type` tag, in zod
//! output order. Unknown keys are stripped, as `z.object` does.
//!
//! Source: `packages/protocol/src/messages.ts` at Paseo `5de45e2`.

use serde::{Deserialize, Serialize};

use crate::agent::AgentStatus;
use crate::agent_config::{AgentSessionConfig, CreateAgentWorktreeTarget, GitSetupOptions};
use crate::attachment::{AgentAttachment, ImageAttachment, LenientAttachments};
use crate::field::{Nullable, optional};
use crate::id::{WorkspaceId, ZodUuid};
use crate::json::{JsRecord, JsonValue};
use crate::literal::string_literal;
use crate::number::{NonNegativeInt, PageLimit, PositiveInt};
use crate::text::{BoundedString, NonEmptyString};

/// `z.string().min(1).max(512)`, the creation idempotency key.
pub type IdempotencyKey = BoundedString<1, 512>;

/// `DirectorySyncRequestSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorySyncRequest {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub generation: Option<String>,
    #[serde(
        rename = "afterSeq",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub after_seq: Option<NonNegativeInt>,
}

/// Directory `page`: `{ limit, cursor? }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryPage {
    pub limit: PageLimit,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub cursor: Option<NonEmptyString>,
}

/// Directory `subscribe`: `{ subscriptionId? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectorySubscribe {
    #[serde(
        rename = "subscriptionId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub subscription_id: Option<String>,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

/// `fetch_agents_request.sort[].key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSortKey {
    StatusPriority,
    CreatedAt,
    UpdatedAt,
    Title,
}

/// `fetch_agents_request.sort[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSort {
    pub key: AgentSortKey,
    pub direction: SortDirection,
}

/// `fetch_workspaces_request.sort[].key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSortKey {
    StatusPriority,
    ActivityAt,
    Name,
    ProjectId,
}

/// `fetch_workspaces_request.sort[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSort {
    pub key: WorkspaceSortKey,
    pub direction: SortDirection,
}

/// `AgentDirectoryFilterSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDirectoryFilter {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub labels: Option<JsRecord<String>>,
    #[serde(
        rename = "projectKeys",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub statuses: Option<Vec<AgentStatus>>,
    #[serde(
        rename = "includeArchived",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub include_archived: Option<bool>,
    #[serde(
        rename = "requiresAttention",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub requires_attention: Option<bool>,
    #[serde(
        rename = "thinkingOptionId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub thinking_option_id: Option<Nullable<String>>,
}

string_literal!(ActiveScope = "active");

/// `FetchAgentsRequestMessageSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchAgentsRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub scope: Option<ActiveScope>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub filter: Option<AgentDirectoryFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub sort: Option<Vec<AgentSort>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub page: Option<DirectoryPage>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<DirectorySubscribe>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub sync: Option<DirectorySyncRequest>,
}

/// `fetch_workspaces_request.filter`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDirectoryFilter {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub query: Option<String>,
    #[serde(
        rename = "projectId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_id: Option<String>,
    /// Accepted for older clients; the daemon does not filter on it.
    #[serde(
        rename = "idPrefix",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub id_prefix: Option<String>,
}

/// `FetchWorkspacesRequestMessageSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchWorkspacesRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub filter: Option<WorkspaceDirectoryFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub sort: Option<Vec<WorkspaceSort>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub page: Option<DirectoryPage>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<DirectorySubscribe>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub sync: Option<DirectorySyncRequest>,
}

/// `FetchAgentRequestMessageSchema`. `agentId` accepts a full id, a unique
/// prefix, or an exact full title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchAgentRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
}

/// `ActiveTurnBehaviorSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActiveTurnBehavior {
    Interrupt,
    Steer,
}

/// `SendAgentMessageRequestSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendAgentMessageRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub text: String,
    #[serde(
        rename = "messageId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub message_id: Option<String>,
    #[serde(
        rename = "activeTurnBehavior",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub active_turn_behavior: Option<ActiveTurnBehavior>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub images: Option<Vec<ImageAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub attachments: Option<LenientAttachments>,
}

/// `WaitForFinishRequestSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitForFinishRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(
        rename = "timeoutMs",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub timeout_ms: Option<PositiveInt>,
}

/// Timeline page direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineDirection {
    Tail,
    Before,
    After,
}

/// Timeline projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimelineProjection {
    Projected,
    Canonical,
}

/// `AgentTimelineCursorSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTimelineCursor {
    pub epoch: String,
    pub seq: NonNegativeInt,
}

/// `FetchAgentTimelineRequestMessageSchema`. `limit: 0` means every
/// matching row in the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchAgentTimelineRequest {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub direction: Option<TimelineDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub cursor: Option<AgentTimelineCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub limit: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub projection: Option<TimelineProjection>,
    #[serde(
        rename = "mergeWindow",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub merge_window: Option<bool>,
}

/// `SetAgentTimelineSubscriptionRequestMessageSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetAgentTimelineSubscriptionRequest {
    #[serde(rename = "agentIds")]
    pub agent_ids: Vec<String>,
    #[serde(rename = "requestId")]
    pub request_id: String,
}

/// `SessionEventSubscriptionSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEventSubscription {
    #[serde(rename = "project.update")]
    ProjectUpdate,
    #[serde(rename = "providers_snapshot_update")]
    ProvidersSnapshotUpdate,
    #[serde(rename = "agent_attention_required")]
    AgentAttentionRequired,
    #[serde(rename = "agent_permission_request")]
    AgentPermissionRequest,
    #[serde(rename = "agent_permission_resolved")]
    AgentPermissionResolved,
    #[serde(rename = "checkout_status_update")]
    CheckoutStatusUpdate,
    #[serde(rename = "script_status_update")]
    ScriptStatusUpdate,
    #[serde(rename = "workspace_setup_progress")]
    WorkspaceSetupProgress,
    #[serde(rename = "agent.provider_subagents.update")]
    AgentProviderSubagentsUpdate,
    #[serde(rename = "terminal_attention_required")]
    TerminalAttentionRequired,
    #[serde(rename = "status.server_info")]
    StatusServerInfo,
    #[serde(rename = "status.daemon_config_changed")]
    StatusDaemonConfigChanged,
    #[serde(rename = "status.plugin_catalog_changed")]
    StatusPluginCatalogChanged,
    #[serde(rename = "status.plugin_settings_changed")]
    StatusPluginSettingsChanged,
    #[serde(rename = "activity_log")]
    ActivityLog,
    #[serde(rename = "hub.execution.agent.update")]
    HubExecutionAgentUpdate,
    #[serde(rename = "hub.execution.agent.stream")]
    HubExecutionAgentStream,
}

/// `SessionEventsSetSubscriptionRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventsSetSubscriptionRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub events: Vec<SessionEventSubscription>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub notifications: Option<bool>,
}

/// `SubscriptionReleaseRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionReleaseRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "subscriptionId")]
    pub subscription_id: String,
}

/// `CreationSubscribeRequestSchema.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreationKind {
    Workspace,
    Agent,
}

/// `CreationSubscribeRequestSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationSubscribeRequest {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub kind: CreationKind,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: IdempotencyKey,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<bool>,
}

/// `CreateAgentRequestMessageSchema`, the legacy create path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    #[serde(
        rename = "idempotencyKey",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub idempotency_key: Option<IdempotencyKey>,
    pub config: AgentSessionConfig,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub env: Option<JsRecord<String>>,
    #[serde(
        rename = "workspaceId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub workspace_id: Option<String>,
    #[serde(
        rename = "callerAgentId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub caller_agent_id: Option<String>,
    #[serde(
        rename = "worktreeName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub worktree_name: Option<String>,
    #[serde(
        rename = "initialPrompt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub initial_prompt: Option<String>,
    #[serde(
        rename = "clientMessageId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub client_message_id: Option<String>,
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub output_schema: Option<JsRecord<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub images: Option<Vec<ImageAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub attachments: Option<LenientAttachments>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub git: Option<GitSetupOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub worktree: Option<CreateAgentWorktreeTarget>,
    #[serde(
        rename = "autoArchive",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub auto_archive: Option<bool>,
    /// Defaults to `{}`.
    #[serde(default)]
    pub labels: JsRecord<String>,
    #[serde(rename = "requestId")]
    pub request_id: String,
}

/// `AgentCreateRequestSchema`: the legacy shape extended in place with a
/// strict `attachments` array, then `agentId` and `subscribe` appended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCreateRequest {
    #[serde(
        rename = "idempotencyKey",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub idempotency_key: Option<IdempotencyKey>,
    pub config: AgentSessionConfig,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub env: Option<JsRecord<String>>,
    #[serde(
        rename = "workspaceId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub workspace_id: Option<String>,
    #[serde(
        rename = "callerAgentId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub caller_agent_id: Option<String>,
    #[serde(
        rename = "worktreeName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub worktree_name: Option<String>,
    #[serde(
        rename = "initialPrompt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub initial_prompt: Option<String>,
    #[serde(
        rename = "clientMessageId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub client_message_id: Option<String>,
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub output_schema: Option<JsRecord<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub images: Option<Vec<ImageAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub attachments: Option<Vec<AgentAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub git: Option<GitSetupOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub worktree: Option<CreateAgentWorktreeTarget>,
    #[serde(
        rename = "autoArchive",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub auto_archive: Option<bool>,
    /// Defaults to `{}`.
    #[serde(default)]
    pub labels: JsRecord<String>,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(
        rename = "agentId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub agent_id: Option<ZodUuid>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<bool>,
}

/// `WorkspaceInitialAgentSchema`: [`AgentCreateRequest`] without `type`,
/// `requestId`, `idempotencyKey`, `subscribe`, `workspaceId`, `worktree`,
/// `worktreeName`, and `git`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInitialAgent {
    pub config: AgentSessionConfig,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub env: Option<JsRecord<String>>,
    #[serde(
        rename = "callerAgentId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub caller_agent_id: Option<String>,
    #[serde(
        rename = "initialPrompt",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub initial_prompt: Option<String>,
    #[serde(
        rename = "clientMessageId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub client_message_id: Option<String>,
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub output_schema: Option<JsRecord<JsonValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub images: Option<Vec<ImageAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub attachments: Option<Vec<AgentAttachment>>,
    #[serde(
        rename = "autoArchive",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub auto_archive: Option<bool>,
    /// Defaults to `{}`.
    #[serde(default)]
    pub labels: JsRecord<String>,
    #[serde(
        rename = "agentId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub agent_id: Option<ZodUuid>,
}

/// `FirstAgentContextSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstAgentContext {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub attachments: Option<LenientAttachments>,
}

/// `workspace.create.request` worktree `action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeAction {
    #[serde(rename = "branch-off")]
    BranchOff,
    #[serde(rename = "checkout")]
    Checkout,
}

/// `WorkspaceCreateRequestSchema.source`, discriminated by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorkspaceSource {
    /// An existing local directory or checkout.
    #[serde(rename = "directory")]
    Directory {
        path: String,
        #[serde(
            rename = "projectId",
            default,
            skip_serializing_if = "Option::is_none",
            with = "optional"
        )]
        project_id: Option<String>,
    },
    /// A new Paseo worktree cut from a project's repository.
    #[serde(rename = "worktree")]
    Worktree(WorktreeSource),
}

/// The `worktree` arm of [`WorkspaceSource`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeSource {
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub cwd: Option<String>,
    #[serde(
        rename = "projectId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub action: Option<WorktreeAction>,
    #[serde(
        rename = "refName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub ref_name: Option<NonEmptyString>,
    #[serde(
        rename = "baseBranch",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub base_branch: Option<String>,
    #[serde(
        rename = "branchName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub branch_name: Option<NonEmptyString>,
    #[serde(
        rename = "checkoutSource",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub checkout_source: Option<crate::agent_config::ChangeRequestCheckoutSource>,
    #[serde(
        rename = "githubPrNumber",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub github_pr_number: Option<PositiveInt>,
    #[serde(
        rename = "worktreeSlug",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub worktree_slug: Option<String>,
}

/// `WorkspaceCreateRequestSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceCreateRequest {
    #[serde(
        rename = "workspaceId",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub agent: Option<Box<WorkspaceInitialAgent>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub subscribe: Option<bool>,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(
        rename = "idempotencyKey",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub idempotency_key: Option<IdempotencyKey>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub title: Option<String>,
    #[serde(
        rename = "firstAgentContext",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub first_agent_context: Option<FirstAgentContext>,
    pub source: WorkspaceSource,
}
