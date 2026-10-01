//! `workspaceGitService.getCheckout(cwd)` from pinned Paseo: the checkout
//! facts (`utils/checkout-git.ts` `getCheckoutSnapshotFacts` and
//! `getCheckoutStatus`) reduced by `checkoutLiteFromGitSnapshot`.
//!
//! Commands whose results feed the lite payload, or whose failure rejects
//! the call, run exactly as in the baseline. There is no cache.

// ponytail: the upstream, branch-remote, pull-request lookup, and head-sha
// commands are skipped. Their failures are caught and their results are
// dropped by `getCheckout`, so only timing differs. Port them with the git
// snapshot service (`gitRuntime`).

use std::path::Path;

use crate::git::{GitError, GitOptions, run_git};
use crate::paths::{
    basename, dirname, expand_tilde, realpath, realpath_aware_relative_path, resolve,
};

/// `ProjectCheckoutLitePayload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutLite {
    pub cwd: String,
    pub is_git: bool,
    pub current_branch: Option<String>,
    pub remote_url: Option<String>,
    pub worktree_root: Option<String>,
    pub is_paseo_owned_worktree: bool,
    pub main_repo_root: Option<String>,
}

impl CheckoutLite {
    fn not_git(cwd: String) -> Self {
        Self {
            cwd,
            is_git: false,
            current_branch: None,
            remote_url: None,
            worktree_root: None,
            is_paseo_owned_worktree: false,
            main_repo_root: None,
        }
    }
}

/// Where Paseo-owned worktrees live (`resolvePaseoWorktreesBaseRoot`).
#[derive(Debug, Clone)]
pub struct CheckoutContext {
    pub paseo_home: String,
    pub worktrees_root: Option<String>,
    pub home: String,
}

impl CheckoutContext {
    fn worktrees_base_root(&self) -> String {
        match &self.worktrees_root {
            Some(root) => {
                let expanded = expand_tilde(root, &self.home);
                if expanded.starts_with('/') {
                    resolve("/", &expanded)
                } else {
                    resolve(&resolve("/", &self.paseo_home), &expanded)
                }
            }
            None => format!("{}/worktrees", resolve("/", &self.paseo_home)),
        }
    }
}

async fn git_stdout(args: &[&str], cwd: &Path) -> Result<String, GitError> {
    run_git(args, &GitOptions::read_only(cwd))
        .await
        .map(|output| output.stdout)
}

/// `parseGitRevParsePath`.
fn parse_rev_parse_path(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut lines = trimmed.split('\n');
    let first = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let path = first.trim_end_matches('\r').trim();
    (!path.is_empty() && !path.starts_with("--")).then(|| path.to_owned())
}

fn non_empty(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `getRebaseHeadBranch`.
async fn rebase_head_branch(cwd: &Path, cwd_text: &str) -> Option<String> {
    for name in ["rebase-merge/head-name", "rebase-apply/head-name"] {
        let Ok(stdout) = git_stdout(&["rev-parse", "--git-path", name], cwd).await else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(resolve(cwd_text, stdout.trim())) else {
            continue;
        };
        let head = contents.trim();
        let branch = head.strip_prefix("refs/heads/").unwrap_or(head);
        if !branch.is_empty() {
            return Some(branch.to_owned());
        }
    }
    None
}

/// `getCurrentBranch`.
async fn current_branch(cwd: &Path, cwd_text: &str) -> Option<String> {
    let stdout = git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"], cwd)
        .await
        .ok()?;
    let branch = stdout.trim();
    if branch == "HEAD" {
        return rebase_head_branch(cwd, cwd_text).await;
    }
    (!branch.is_empty()).then(|| branch.to_owned())
}

/// `branchNameFromRef`.
#[must_use]
pub fn branch_name_from_ref(reference: &str) -> String {
    let trimmed = reference.trim();
    if let Some(rest) = trimmed.strip_prefix("refs/heads/") {
        return rest.to_owned();
    }
    if let Some(rest) = trimmed.strip_prefix("refs/remotes/") {
        return rest
            .find('/')
            .map_or_else(|| rest.to_owned(), |slash| rest[slash + 1..].to_owned());
    }
    trimmed
        .strip_prefix("origin/")
        .unwrap_or(trimmed)
        .to_owned()
}

/// `resolveRepositoryDefaultBranch`. The `branch` fallback is not caught.
async fn repository_default_branch(cwd: &Path) -> Result<Option<String>, GitError> {
    if let Ok(stdout) = git_stdout(
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        cwd,
    )
    .await
    {
        let reference = stdout.trim();
        if !reference.is_empty() {
            let remote_short = reference.strip_prefix("refs/remotes/").unwrap_or(reference);
            let local = remote_short.strip_prefix("origin/").unwrap_or(remote_short);
            let local_ref = format!("refs/heads/{local}");
            return Ok(Some(
                if git_stdout(&["show-ref", "--verify", "--quiet", &local_ref], cwd)
                    .await
                    .is_ok()
                {
                    local.to_owned()
                } else {
                    remote_short.to_owned()
                },
            ));
        }
    }
    let stdout = git_stdout(&["branch", "--format=%(refname:short)"], cwd).await?;
    let branches: Vec<&str> = stdout
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    Ok(["main", "master"]
        .into_iter()
        .find(|candidate| branches.contains(candidate))
        .map(str::to_owned))
}

async fn ref_exists(cwd: &Path, reference: &str) -> Result<bool, GitError> {
    run_git(
        &["show-ref", "--verify", "--quiet", reference],
        &GitOptions {
            accept_exit_codes: &[0, 1],
            ..GitOptions::read_only(cwd)
        },
    )
    .await
    .map(|output| output.exit_code == Some(0))
}

/// `resolveBestComparisonBaseRef`; any failure is `None` at the call site.
async fn comparison_base_ref(cwd: &Path, base_ref: &str) -> Option<String> {
    if base_ref.starts_with("refs/heads/") || base_ref.starts_with("refs/remotes/") {
        return ref_exists(cwd, base_ref)
            .await
            .ok()
            .filter(|exists| *exists)
            .map(|_| base_ref.to_owned());
    }
    let local = branch_name_from_ref(base_ref);
    let local_ref = format!("refs/heads/{local}");
    let origin_ref = format!("refs/remotes/origin/{local}");
    // `Promise.all`: either lookup failing rejects the whole resolution.
    let (has_local, has_origin) =
        tokio::join!(ref_exists(cwd, &local_ref), ref_exists(cwd, &origin_ref));
    let (has_local, has_origin) = (has_local.ok()?, has_origin.ok()?);
    if has_origin {
        return Some(format!("origin/{local}"));
    }
    has_local.then_some(local)
}

/// `getMainRepoRootFromCommonDir`, with errors caught to `None`.
async fn main_repo_root(
    cwd: &Path,
    common_dir: Option<&str>,
    context: &CheckoutContext,
) -> Option<String> {
    let normalized = realpath(common_dir?)?;
    if basename(&normalized) == ".git" {
        return Some(dirname(&normalized));
    }
    let stdout = git_stdout(&["worktree", "list", "--porcelain"], cwd)
        .await
        .ok()?;
    let entries = parse_worktree_list(&stdout);
    let worktrees_root = context.worktrees_base_root();
    let candidates: Vec<&WorktreeEntry> = entries
        .iter()
        .filter(|entry| !entry.bare && !is_descendant_path(&entry.path, &worktrees_root))
        .collect();
    let inside_bare: Vec<&&WorktreeEntry> = candidates
        .iter()
        .filter(|entry| is_descendant_path(&entry.path, &normalized))
        .collect();
    inside_bare
        .iter()
        .find(|entry| basename(&entry.path) == "main")
        .or_else(|| inside_bare.first())
        .map(|entry| entry.path.clone())
        .or_else(|| candidates.first().map(|entry| entry.path.clone()))
        .or(Some(normalized))
}

/// `isDescendantPath`: strictly inside, POSIX comparison.
fn is_descendant_path(child: &str, parent: &str) -> bool {
    let child = child.trim_end_matches('/');
    let parent = parent.trim_end_matches('/');
    child.len() > parent.len() + 1
        && child.starts_with(parent)
        && child.as_bytes()[parent.len()] == b'/'
}

struct WorktreeEntry {
    path: String,
    bare: bool,
}

/// `parseWorktreeList`: lines are trimmed and blank lines skipped.
fn parse_worktree_list(stdout: &str) -> Vec<WorktreeEntry> {
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in stdout.split('\n') {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix("worktree ") {
            entries.push(WorktreeEntry {
                path: path.trim().to_owned(),
                bare: false,
            });
        } else if trimmed == "bare"
            && let Some(last) = entries.last_mut()
        {
            last.bare = true;
        }
    }
    entries
}

/// `isPaseoOwnedWorktreeCwd` path shape: `<worktrees-root>/<hash>/<slug>[/...]`.
fn is_owned_worktree_path(cwd: &str, context: &CheckoutContext) -> bool {
    if !(cwd.contains("/worktrees/") || cwd.contains("\\worktrees\\")) {
        return false;
    }
    let resolved = realpath(cwd).unwrap_or_else(|| resolve("/", cwd));
    realpath_aware_relative_path(&context.worktrees_base_root(), &resolved)
        .is_some_and(|relative| relative.split('/').filter(|part| !part.is_empty()).count() >= 2)
}

/// `storedBaseRefFromMetadata(readPaseoWorktreeMetadata(worktreeRoot))`.
fn stored_base_ref(worktree_root: &str) -> Result<Option<String>, GitError> {
    // ponytail: validates only the fields read here (version, baseRefName,
    // baseRef); the nested change-request, auto-name, and runtime schemas are
    // not checked, so a file invalid only there loads instead of throwing.
    let git_entry = format!("{worktree_root}/.git");
    let git_dir = match std::fs::read_to_string(&git_entry) {
        Ok(contents) => contents
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map_or(git_entry.clone(), |dir| resolve(worktree_root, dir.trim())),
        Err(_) => git_entry,
    };
    let metadata_path = format!("{git_dir}/paseo/worktree.json");
    let Ok(text) = std::fs::read_to_string(&metadata_path) else {
        return Ok(None);
    };
    let invalid = || GitError {
        message: format!("Invalid Paseo worktree metadata: {metadata_path}"),
    };
    let value = spocky_store::js_value::parse(&text).map_err(|_| invalid())?;
    let version = value
        .get("version")
        .and_then(spocky_store::js_value::JsValue::as_f64);
    if !matches!(version, Some(v) if (v - 1.0).abs() < f64::EPSILON || (v - 2.0).abs() < f64::EPSILON)
    {
        return Err(invalid());
    }
    let text_field = |key: &str| {
        value
            .get(key)
            .and_then(|field| field.as_str())
            .map(str::to_owned)
    };
    let base_ref_name = text_field("baseRefName").filter(|name| !name.is_empty());
    if base_ref_name.is_none() {
        return Err(invalid());
    }
    Ok(text_field("baseRef")
        .filter(|name| !name.is_empty())
        .or(base_ref_name))
}

/// `getCheckout(cwd)`.
///
/// # Errors
///
/// Rejects where the baseline does: the `git branch` fallback, `git status
/// --porcelain`, the ahead/behind `rev-list`, or invalid worktree metadata.
pub async fn get_checkout(cwd: &str, context: &CheckoutContext) -> Result<CheckoutLite, GitError> {
    let cwd_text = crate::paths::resolve_from_cwd(cwd);
    let cwd_path = Path::new(&cwd_text);
    let Some(worktree_root) = git_stdout(&["rev-parse", "--show-toplevel"], cwd_path)
        .await
        .ok()
        .and_then(|stdout| parse_rev_parse_path(&stdout))
    else {
        return Ok(CheckoutLite::not_git(cwd_text));
    };
    let (branch, remote_url, _absolute_git_dir, common_dir) = tokio::join!(
        current_branch(cwd_path, &cwd_text),
        async {
            git_stdout(&["config", "--get", "remote.origin.url"], cwd_path)
                .await
                .ok()
                .and_then(|stdout| non_empty(&stdout))
        },
        async {
            git_stdout(&["rev-parse", "--absolute-git-dir"], cwd_path)
                .await
                .ok()
                .and_then(|stdout| non_empty(&stdout))
        },
        async {
            git_stdout(&["rev-parse", "--git-common-dir"], cwd_path)
                .await
                .ok()
                .and_then(|stdout| parse_rev_parse_path(&stdout))
                .map(|path| resolve(&cwd_text, &path))
        }
    );
    let owned = is_owned_worktree_path(&cwd_text, context);
    let stored = if owned {
        stored_base_ref(&worktree_root)?
    } else {
        None
    };
    let base_ref = match stored {
        Some(stored) => Some(stored),
        None => repository_default_branch(cwd_path).await?,
    };
    let main_root = main_repo_root(cwd_path, common_dir.as_deref(), context).await;
    let comparison = match (&base_ref, &branch) {
        (Some(base), Some(current)) if branch_name_from_ref(base) != *current => {
            comparison_base_ref(cwd_path, base).await
        }
        _ => None,
    };
    git_stdout(&["status", "--porcelain"], cwd_path).await?;
    if let (Some(base), Some(current)) = (&base_ref, &branch) {
        let normalized = branch_name_from_ref(base);
        if !normalized.is_empty()
            && normalized != *current
            && let Some(comparison) = &comparison
        {
            git_stdout(
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{comparison}...{current}"),
                ],
                cwd_path,
            )
            .await?;
        }
    }
    let (is_owned, main_repo_root) = if owned && base_ref.is_some() {
        (true, main_root.or_else(|| Some(worktree_root.clone())))
    } else {
        let distinct = main_root.filter(|root| resolve("/", root) != resolve("/", &worktree_root));
        (false, distinct)
    };
    Ok(CheckoutLite {
        cwd: cwd_text,
        is_git: true,
        current_branch: branch,
        remote_url,
        worktree_root: Some(worktree_root),
        is_paseo_owned_worktree: is_owned && main_repo_root.is_some(),
        main_repo_root,
    })
}

#[cfg(test)]
mod tests {
    use super::{branch_name_from_ref, parse_rev_parse_path};

    #[test]
    fn rev_parse_path_rules() {
        assert_eq!(parse_rev_parse_path("/tmp/x\n").as_deref(), Some("/tmp/x"));
        assert_eq!(parse_rev_parse_path("  \n"), None);
        assert_eq!(parse_rev_parse_path("a\nb"), None);
        assert_eq!(parse_rev_parse_path("--show-toplevel"), None);
    }

    #[test]
    fn branch_names_from_refs() {
        assert_eq!(branch_name_from_ref("refs/heads/main"), "main");
        assert_eq!(branch_name_from_ref("refs/remotes/upstream/dev/x"), "dev/x");
        assert_eq!(branch_name_from_ref("origin/main"), "main");
        assert_eq!(branch_name_from_ref(" feature/x "), "feature/x");
    }
}
