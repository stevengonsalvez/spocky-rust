//! Directory workspace provisioning from pinned Paseo
//! `session/workspace-provisioning/workspace-provisioning-service.ts`
//! (`createWorkspaceForDirectory`, `findOrCreateProjectForDirectory`,
//! `refreshProjectKind`) and the checkout placement rules in
//! `workspace-registry-model.ts`.

use spocky_store::StoreError;
use spocky_store::path_compare::are_equivalent_paths;
use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, ProjectAllocation, ProjectKind,
    ProjectRegistry, ProjectRootInput, WorkspaceKind, WorkspaceRegistry,
};
use tokio::sync::Mutex;

use crate::checkout::{CheckoutContext, CheckoutLite, get_checkout};
use crate::clock::{generate_project_id, generate_workspace_id, now_iso};
use crate::git::GitError;
use crate::git_remote::js_trim;
use crate::paths::{basename, resolve_from_cwd};
use crate::project_key::{ProjectKeyInput, derive_project_key};

/// A provisioning failure. `code` is the wire `errorCode` where one exists.
#[derive(Debug)]
pub enum ProvisioningError {
    UnknownProject(String),
    ArchivedProject(String),
    Git(GitError),
    Store(StoreError),
}

impl ProvisioningError {
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::UnknownProject(_) => Some("unknown_project"),
            Self::ArchivedProject(_) => Some("archived_project"),
            Self::Git(_) | Self::Store(_) => None,
        }
    }
}

impl std::fmt::Display for ProvisioningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProject(id) => write!(formatter, "Unknown project: {id}"),
            Self::ArchivedProject(id) => write!(formatter, "Archived project: {id}"),
            Self::Git(error) => error.fmt(formatter),
            Self::Store(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProvisioningError {}

impl From<GitError> for ProvisioningError {
    fn from(error: GitError) -> Self {
        Self::Git(error)
    }
}

impl From<StoreError> for ProvisioningError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// `normalizeBranch`: trimmed, and `HEAD` in any case means detached.
#[must_use]
pub fn normalize_branch(branch: Option<&str>) -> Option<String> {
    branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty() && branch.to_uppercase() != "HEAD")
        .map(str::to_owned)
}

/// `deriveWorkspaceDisplayName`: the branch, else the last cwd segment.
#[must_use]
pub fn derive_workspace_display_name(cwd: &str, checkout: &CheckoutLite) -> String {
    if let Some(branch) = normalize_branch(checkout.current_branch.as_deref()) {
        return branch;
    }
    cwd.replace('\\', "/")
        .split('/')
        .rfind(|segment| !segment.is_empty())
        .map_or_else(|| cwd.to_owned(), str::to_owned)
}

/// The placement fields of a new workspace (`initialWorkspacePlacement`,
/// source `"checkout"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePlacement {
    pub cwd: String,
    pub kind: WorkspaceKind,
    pub display_name: String,
    pub branch: Option<String>,
    pub worktree_root: Option<String>,
    pub is_paseo_owned_worktree: bool,
    pub main_repo_root: Option<String>,
}

#[must_use]
pub fn initial_checkout_placement(cwd: &str, checkout: &CheckoutLite) -> WorkspacePlacement {
    let kind = if !checkout.is_git {
        WorkspaceKind::Directory
    } else if checkout.main_repo_root.is_some() {
        WorkspaceKind::Worktree
    } else {
        WorkspaceKind::LocalCheckout
    };
    WorkspacePlacement {
        cwd: cwd.to_owned(),
        kind,
        display_name: derive_workspace_display_name(cwd, checkout),
        branch: normalize_branch(checkout.current_branch.as_deref()),
        worktree_root: checkout.is_git.then(|| {
            checkout
                .worktree_root
                .clone()
                .unwrap_or_else(|| cwd.to_owned())
        }),
        is_paseo_owned_worktree: checkout.is_git && checkout.is_paseo_owned_worktree,
        main_repo_root: if checkout.is_git {
            checkout.main_repo_root.clone()
        } else {
            None
        },
    }
}

/// A new directory workspace and the project registry writes it caused,
/// which the session publishes as mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorkspace {
    pub workspace: PersistedWorkspaceRecord,
    pub project: PersistedProjectRecord,
    pub project_upserted: bool,
}

/// Registries plus the identity and paths that provisioning needs.
#[derive(Debug)]
pub struct WorkspaceProvisioning {
    pub projects: Mutex<ProjectRegistry>,
    pub workspaces: Mutex<WorkspaceRegistry>,
    pub server_id: Option<String>,
    pub checkout: CheckoutContext,
}

impl WorkspaceProvisioning {
    fn project_key(&self, root_path: &str, checkout: &CheckoutLite) -> String {
        derive_project_key(&ProjectKeyInput {
            root_path,
            remote_url: checkout.remote_url.as_deref(),
            worktree_root: checkout.worktree_root.as_deref(),
            main_repo_root: checkout.main_repo_root.as_deref(),
            server_id: self.server_id.as_deref(),
        })
    }

    /// `findOrCreateProjectForDirectory`: probes the checkout again, as the baseline does.
    async fn find_or_create_project(
        &self,
        cwd: &str,
    ) -> Result<(PersistedProjectRecord, bool), ProvisioningError> {
        let root_path = resolve_from_cwd(cwd);
        let checkout = get_checkout(&root_path, &self.checkout).await?;
        let timestamp = now_iso();
        let display_name = match basename(&root_path) {
            name if name.is_empty() => root_path.clone(),
            name => name,
        };
        let project_key = self.project_key(&root_path, &checkout);
        let allocation = self.projects.lock().await.get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: &root_path,
                kind: if checkout.is_git {
                    ProjectKind::Git
                } else {
                    ProjectKind::NonGit
                },
                display_name: &display_name,
                project_key: Some(&project_key),
                timestamp: &timestamp,
            },
            generate_project_id,
        )?;
        let upserted = !matches!(allocation, ProjectAllocation::Existing(_));
        Ok((allocation.record().clone(), upserted))
    }

    /// `requireActiveProject` then `refreshProjectKind`.
    async fn refresh_project(
        &self,
        project_id: &str,
        cwd: &str,
        checkout: &CheckoutLite,
    ) -> Result<(PersistedProjectRecord, bool), ProvisioningError> {
        let project = self
            .projects
            .lock()
            .await
            .get(project_id)
            .ok_or_else(|| ProvisioningError::UnknownProject(project_id.to_owned()))?;
        if project
            .archived_at
            .as_deref()
            .is_some_and(|at| !at.is_empty())
        {
            return Err(ProvisioningError::ArchivedProject(project_id.to_owned()));
        }
        let project_checkout = if are_equivalent_paths(&project.root_path, cwd) {
            checkout.clone()
        } else {
            get_checkout(&project.root_path, &self.checkout).await?
        };
        let kind = if project_checkout.is_git {
            ProjectKind::Git
        } else {
            ProjectKind::NonGit
        };
        let project_key = self.project_key(&project.root_path, &project_checkout);
        if project.kind == kind && project.project_key.as_deref() == Some(project_key.as_str()) {
            return Ok((project, false));
        }
        let refreshed = PersistedProjectRecord {
            kind,
            project_key: Some(project_key),
            updated_at: now_iso(),
            ..project
        };
        self.projects.lock().await.upsert(refreshed.clone())?;
        Ok((refreshed, true))
    }

    /// `createWorkspaceForDirectory`: always a new workspace record.
    ///
    /// # Errors
    ///
    /// Returns the baseline errors: unknown or archived project, a rejected
    /// git probe, or a failed registry write.
    pub async fn create_workspace_for_directory(
        &self,
        cwd: &str,
        title: Option<&str>,
        project_id: Option<&str>,
        workspace_id: Option<String>,
    ) -> Result<CreatedWorkspace, ProvisioningError> {
        let normalized_cwd = resolve_from_cwd(cwd);
        let checkout = get_checkout(&normalized_cwd, &self.checkout).await?;
        let (project, project_upserted) = match project_id {
            Some(project_id) => {
                self.refresh_project(project_id, &normalized_cwd, &checkout)
                    .await?
            }
            None => self.find_or_create_project(&normalized_cwd).await?,
        };
        let timestamp = now_iso();
        let placement = initial_checkout_placement(&normalized_cwd, &checkout);
        let workspace = PersistedWorkspaceRecord {
            workspace_id: workspace_id.unwrap_or_else(generate_workspace_id),
            project_id: project.project_id.clone(),
            cwd: placement.cwd,
            kind: placement.kind,
            display_name: placement.display_name,
            title: title
                .map(js_trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned),
            branch: placement.branch,
            worktree_root: placement.worktree_root,
            base_branch: None,
            is_paseo_owned_worktree: placement.is_paseo_owned_worktree,
            main_repo_root: placement.main_repo_root,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            archived_at: None,
            auto_archived_change_request_url: None,
            pinned_at: None,
            labels: None,
            untrusted_source: None,
        };
        self.workspaces.lock().await.upsert(workspace.clone())?;
        Ok(CreatedWorkspace {
            workspace,
            project,
            project_upserted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckoutLite, derive_workspace_display_name, normalize_branch};

    fn checkout(branch: Option<&str>) -> CheckoutLite {
        CheckoutLite {
            cwd: "/tmp/p".to_owned(),
            is_git: true,
            current_branch: branch.map(str::to_owned),
            remote_url: None,
            worktree_root: Some("/tmp/p".to_owned()),
            is_paseo_owned_worktree: false,
            main_repo_root: None,
        }
    }

    #[test]
    fn branch_and_display_name_rules() {
        assert_eq!(normalize_branch(Some(" main ")).as_deref(), Some("main"));
        assert_eq!(normalize_branch(Some("head")), None);
        assert_eq!(normalize_branch(Some("  ")), None);
        assert_eq!(
            derive_workspace_display_name("/tmp/p", &checkout(Some("feat/x"))),
            "feat/x"
        );
        assert_eq!(
            derive_workspace_display_name("/tmp/proj/", &checkout(Some("HEAD"))),
            "proj"
        );
        assert_eq!(derive_workspace_display_name("/", &checkout(None)), "/");
    }
}
