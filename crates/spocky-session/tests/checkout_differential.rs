//! Differential check of the checkout probe and project key against the
//! pinned Paseo build: for each disposable checkout, node runs the built
//! `getCheckoutStatus` + `checkoutLiteFromGitSnapshot` (as
//! `workspaceGitService.getCheckout` does) and `deriveProjectKey`, and the
//! Rust port must print the same JSON.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0) and `SPOCKY_PASEO_DIST` (the
//! pinned build's `packages/server/dist/server`). The lane acceptance command
//! sets both. Without them the tests FAIL; set `SPOCKY_ALLOW_SKIP=1` to skip
//! them explicitly outside the gate.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_session::checkout::{CheckoutContext, CheckoutLite, get_checkout};
use spocky_session::project_key::{ProjectKeyInput, derive_project_key};
use spocky_store::js_value::{JsObject, JsValue, stringify};

struct Disposable(PathBuf);

impl Drop for Disposable {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "user.name=Spocky",
            "-c",
        ])
        .arg("user.email=spocky@example.invalid")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_DATE", "2026-10-01T10:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-01T10:00:00Z")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} in {}", cwd.display());
}

fn commit(cwd: &Path, name: &str) {
    fs::write(cwd.join(name), name).expect("write file");
    git(cwd, &["add", name]);
    git(cwd, &["commit", "-q", "-m", name]);
}

/// Builds the checkouts and returns `(label, cwd)` pairs.
fn fixtures(root: &Path) -> Vec<(&'static str, PathBuf)> {
    let plain = root.join("plain");
    fs::create_dir_all(&plain).expect("plain dir");

    let repo = root.join("repo");
    fs::create_dir_all(repo.join("sub/dir")).expect("repo dir");
    git(&repo, &["init", "-q"]);
    commit(&repo, "a.txt");

    let remote = root.join("remote");
    fs::create_dir_all(&remote).expect("remote dir");
    git(&remote, &["init", "-q"]);
    commit(&remote, "a.txt");
    git(
        &remote,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/Owner/Repo.git",
        ],
    );

    let detached = root.join("detached");
    fs::create_dir_all(&detached).expect("detached dir");
    git(&detached, &["init", "-q"]);
    commit(&detached, "a.txt");
    commit(&detached, "b.txt");
    git(&detached, &["checkout", "-q", "--detach", "HEAD~1"]);

    let unborn = root.join("unborn");
    fs::create_dir_all(&unborn).expect("unborn dir");
    git(&unborn, &["init", "-q"]);

    let feature = root.join("feature");
    fs::create_dir_all(&feature).expect("feature dir");
    git(&feature, &["init", "-q"]);
    commit(&feature, "a.txt");
    git(&feature, &["checkout", "-q", "-b", "dev"]);
    commit(&feature, "b.txt");

    let credentials = root.join("credentials");
    fs::create_dir_all(&credentials).expect("credentials dir");
    git(&credentials, &["init", "-q"]);
    commit(&credentials, "a.txt");
    git(
        &credentials,
        &[
            "remote",
            "add",
            "origin",
            "https://user:s3cret@git.example.com:8443/Team/App.git",
        ],
    );

    let scp = root.join("scp");
    fs::create_dir_all(&scp).expect("scp dir");
    git(&scp, &["init", "-q"]);
    commit(&scp, "a.txt");
    git(
        &scp,
        &["remote", "add", "origin", "git@GitHub.com:Owner/Repo.git"],
    );

    let ssh = root.join("ssh");
    fs::create_dir_all(&ssh).expect("ssh dir");
    git(&ssh, &["init", "-q"]);
    commit(&ssh, "a.txt");
    git(
        &ssh,
        &[
            "remote",
            "add",
            "origin",
            "ssh://git@Host.Example:2222/team/app.git",
        ],
    );

    let linked = root.join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().expect("utf8 path"),
        ],
    );

    vec![
        ("plain", plain),
        ("repo", repo.clone()),
        ("subdirectory", repo.join("sub/dir")),
        ("remote", remote),
        ("detached", detached),
        ("unborn", unborn),
        ("feature-branch", feature),
        ("linked-worktree", linked),
        ("credentials-remote", credentials),
        ("scp-remote", scp),
        ("ssh-remote", ssh),
        ("missing", root.join("missing")),
    ]
}

fn text(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Null, |text| JsValue::String(text.clone()))
}

fn rust_row(label: &str, checkout: &CheckoutLite, key: String) -> JsValue {
    let mut lite = JsObject::new();
    lite.insert("cwd", JsValue::String(checkout.cwd.clone()));
    lite.insert("isGit", JsValue::Bool(checkout.is_git));
    lite.insert("currentBranch", text(checkout.current_branch.as_ref()));
    lite.insert("remoteUrl", text(checkout.remote_url.as_ref()));
    lite.insert("worktreeRoot", text(checkout.worktree_root.as_ref()));
    lite.insert(
        "isPaseoOwnedWorktree",
        JsValue::Bool(checkout.is_paseo_owned_worktree),
    );
    lite.insert("mainRepoRoot", text(checkout.main_repo_root.as_ref()));
    let mut row = JsObject::new();
    row.insert("label", JsValue::String(label.to_owned()));
    row.insert("checkout", JsValue::Object(lite));
    row.insert("projectKey", JsValue::String(key));
    JsValue::Object(row)
}

/// The pinned node and dist paths, or `None` when skipping was requested.
fn pinned_inputs() -> Option<(std::ffi::OsString, std::ffi::OsString)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => Some((node, dist)),
        _ if std::env::var_os("SPOCKY_ALLOW_SKIP").is_some() => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: pinned differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

/// `gtimeout` on macOS with coreutils, else `timeout`.
fn timeout_program() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

const NODE_SCRIPT: &str = r#"
const [dist, paseoHome, ...pairs] = process.argv.slice(1);
const { getCheckoutStatus } = await import(`${dist}/utils/checkout-git.js`);
const { checkoutLiteFromGitSnapshot } = await import(`${dist}/server/workspace-registry-model.js`);
const { deriveProjectKey } = await import(`${dist}/server/project-key.js`);
const { resolve } = await import("node:path");
const rows = [];
for (let i = 0; i < pairs.length; i += 2) {
  const label = pairs[i];
  const cwd = resolve(pairs[i + 1]);
  const status = await getCheckoutStatus(cwd, { paseoHome });
  const checkout = status.isGit
    ? checkoutLiteFromGitSnapshot(cwd, { isGit: true, currentBranch: status.currentBranch, remoteUrl: status.remoteUrl, repoRoot: status.repoRoot, isPaseoOwnedWorktree: status.isPaseoOwnedWorktree, mainRepoRoot: status.mainRepoRoot })
    : checkoutLiteFromGitSnapshot(cwd, { isGit: false, currentBranch: null, remoteUrl: null, repoRoot: null, isPaseoOwnedWorktree: false, mainRepoRoot: null });
  const projectKey = deriveProjectKey({ rootPath: cwd, remoteUrl: checkout.remoteUrl, worktreeRoot: checkout.worktreeRoot, mainRepoRoot: checkout.mainRepoRoot, serverId: "srv" });
  rows.push({ label, checkout, projectKey });
}
process.stdout.write(JSON.stringify(rows));
"#;

#[tokio::test]
async fn checkout_probe_and_project_key_match_pinned_build() {
    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "spocky-session-checkout-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create disposable root");
    let guard = Disposable(root.clone());
    let paseo_home = root.join("paseo-home");
    let cases = fixtures(&root);

    let context = CheckoutContext {
        paseo_home: paseo_home.to_string_lossy().into_owned(),
        worktrees_root: None,
        home: std::env::var("HOME").unwrap_or_default(),
    };
    let mut rust_rows = Vec::new();
    for (label, cwd) in &cases {
        let cwd = cwd.to_string_lossy().into_owned();
        let checkout = get_checkout(&cwd, &context)
            .await
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        let key = derive_project_key(&ProjectKeyInput {
            root_path: &checkout.cwd,
            remote_url: checkout.remote_url.as_deref(),
            worktree_root: checkout.worktree_root.as_deref(),
            main_repo_root: checkout.main_repo_root.as_deref(),
            server_id: Some("srv"),
        });
        rust_rows.push(rust_row(label, &checkout, key));
    }

    let mut command = Command::new(timeout_program());
    command
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(&paseo_home);
    for (label, cwd) in &cases {
        command.arg(label).arg(cwd);
    }
    let output = command.output().expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8_lossy(&output.stdout).into_owned();
    let actual = stringify(&JsValue::Array(rust_rows));
    drop(guard);
    assert_eq!(actual, expected);
}

/// Replaces generated ids and wall-clock values, in order of first
/// appearance, with stable placeholders: `wks_`/`prj_` + 16 hex become
/// `<wks-N>`/`<prj-N>`, and ISO-8601 millisecond UTC timestamps become
/// `<time>`. Nothing else changes; key order and every other byte stay.
fn normalize_generated(text: &str) -> String {
    let mut ids: Vec<String> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let rest = &text[index..];
        let id_prefix = ["wks_", "prj_"]
            .into_iter()
            .find(|prefix| rest.starts_with(prefix));
        if let Some(prefix) = id_prefix
            && rest.len() >= 20
            && rest.as_bytes()[4..20]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            let id = &rest[..20];
            let position = ids.iter().position(|seen| seen == id).unwrap_or_else(|| {
                ids.push(id.to_owned());
                ids.len() - 1
            });
            let _ = write!(out, "<{}-{position}>", &prefix[..3]);
            index += 20;
            continue;
        }
        if is_iso_timestamp(rest) {
            out.push_str("<time>");
            index += 24;
            continue;
        }
        let character = rest.chars().next().expect("non-empty rest");
        out.push(character);
        index += character.len_utf8();
    }
    out
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, as `toISOString` writes for years 0000..9999.
fn is_iso_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 24 {
        return false;
    }
    let pattern = b"dddd-dd-ddTdd:dd:dd.dddZ";
    pattern
        .iter()
        .zip(bytes)
        .all(|(expected, actual)| match expected {
            b'd' => actual.is_ascii_digit(),
            literal => literal == actual,
        })
}

#[test]
fn normalization_only_touches_generated_ids_and_timestamps() {
    let input = r#"{"workspaceId":"wks_0123456789abcdef","projectId":"prj_aaaaaaaaaaaaaaaa","other":"wks_0123456789abcdef","x":"prj_AAAAAAAAAAAAAAAA","t":"2026-10-01T10:00:00.000Z","u":"2026-10-01 10:00"}"#;
    assert_eq!(
        normalize_generated(input),
        r#"{"workspaceId":"<wks-0>","projectId":"<prj-1>","other":"<wks-0>","x":"prj_AAAAAAAAAAAAAAAA","t":"<time>","u":"2026-10-01 10:00"}"#
    );
}

const PROVISION_SCRIPT: &str = r#"
const [dist, paseoHome, ...cases] = process.argv.slice(1);
const { getCheckoutStatus } = await import(`${dist}/utils/checkout-git.js`);
const { checkoutLiteFromGitSnapshot } = await import(`${dist}/server/workspace-registry-model.js`);
const { FileBackedProjectRegistry, FileBackedWorkspaceRegistry } = await import(`${dist}/server/workspace-registry.js`);
const { createWorkspaceProvisioningService } = await import(`${dist}/server/session/workspace-provisioning/workspace-provisioning-service.js`);
const { resolve, join } = await import("node:path");
const { stat } = await import("node:fs/promises");
const logger = { child() { return logger; }, error() {}, warn() {}, info() {}, debug() {}, trace() {} };
const getCheckout = async (cwd) => {
  const normalizedCwd = resolve(cwd);
  const status = await getCheckoutStatus(normalizedCwd, { paseoHome });
  return status.isGit
    ? checkoutLiteFromGitSnapshot(normalizedCwd, { isGit: true, currentBranch: status.currentBranch, remoteUrl: status.remoteUrl, repoRoot: status.repoRoot, isPaseoOwnedWorktree: status.isPaseoOwnedWorktree, mainRepoRoot: status.mainRepoRoot })
    : checkoutLiteFromGitSnapshot(normalizedCwd, { isGit: false, currentBranch: null, remoteUrl: null, repoRoot: null, isPaseoOwnedWorktree: false, mainRepoRoot: null });
};
const projectRegistry = new FileBackedProjectRegistry(join(paseoHome, "projects", "projects.json"), logger);
const workspaceRegistry = new FileBackedWorkspaceRegistry(join(paseoHome, "projects", "workspaces.json"), logger);
const service = createWorkspaceProvisioningService({
  serverId: "srv",
  workspaceRegistry,
  projectRegistry,
  workspaceGitService: { getCheckout, getSnapshot: async () => null, peekSnapshot: () => null },
  isDirectory: async (path) => { try { return (await stat(path)).isDirectory(); } catch { return false; } },
  logger,
});
for (let i = 0; i < cases.length; i += 2) {
  await service.createWorkspaceForDirectory(cases[i + 1], cases[i] === "-" ? undefined : cases[i]);
}
"#;

/// Runs the pinned build's `createWorkspaceForDirectory` over `sequence`.
fn run_original_provisioning(
    node: &std::ffi::OsStr,
    dist: &std::ffi::OsStr,
    home: &Path,
    sequence: &[(&str, String)],
) {
    let mut command = Command::new(timeout_program());
    command
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args(["--input-type=module", "-e", PROVISION_SCRIPT])
        .arg(dist)
        .arg(home);
    for (title, cwd) in sequence {
        command.arg(title).arg(cwd);
    }
    let output = command.output().expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn directory_provisioning_matches_pinned_build() {
    use spocky_session::provisioning::{WorkspaceCreateContext, WorkspaceProvisioning};
    use spocky_store::registry::{ProjectRegistry, WorkspaceRegistry};
    use tokio::sync::Mutex;

    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "spocky-session-provision-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create disposable root");
    let guard = Disposable(root.clone());
    let fixtures = fixtures(&root);
    let path_of = |label: &str| {
        fixtures
            .iter()
            .find(|(name, _)| *name == label)
            .map(|(_, path)| path.to_string_lossy().into_owned())
            .expect("fixture")
    };
    // (title or "-", cwd): a repeat directory gets a new workspace and reuses its project.
    let sequence = vec![
        ("-", path_of("plain")),
        ("  Titled  ", path_of("repo")),
        ("-", path_of("subdirectory")),
        ("-", path_of("repo")),
        ("-", path_of("linked-worktree")),
        ("-", path_of("detached")),
        ("-", path_of("unborn")),
        ("-", path_of("feature-branch")),
        ("-", path_of("credentials-remote")),
        ("-", path_of("scp-remote")),
        ("-", path_of("ssh-remote")),
    ];

    let original_home = root.join("original-home");
    run_original_provisioning(&node, &dist, &original_home, &sequence);

    let spocky_home = root.join("spocky-home");
    let created = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provisioning = WorkspaceProvisioning {
        projects: Mutex::new(ProjectRegistry::new(
            spocky_home.join("projects").join("projects.json"),
        )),
        workspaces: Mutex::new(WorkspaceRegistry::new(
            spocky_home.join("projects").join("workspaces.json"),
        )),
        server_id: Some("srv".to_owned()),
        on_workspace_created: Some(Box::new({
            let created = std::sync::Arc::clone(&created);
            move |_| {
                created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        })),
        checkout: CheckoutContext {
            paseo_home: spocky_home.to_string_lossy().into_owned(),
            worktrees_root: None,
            home: std::env::var("HOME").unwrap_or_default(),
        },
    };
    for (title, cwd) in &sequence {
        let title = (*title != "-").then_some(*title);
        provisioning
            .create_workspace_for_directory(cwd, title, None, WorkspaceCreateContext::default())
            .await
            .unwrap_or_else(|error| panic!("{cwd}: {error}"));
    }

    assert_eq!(
        created.load(std::sync::atomic::Ordering::Relaxed),
        sequence.len(),
        "workspace.created fires once per created workspace"
    );
    for file in ["projects.json", "workspaces.json"] {
        let read = |home: &Path| {
            fs::read_to_string(home.join("projects").join(file))
                .unwrap_or_else(|error| panic!("read {file}: {error}"))
        };
        let (expected, actual) = (read(&original_home), read(&spocky_home));
        assert_eq!(
            normalize_generated(&actual),
            normalize_generated(&expected),
            "{file}"
        );
    }
    drop(guard);
}
