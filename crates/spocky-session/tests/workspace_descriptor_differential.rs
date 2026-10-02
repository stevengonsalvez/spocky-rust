//! Differential check of `describe_workspace` against the pinned build's
//! `Session.prototype.describeWorkspaceRecord`, called with the session's
//! collaborators replaced by the same inputs Rust takes as arguments: the
//! project registry, the git snapshot peek and the scripts snapshot.
//!
//! Every input is fixed, so nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::Path;
use std::process::Command;

use spocky_contracts::frame::frame_text;
use spocky_contracts::json::JsonValue;
use spocky_contracts::number::JsNumber;
use spocky_contracts::workspace::DiffStat;
use spocky_session::workspace_descriptor::{GitSnapshotPeek, describe_workspace};
use spocky_store::js_value::{JsValue, parse};
use spocky_store::registry::{PersistedProjectRecord, PersistedWorkspaceRecord, RegistryRecord};

/// `{workspace, project, passProject, snapshot, scripts}` per case. With
/// `passProject` false node resolves the project through the registry
/// (`null` when missing), as the Rust caller does before the call.
const CASES: &str = r#"[
  {
    "workspace": {"workspaceId":"ws-1","projectId":"proj-git","cwd":"/repo/.paseo/worktrees/brave-otter","kind":"worktree","displayName":"brave-otter","title":"Fix login","branch":"brave-otter","worktreeRoot":"/repo/.paseo/worktrees/brave-otter","baseBranch":"main","isPaseoOwnedWorktree":true,"mainRepoRoot":"/repo","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z","pinnedAt":"2026-07-12T11:00:00.000Z","labels":["ui","bug"]},
    "project": {"projectId":"proj-git","rootPath":"/repo","kind":"git","displayName":"repo","customName":"My Repo","customIconRevision":"rev-3","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "passProject": true,
    "snapshot": {"diffStat":{"additions":12,"deletions":3},"currentBranch":"other","repoRoot":"/elsewhere"},
    "scripts": [{"scriptName":"web","type":"service","lifecycle":"stopped"}]
  },
  {
    "workspace": {"workspaceId":"ws-2","projectId":"proj-missing","cwd":"/tmp/notes","kind":"directory","displayName":"notes","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "project": null,
    "passProject": false,
    "snapshot": null,
    "scripts": []
  },
  {
    "workspace": {"workspaceId":"ws-3","projectId":"proj-plain","cwd":"/src/app/web","kind":"local_checkout","displayName":"web","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z","labels":[]},
    "project": {"projectId":"proj-plain","rootPath":"/src/app","kind":"non_git","displayName":"app","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "passProject": false,
    "snapshot": {"diffStat":null,"currentBranch":"feature/x","repoRoot":"/src/app"},
    "scripts": []
  },
  {
    "workspace": {"workspaceId":"ws-4","projectId":"proj-git","cwd":"/wt/slug-a","kind":"worktree","displayName":"slug-a","worktreeRoot":"/wt/slug-a/","isPaseoOwnedWorktree":true,"archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "project": {"projectId":"proj-git","rootPath":"/repo","kind":"git","displayName":"repo","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "passProject": true,
    "snapshot": null,
    "scripts": []
  },
  {
    "workspace": {"workspaceId":"ws-5","projectId":"proj-git","cwd":"/repo/sub","kind":"local_checkout","displayName":"repo","mainRepoRoot":"/main","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "project": {"projectId":"proj-git","rootPath":"/repo","kind":"git","displayName":"repo","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "passProject": true,
    "snapshot": {"diffStat":{"additions":0,"deletions":0},"currentBranch":null},
    "scripts": []
  },
  {
    "workspace": {"workspaceId":"ws-6","projectId":"proj-git","cwd":"/x/y/pkg","kind":"worktree","displayName":"y","branch":"topic","worktreeRoot":"/x/y","isPaseoOwnedWorktree":false,"labels":["a"],"archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "project": {"projectId":"proj-git","rootPath":"/repo","kind":"git","displayName":"repo","archivedAt":null,"createdAt":"2026-07-12T10:00:00.000Z","updatedAt":"2026-07-12T10:00:00.000Z"},
    "passProject": false,
    "snapshot": null,
    "scripts": []
  }
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, casesJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { Session } = await import(`${dist}/server/session.js`);
const { createPersistedProjectRecord, createPersistedWorkspaceRecord } = await import(
  `${dist}/server/workspace-registry.js`
);
const results = [];
for (const testCase of JSON.parse(casesJson)) {
  const workspace = createPersistedWorkspaceRecord(testCase.workspace);
  const project = testCase.project ? createPersistedProjectRecord(testCase.project) : null;
  const session = {
    projectRegistry: { get: async (id) => (project && project.projectId === id ? project : null) },
    workspaceGitService: {
      peekSnapshot: (cwd) => {
        if (cwd !== workspace.cwd) throw new Error(`unexpected peek ${cwd}`);
        return testCase.snapshot ? { git: testCase.snapshot } : null;
      },
    },
    buildWorkspaceScriptPayloadSnapshot: () => testCase.scripts,
    buildProjectPlacementForWorkspace: Session.prototype.buildProjectPlacementForWorkspace,
  };
  results.push(
    await Session.prototype.describeWorkspaceRecord.call(
      session,
      workspace,
      testCase.passProject ? project : undefined,
    ),
  );
}
process.stdout.write(JSON.stringify(results));
"#;

fn snapshot(value: &JsValue) -> Option<GitSnapshotPeek> {
    if !matches!(value, JsValue::Object(_)) {
        return None;
    }
    let text = |name: &str| value.get(name).and_then(JsValue::as_str).map(str::to_owned);
    let number = |stat: &JsValue, name: &str| {
        JsNumber::new(stat.get(name).and_then(JsValue::as_f64).expect(name)).expect(name)
    };
    Some(GitSnapshotPeek {
        diff_stat: value
            .get("diffStat")
            .filter(|stat| matches!(stat, JsValue::Object(_)))
            .map(|stat| DiffStat {
                additions: number(stat, "additions"),
                deletions: number(stat, "deletions"),
            }),
        current_branch: text("currentBranch"),
        repo_root: text("repoRoot"),
    })
}

fn rust_output() -> String {
    let cases = parse(CASES).expect("cases");
    let descriptors: Vec<_> = cases
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            let workspace =
                PersistedWorkspaceRecord::from_value(case.get("workspace").expect("ws"))
                    .expect("workspace record");
            let project = case
                .get("project")
                .filter(|project| matches!(project, JsValue::Object(_)))
                .map(|project| PersistedProjectRecord::from_value(project).expect("project"));
            let scripts = case
                .get("scripts")
                .and_then(JsValue::as_array)
                .expect("scripts")
                .iter()
                .cloned()
                .map(JsonValue::from)
                .collect();
            describe_workspace(
                &workspace,
                project.as_ref(),
                snapshot(case.get("snapshot").expect("snapshot")).as_ref(),
                scripts,
            )
        })
        .collect();
    frame_text(&descriptors).expect("serialize")
}

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/session.js",
        "419387d87dba556905ace2e49c0b3badf18d390fad3de95a8f61858593825d1f",
    ),
    (
        "server/workspace-registry.js",
        "30578109d7388b6d0cd0b76a1f1e5e19d1711a6ce13eedcdcb9fbc9eebcb9164",
    ),
    (
        "server/workspace-registry-model.js",
        "99035b682844ef38281733462dc777a5219df9229264d05dc716c962f22ec6c1",
    ),
];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(Path::new(dist).join(path)).expect("pinned module");
        let actual = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(&actual, expected, "{path} is not the pinned build");
    }
}

#[test]
fn workspace_descriptor_matches_pinned_build() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: workspace descriptor differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(CASES)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
