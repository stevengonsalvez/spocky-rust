//! Workspace payloads as the pinned daemon builds them (emit-only).
//!
//! Sources: `describeWorkspaceRecord`, `describeWorkspaceRecordWithGitData`,
//! `describeCreatedWorktreeWorkspace`, and `buildProjectPlacementForWorkspace`
//! in `packages/server/src/server/session.ts`, and
//! `checkoutFromPersistedWorkspacePlacement` in `workspace-registry-model.ts`.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::field::{Nullable, optional};
use crate::json::JsonValue;
use crate::number::JsNumber;
use crate::text::JsText;

/// `projectKind` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Git,
    NonGit,
    Directory,
}

/// `workspaceKind` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Directory,
    LocalCheckout,
    Checkout,
    Worktree,
}

/// `WorkspaceStateBucketSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

/// `{ additions, deletions }` from the git snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DiffStat {
    pub additions: JsNumber,
    pub deletions: JsNumber,
}

/// `{ ahead, behind }`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AheadBehind {
    pub ahead: JsNumber,
    pub behind: JsNumber,
}

/// `buildWorkspaceGitRuntimePayload`, emitted for git checkouts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GitRuntime {
    #[serde(rename = "currentBranch")]
    pub current_branch: Option<JsText>,
    #[serde(rename = "remoteUrl")]
    pub remote_url: Option<JsText>,
    #[serde(rename = "isPaseoOwnedWorktree")]
    pub is_paseo_owned_worktree: bool,
    #[serde(rename = "isDirty")]
    pub is_dirty: Option<bool>,
    #[serde(rename = "aheadBehind")]
    pub ahead_behind: Option<AheadBehind>,
    #[serde(rename = "aheadOfOrigin")]
    pub ahead_of_origin: Option<JsNumber>,
    #[serde(rename = "behindOfOrigin")]
    pub behind_of_origin: Option<JsNumber>,
}

/// `{ message }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeError {
    pub message: JsText,
}

/// `buildWorkspaceGitHubRuntimePayload`. `pullRequest` is the forge
/// service's object, carried as built.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GitHubRuntime {
    #[serde(rename = "featuresEnabled")]
    pub features_enabled: bool,
    #[serde(rename = "pullRequest")]
    pub pull_request: Option<JsonValue>,
    pub error: Option<ForgeError>,
}

/// `ProjectPlacementPayload.checkout` from
/// `checkoutFromPersistedWorkspacePlacement`, whose two branches build keys
/// in different orders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementCheckout {
    /// `workspace.kind === "directory"`: `cwd, isGit, currentBranch,
    /// remoteUrl, worktreeRoot, isPaseoOwnedWorktree, mainRepoRoot`, all
    /// fixed except `cwd`.
    NotGit { cwd: JsText },
    /// Any git workspace: `cwd, currentBranch, remoteUrl: null,
    /// worktreeRoot, isGit: true, isPaseoOwnedWorktree, mainRepoRoot`.
    Git {
        cwd: JsText,
        current_branch: Option<JsText>,
        worktree_root: JsText,
        is_paseo_owned_worktree: bool,
        main_repo_root: Option<JsText>,
    },
}

impl Serialize for PlacementCheckout {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("ProjectCheckoutLite", 7)?;
        match self {
            Self::NotGit { cwd } => {
                out.serialize_field("cwd", cwd)?;
                out.serialize_field("isGit", &false)?;
                out.serialize_field("currentBranch", &())?;
                out.serialize_field("remoteUrl", &())?;
                out.serialize_field("worktreeRoot", &())?;
                out.serialize_field("isPaseoOwnedWorktree", &false)?;
                out.serialize_field("mainRepoRoot", &())?;
            }
            Self::Git {
                cwd,
                current_branch,
                worktree_root,
                is_paseo_owned_worktree,
                main_repo_root,
            } => {
                out.serialize_field("cwd", cwd)?;
                out.serialize_field("currentBranch", current_branch)?;
                out.serialize_field("remoteUrl", &())?;
                out.serialize_field("worktreeRoot", worktree_root)?;
                out.serialize_field("isGit", &true)?;
                out.serialize_field("isPaseoOwnedWorktree", is_paseo_owned_worktree)?;
                out.serialize_field("mainRepoRoot", main_repo_root)?;
            }
        }
        out.end()
    }
}

/// `buildProjectPlacementForWorkspace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectPlacement {
    #[serde(rename = "projectKey")]
    pub project_key: JsText,
    #[serde(rename = "projectName")]
    pub project_name: JsText,
    /// `resolveWorkspaceDisplayName`, always a string.
    #[serde(rename = "workspaceName")]
    pub workspace_name: JsText,
    pub checkout: PlacementCheckout,
}

/// `WorkspaceDescriptorPayload` in daemon construction order.
///
/// `describeWorkspaceRecord` ends at `project`; the git-data variant spreads
/// it, overwrites `name` and `diffStat` in place, and appends `gitRuntime`
/// (dropped when not git), `githubRuntime`, and `forge`. The created-worktree
/// variant has no `project` and sets `gitRuntime` and `githubRuntime` itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceDescriptor {
    pub id: JsText,
    #[serde(rename = "projectId")]
    pub project_id: JsText,
    #[serde(rename = "projectDisplayName")]
    pub project_display_name: JsText,
    #[serde(rename = "projectCustomName")]
    pub project_custom_name: Option<JsText>,
    #[serde(rename = "projectCustomIconRevision")]
    pub project_custom_icon_revision: Option<JsText>,
    #[serde(rename = "projectRootPath")]
    pub project_root_path: JsText,
    #[serde(rename = "workspaceDirectory")]
    pub workspace_directory: JsText,
    /// Present only for Paseo-owned worktrees.
    #[serde(
        rename = "worktreeSlug",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub worktree_slug: Option<JsText>,
    #[serde(rename = "projectKind")]
    pub project_kind: ProjectKind,
    #[serde(rename = "workspaceKind")]
    pub workspace_kind: WorkspaceKind,
    pub name: JsText,
    pub title: Option<JsText>,
    #[serde(rename = "pinnedAt")]
    pub pinned_at: Option<JsText>,
    /// Spread in only when non-empty.
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub labels: Option<Vec<JsText>>,
    #[serde(rename = "archivingAt")]
    pub archiving_at: Option<JsText>,
    pub status: WorkspaceStatus,
    #[serde(rename = "statusEnteredAt")]
    pub status_entered_at: Option<JsText>,
    #[serde(rename = "activityAt")]
    pub activity_at: Option<JsText>,
    #[serde(rename = "diffStat")]
    pub diff_stat: Option<DiffStat>,
    /// The workspace scripts service's payloads, carried as built.
    pub scripts: Vec<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub project: Option<ProjectPlacement>,
    #[serde(
        rename = "gitRuntime",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub git_runtime: Option<GitRuntime>,
    #[serde(
        rename = "githubRuntime",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub github_runtime: Option<Nullable<GitHubRuntime>>,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub forge: Option<JsText>,
}
