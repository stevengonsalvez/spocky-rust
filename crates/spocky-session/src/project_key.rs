//! `deriveProjectKey` from pinned Paseo `server/project-key.ts`: an opaque key
//! that joins the same remote across hosts, or the host-local real path.

use crate::git_remote::{is_github_host, parse_git_remote_location};
use crate::paths::{normalize_path_for_identity, realpath_aware_relative_path, resolve};

/// Inputs to [`derive_project_key`].
#[derive(Debug, Clone, Copy)]
pub struct ProjectKeyInput<'a> {
    pub root_path: &'a str,
    pub remote_url: Option<&'a str>,
    pub worktree_root: Option<&'a str>,
    pub main_repo_root: Option<&'a str>,
    pub server_id: Option<&'a str>,
}

/// `deriveProjectKey`.
#[must_use]
pub fn derive_project_key(input: &ProjectKeyInput<'_>) -> String {
    let remote = input.remote_url.and_then(parse_git_remote_location);
    let selected_path = input
        .worktree_root
        .and_then(|root| realpath_aware_relative_path(root, input.root_path))
        .filter(|relative| !relative.is_empty());
    if let Some(remote) = remote {
        let host = match &remote.port {
            Some(port) => format!("{}:{port}", remote.host),
            None => remote.host.clone(),
        };
        let path = if is_github_host(&remote.host) {
            remote.path.to_lowercase()
        } else {
            remote.path.clone()
        };
        let remote_key = format!("remote:{host}/{path}");
        return match selected_path {
            Some(selected) => format!("{remote_key}#subdir:{}", selected.replace('\\', "/")),
            None => remote_key,
        };
    }
    let (base, relative) = match (&selected_path, input.main_repo_root) {
        (Some(selected), Some(main_root)) => (main_root, selected.as_str()),
        _ => (input.root_path, ""),
    };
    let resolved = resolve(&resolve("/", base), relative);
    let local_path = normalize_path_for_identity(&resolved);
    match input.server_id {
        Some(server_id) if !server_id.is_empty() => format!("host:{server_id}:{local_path}"),
        _ => local_path,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectKeyInput, derive_project_key};

    fn key(root: &str, remote: Option<&str>, worktree: Option<&str>, main: Option<&str>) -> String {
        derive_project_key(&ProjectKeyInput {
            root_path: root,
            remote_url: remote,
            worktree_root: worktree,
            main_repo_root: main,
            server_id: Some("srv"),
        })
    }

    #[test]
    fn remote_keys_lowercase_github_paths_and_append_subdir() {
        assert_eq!(
            key(
                "/nonexistent/repo",
                Some("git@github.com:Owner/Repo.git"),
                Some("/nonexistent/repo"),
                None
            ),
            "remote:github.com/owner/repo"
        );
        assert_eq!(
            key(
                "/nonexistent/repo/sub",
                Some("https://git.example:8443/Team/App"),
                Some("/nonexistent/repo"),
                None
            ),
            "remote:git.example:8443/Team/App#subdir:sub"
        );
    }

    #[test]
    fn local_keys_use_host_and_main_repo_for_subdirectories() {
        assert_eq!(
            key("/nonexistent/dir", None, None, None),
            "host:srv:/nonexistent/dir"
        );
        assert_eq!(
            key(
                "/nonexistent/wt/sub",
                None,
                Some("/nonexistent/wt"),
                Some("/nonexistent/main")
            ),
            "host:srv:/nonexistent/main/sub"
        );
        assert_eq!(
            derive_project_key(&ProjectKeyInput {
                root_path: "/nonexistent/dir/",
                remote_url: None,
                worktree_root: None,
                main_repo_root: None,
                server_id: None,
            }),
            "/nonexistent/dir"
        );
    }
}
