//! `describeWorkspaceRecord` and `buildProjectPlacementForWorkspace` from
//! pinned Paseo `session.ts`, with `checkoutFromPersistedWorkspacePlacement`
//! from `workspace-registry-model.ts`.
//!
//! The session's runtime inputs come from the caller: the workspace git
//! service's `peekSnapshot(workspace.cwd)` as [`GitSnapshotPeek`] and the
//! workspace scripts service's `buildSnapshot(workspace, project)` as
//! `scripts`. The caller also resolves the project record
//! (`projectRegistry.get(workspace.projectId)`).

use spocky_contracts::json::JsonValue;
use spocky_contracts::text::JsText;
use spocky_contracts::workspace::{
    DiffStat, PlacementCheckout, ProjectKind, ProjectPlacement, WorkspaceDescriptor, WorkspaceKind,
    WorkspaceStatus,
};
use spocky_store::registry::{
    self, PersistedProjectRecord, PersistedWorkspaceRecord, resolve_project_display_name,
    resolve_workspace_display_name,
};

use crate::paths::basename;

/// The fields of a `WorkspaceGitRuntimeSnapshot` these builders read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitSnapshotPeek {
    /// `snapshot.git.diffStat`.
    pub diff_stat: Option<DiffStat>,
    /// `snapshot.git.currentBranch`.
    pub current_branch: Option<String>,
    /// `snapshot.git.repoRoot`.
    pub repo_root: Option<String>,
}

fn text(value: &str) -> JsText {
    JsText::from_js(value.to_owned())
}

fn optional_text(value: Option<&str>) -> Option<JsText> {
    value.map(text)
}

/// `checkoutFromPersistedWorkspacePlacement`.
#[must_use]
pub fn checkout_from_persisted_workspace_placement(
    workspace: &PersistedWorkspaceRecord,
    fallback_branch: Option<&str>,
    fallback_worktree_root: Option<&str>,
) -> PlacementCheckout {
    if workspace.kind == registry::WorkspaceKind::Directory {
        return PlacementCheckout::NotGit {
            cwd: text(&workspace.cwd),
        };
    }
    let paseo_owned = workspace.is_paseo_owned_worktree && workspace.main_repo_root.is_some();
    PlacementCheckout::Git {
        cwd: text(&workspace.cwd),
        current_branch: optional_text(workspace.branch.as_deref().or(fallback_branch)),
        worktree_root: text(
            workspace
                .worktree_root
                .as_deref()
                .or(fallback_worktree_root)
                .unwrap_or(&workspace.cwd),
        ),
        is_paseo_owned_worktree: paseo_owned,
        main_repo_root: optional_text(workspace.main_repo_root.as_deref()),
    }
}

/// `buildProjectPlacementForWorkspace` with the project already resolved.
#[must_use]
pub fn build_project_placement(
    workspace: &PersistedWorkspaceRecord,
    project: &PersistedProjectRecord,
    snapshot: Option<&GitSnapshotPeek>,
) -> ProjectPlacement {
    ProjectPlacement {
        project_key: text(&project.project_id),
        project_name: text(resolve_project_display_name(project)),
        workspace_name: text(resolve_workspace_display_name(workspace)),
        checkout: checkout_from_persisted_workspace_placement(
            workspace,
            snapshot.and_then(|snapshot| snapshot.current_branch.as_deref()),
            snapshot.and_then(|snapshot| snapshot.repo_root.as_deref()),
        ),
    }
}

fn workspace_kind(kind: registry::WorkspaceKind) -> WorkspaceKind {
    match kind {
        registry::WorkspaceKind::LocalCheckout => WorkspaceKind::LocalCheckout,
        registry::WorkspaceKind::Worktree => WorkspaceKind::Worktree,
        registry::WorkspaceKind::Directory => WorkspaceKind::Directory,
    }
}

/// `describeWorkspaceRecord(workspace, projectRecord)`.
#[must_use]
pub fn describe_workspace(
    workspace: &PersistedWorkspaceRecord,
    project: Option<&PersistedProjectRecord>,
    snapshot: Option<&GitSnapshotPeek>,
    scripts: Vec<JsonValue>,
) -> WorkspaceDescriptor {
    let worktree_slug = match workspace.worktree_root.as_deref() {
        Some(root) if workspace.is_paseo_owned_worktree && !root.is_empty() => {
            Some(text(&basename(root)))
        }
        _ => None,
    };
    let labels = workspace
        .labels
        .as_ref()
        .filter(|labels| !labels.is_empty())
        .map(|labels| labels.iter().map(|label| text(label)).collect());
    WorkspaceDescriptor {
        id: text(&workspace.workspace_id),
        project_id: text(&workspace.project_id),
        project_display_name: text(
            project.map_or(workspace.project_id.as_str(), resolve_project_display_name),
        ),
        project_custom_name: optional_text(project.and_then(|p| p.custom_name.as_deref())),
        project_custom_icon_revision: optional_text(
            project.and_then(|p| p.custom_icon_revision.as_deref()),
        ),
        project_root_path: text(project.map_or(workspace.cwd.as_str(), |p| p.root_path.as_str())),
        workspace_directory: text(&workspace.cwd),
        worktree_slug,
        project_kind: match project.map(|p| p.kind) {
            Some(registry::ProjectKind::Git) => ProjectKind::Git,
            _ => ProjectKind::NonGit,
        },
        workspace_kind: workspace_kind(workspace.kind),
        name: text(resolve_workspace_display_name(workspace)),
        title: optional_text(workspace.title.as_deref()),
        pinned_at: optional_text(workspace.pinned_at.as_deref()),
        labels,
        archiving_at: None,
        status: WorkspaceStatus::Done,
        status_entered_at: None,
        activity_at: None,
        diff_stat: snapshot.and_then(|snapshot| snapshot.diff_stat),
        scripts,
        project: project.map(|project| build_project_placement(workspace, project, snapshot)),
        git_runtime: None,
        github_runtime: None,
        forge: None,
    }
}
