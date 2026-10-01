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

use spocky_store::js_value::{JsObject, JsValue, parse, stringify_pretty};

use crate::git::{GitError, GitOptions, run_git};
use crate::paths::{
    basename, dirname, expand_tilde, realpath_aware_relative_path, realpath_js, resolve,
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
            Some(root) if !root.is_empty() => {
                let expanded = expand_tilde(root, &self.home);
                if expanded.starts_with('/') {
                    resolve("/", &expanded)
                } else {
                    resolve(&resolve("/", &self.paseo_home), &expanded)
                }
            }
            _ => format!("{}/worktrees", resolve("/", &self.paseo_home)),
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
        let head_name_path = resolve(cwd_text, stdout.trim());
        let Ok(contents) = blocking(move || std::fs::read_to_string(head_name_path)).await else {
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
    let common_dir = common_dir?.to_owned();
    let normalized = blocking(move || realpath_js(&common_dir)).await?;
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
    let resolved = realpath_js(cwd).unwrap_or_else(|| resolve("/", cwd));
    realpath_aware_relative_path(&context.worktrees_base_root(), &resolved)
        .is_some_and(|relative| relative.split('/').filter(|part| !part.is_empty()).count() >= 2)
}

/// `getGitDirForWorktreeRoot`: `.git` must exist; a `.git` file names the
/// git dir with `/gitdir:\s*(.+)/`.
fn git_dir_for_worktree_root(worktree_root: &str) -> Result<String, GitError> {
    let git_path = format!("{}/.git", worktree_root.trim_end_matches('/'));
    if std::fs::metadata(&git_path).is_err() {
        return Err(GitError {
            message: format!("Not a git repository: {worktree_root}"),
        });
    }
    if let Ok(contents) = std::fs::read_to_string(&git_path)
        && let Some(start) = contents.find("gitdir:")
    {
        let after = contents[start + "gitdir:".len()..].trim_start();
        let line_end = after
            .find(['\n', '\r', '\u{2028}', '\u{2029}'])
            .unwrap_or(after.len());
        let raw = after[..line_end].trim();
        if !raw.is_empty() {
            return Ok(if raw.starts_with('/') {
                raw.to_owned()
            } else {
                resolve(worktree_root, raw)
            });
        }
    }
    Ok(git_path)
}

/// One zod 4.4.3 issue, as `ZodError.message` serializes it, and whether it
/// aborts its schema (type and literal failures abort; checks continue).
struct Issue {
    value: JsValue,
    aborts: bool,
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn path_value(path: &[&str]) -> JsValue {
    JsValue::Array(path.iter().map(|segment| text(segment)).collect())
}

/// zod's `parsedType` names for JSON values; `None` is a missing key.
fn received(value: Option<&JsValue>) -> &'static str {
    match value {
        None => "undefined",
        Some(JsValue::Null) => "null",
        Some(JsValue::Bool(_)) => "boolean",
        Some(JsValue::Number(_)) => "number",
        Some(JsValue::String(_)) => "string",
        Some(JsValue::Array(_)) => "array",
        Some(JsValue::Object(_)) => "object",
    }
}

fn issue(entries: Vec<(&str, JsValue)>, aborts: bool) -> Issue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    Issue {
        value: JsValue::Object(object),
        aborts,
    }
}

fn invalid_type(expected: &str, value: Option<&JsValue>, path: &[&str]) -> Issue {
    issue(
        vec![
            ("expected", text(expected)),
            ("code", text("invalid_type")),
            ("path", path_value(path)),
            (
                "message",
                text(&format!(
                    "Invalid input: expected {expected}, received {}",
                    received(value)
                )),
            ),
        ],
        true,
    )
}

/// `z.string().min(1)`, required or `.optional()`.
fn check_text(object: &JsObject, key: &str, optional: bool, path: &[&str], out: &mut Vec<Issue>) {
    let field_path: Vec<&str> = path.iter().copied().chain([key]).collect();
    match object.get(key) {
        None if optional => {}
        Some(JsValue::String(value)) => {
            if value.is_empty() {
                out.push(issue(
                    vec![
                        ("origin", text("string")),
                        ("code", text("too_small")),
                        ("minimum", JsValue::Number(1.0)),
                        ("inclusive", JsValue::Bool(true)),
                        ("path", path_value(&field_path)),
                        (
                            "message",
                            text("Too small: expected string to have >=1 characters"),
                        ),
                    ],
                    false,
                ));
            }
        }
        value => out.push(invalid_type("string", value, &field_path)),
    }
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `z.number().int().positive()`, required or `.optional()`.
fn check_positive_int(
    object: &JsObject,
    key: &str,
    optional: bool,
    path: &[&str],
    out: &mut Vec<Issue>,
) {
    let field_path: Vec<&str> = path.iter().copied().chain([key]).collect();
    let number = match object.get(key) {
        None if optional => return,
        Some(JsValue::Number(number)) => *number,
        value => {
            out.push(invalid_type("number", value, &field_path));
            return;
        }
    };
    if !number.is_finite() {
        out.push(issue(
            vec![
                ("expected", text("number")),
                ("code", text("invalid_type")),
                ("received", text("Infinity")),
                ("path", path_value(&field_path)),
                (
                    "message",
                    text("Invalid input: expected number, received number"),
                ),
            ],
            true,
        ));
        return;
    }
    if number.fract() != 0.0 {
        out.push(issue(
            vec![
                ("expected", text("int")),
                ("format", text("safeint")),
                ("code", text("invalid_type")),
                ("path", path_value(&field_path)),
                (
                    "message",
                    text("Invalid input: expected int, received number"),
                ),
            ],
            true,
        ));
        return;
    }
    let note = text("Integers must be within the safe integer range.");
    if number > MAX_SAFE_INTEGER {
        out.push(issue(
            vec![
                ("code", text("too_big")),
                ("maximum", JsValue::Number(MAX_SAFE_INTEGER)),
                ("note", note),
                ("origin", text("int")),
                ("inclusive", JsValue::Bool(true)),
                ("path", path_value(&field_path)),
                (
                    "message",
                    text("Too big: expected int to be <=9007199254740991"),
                ),
            ],
            false,
        ));
    } else if number < -MAX_SAFE_INTEGER {
        out.push(issue(
            vec![
                ("code", text("too_small")),
                ("minimum", JsValue::Number(-MAX_SAFE_INTEGER)),
                ("note", note),
                ("origin", text("int")),
                ("inclusive", JsValue::Bool(true)),
                ("path", path_value(&field_path)),
                (
                    "message",
                    text("Too small: expected int to be >=-9007199254740991"),
                ),
            ],
            false,
        ));
    }
    if number <= 0.0 {
        out.push(issue(
            vec![
                ("origin", text("number")),
                ("code", text("too_small")),
                ("minimum", JsValue::Number(0.0)),
                ("inclusive", JsValue::Bool(false)),
                ("path", path_value(&field_path)),
                ("message", text("Too small: expected number to be >0")),
            ],
            false,
        ));
    }
}

/// A nested `z.object(...)` field, required or `.optional()`; `None` when absent.
fn nested_object<'a>(
    object: &'a JsObject,
    key: &str,
    path: &[&str],
    out: &mut Vec<Issue>,
) -> Option<&'a JsObject> {
    match object.get(key) {
        None => None,
        Some(JsValue::Object(nested)) => Some(nested),
        value => {
            let field_path: Vec<&str> = path.iter().copied().chain([key]).collect();
            out.push(invalid_type("object", value, &field_path));
            None
        }
    }
}

/// `ChangeRequestLookupTargetSchema.optional()`.
fn check_lookup_target(object: &JsObject, out: &mut Vec<Issue>) {
    let path = ["changeRequestLookupTarget"];
    if let Some(target) = nested_object(object, "changeRequestLookupTarget", &[], out) {
        check_text(target, "headRef", false, &path, out);
        check_text(target, "headRepositoryOwner", true, &path, out);
        check_positive_int(target, "changeRequestNumber", true, &path, out);
        check_text(target, "localBranchName", true, &path, out);
    }
}

/// The `firstAgentBranchAutoName` discriminated union, `.optional()`.
fn check_auto_name(object: &JsObject, out: &mut Vec<Issue>) {
    let key = "firstAgentBranchAutoName";
    let auto = match object.get(key) {
        None => return,
        Some(JsValue::Object(auto)) => auto,
        value => {
            out.push(issue(
                vec![
                    ("code", text("invalid_type")),
                    ("expected", text("object")),
                    ("path", path_value(&[key])),
                    (
                        "message",
                        text(&format!(
                            "Invalid input: expected object, received {}",
                            received(value)
                        )),
                    ),
                ],
                true,
            ));
            return;
        }
    };
    match auto.get("status").and_then(JsValue::as_str) {
        Some("pending") => check_text(auto, "placeholderBranchName", false, &[key], out),
        Some("attempted") => {
            check_text(auto, "placeholderBranchName", false, &[key], out);
            check_text(auto, "attemptedAt", false, &[key], out);
        }
        _ => out.push(issue(
            vec![
                ("code", text("invalid_union")),
                ("errors", JsValue::Array(Vec::new())),
                ("note", text("No matching discriminator")),
                ("discriminator", text("status")),
                (
                    "options",
                    JsValue::Array(vec![text("pending"), text("attempted")]),
                ),
                ("path", path_value(&[key, "status"])),
                (
                    "message",
                    text("Invalid discriminator value. Expected 'pending' | 'attempted'"),
                ),
            ],
            true,
        )),
    }
}

/// One branch of the metadata union: `version` literal, then fields in shape order.
fn branch_issues(value: &JsValue, version: f64) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(object) = value.as_object() else {
        out.push(invalid_type("object", Some(value), &[]));
        return out;
    };
    let version_matches = object
        .get("version")
        .and_then(JsValue::as_f64)
        .is_some_and(|found| (found - version).abs() < f64::EPSILON);
    if !version_matches {
        out.push(issue(
            vec![
                ("code", text("invalid_value")),
                ("values", JsValue::Array(vec![JsValue::Number(version)])),
                ("path", path_value(&["version"])),
                (
                    "message",
                    text(&format!("Invalid input: expected {version}")),
                ),
            ],
            true,
        ));
    }
    check_text(object, "baseRefName", false, &[], &mut out);
    check_text(object, "baseRef", true, &[], &mut out);
    check_lookup_target(object, &mut out);
    if version > 1.5 {
        check_auto_name(object, &mut out);
        if let Some(runtime) = nested_object(object, "runtime", &[], &mut out) {
            check_positive_int(runtime, "worktreePort", false, &["runtime"], &mut out);
        }
    }
    out
}

/// `PaseoWorktreeMetadataSchema.parse` (`z.union([V1, V2])`): the issues
/// `ZodError` carries, or an empty list when the value is valid.
fn worktree_metadata_issues(value: &JsValue) -> Vec<JsValue> {
    let branches = [branch_issues(value, 1.0), branch_issues(value, 2.0)];
    if branches.iter().any(Vec::is_empty) {
        return Vec::new();
    }
    let live: Vec<&Vec<Issue>> = branches
        .iter()
        .filter(|issues| !issues.iter().any(|issue| issue.aborts))
        .collect();
    if let [only] = live.as_slice() {
        return only.iter().map(|issue| issue.value.clone()).collect();
    }
    let errors = branches
        .iter()
        .map(|issues| JsValue::Array(issues.iter().map(|issue| issue.value.clone()).collect()))
        .collect();
    vec![
        issue(
            vec![
                ("code", text("invalid_union")),
                ("errors", JsValue::Array(errors)),
                ("path", JsValue::Array(Vec::new())),
                ("message", text("Invalid input")),
            ],
            true,
        )
        .value,
    ]
}

/// `storedBaseRefFromMetadata(readPaseoWorktreeMetadata(worktreeRoot))`.
/// Failures carry the baseline text: V8's `JSON.parse` message, or the
/// `ZodError` message (`JSON.stringify(issues, null, 2)`).
fn stored_base_ref(worktree_root: &str) -> Result<Option<String>, GitError> {
    let metadata_path = format!(
        "{}/paseo/worktree.json",
        git_dir_for_worktree_root(worktree_root)?
    );
    if std::fs::metadata(&metadata_path).is_err() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&metadata_path).map_err(|error| GitError {
        message: error.to_string(),
    })?;
    let value = parse(&text).map_err(|error| GitError {
        message: error.message,
    })?;
    let issues = worktree_metadata_issues(&value);
    if !issues.is_empty() {
        return Err(GitError {
            message: stringify_pretty(&JsValue::Array(issues)),
        });
    }
    let text_field = |key: &str| value.get(key).and_then(JsValue::as_str).map(str::to_owned);
    Ok(text_field("baseRef").or_else(|| text_field("baseRefName")))
}

/// Runs blocking filesystem work off the async executor.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
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
    let owned = {
        let (cwd, context) = (cwd_text.clone(), context.clone());
        blocking(move || is_owned_worktree_path(&cwd, &context)).await
    };
    let stored = if owned {
        let root = worktree_root.clone();
        blocking(move || stored_base_ref(&root)).await?
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

    /// Expected acceptance printed by the pinned build's
    /// `readPaseoWorktreeMetadata` on node 22.20.0 for each text.
    #[test]
    fn worktree_metadata_schema_matches_zod() {
        use spocky_store::js_value::parse;
        for (text, valid) in [
            (r#"{"version":1,"baseRefName":"main"}"#, true),
            (r#"{"version":1,"baseRefName":""}"#, false),
            (r#"{"version":3,"baseRefName":"main"}"#, false),
            (r#"{"version":1,"baseRefName":"main","baseRef":""}"#, false),
            (
                r#"{"version":1,"baseRefName":"main","baseRef":null}"#,
                false,
            ),
            (
                r#"{"version":1,"baseRefName":"main","changeRequestLookupTarget":{"headRef":"x","changeRequestNumber":2}}"#,
                true,
            ),
            (
                r#"{"version":1,"baseRefName":"main","changeRequestLookupTarget":{"headRef":"x","changeRequestNumber":0}}"#,
                false,
            ),
            (
                r#"{"version":2,"baseRefName":"main","firstAgentBranchAutoName":{"status":"pending","placeholderBranchName":"p"}}"#,
                true,
            ),
            (
                r#"{"version":2,"baseRefName":"main","firstAgentBranchAutoName":{"status":"attempted","placeholderBranchName":"p"}}"#,
                false,
            ),
            (
                r#"{"version":2,"baseRefName":"main","runtime":{"worktreePort":3000}}"#,
                true,
            ),
            (
                r#"{"version":2,"baseRefName":"main","runtime":{"worktreePort":1.5}}"#,
                false,
            ),
            ("[]", false),
        ] {
            assert_eq!(
                super::worktree_metadata_issues(&parse(text).expect("JSON")).is_empty(),
                valid,
                "{text}"
            );
        }
    }

    #[test]
    fn missing_git_entry_is_not_a_repository() {
        let root = std::env::temp_dir().join(format!("spocky-no-git-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create dir");
        let root_text = root.to_string_lossy().into_owned();
        let error = super::stored_base_ref(&root_text).expect_err("no .git");
        assert_eq!(error.message, format!("Not a git repository: {root_text}"));
        std::fs::write(root.join(".git"), "gitdir:\n  ../elsewhere/.git\n").expect("git file");
        assert_eq!(
            super::git_dir_for_worktree_root(&root_text).expect("git dir"),
            super::resolve(&root_text, "../elsewhere/.git")
        );
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn branch_names_from_refs() {
        assert_eq!(branch_name_from_ref("refs/heads/main"), "main");
        assert_eq!(branch_name_from_ref("refs/remotes/upstream/dev/x"), "dev/x");
        assert_eq!(branch_name_from_ref("origin/main"), "main");
        assert_eq!(branch_name_from_ref(" feature/x "), "feature/x");
    }
}
