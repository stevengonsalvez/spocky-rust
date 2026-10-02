//! Old-state home check for G4, registries: `projects.json` and
//! `workspaces.json` files the original wrote load the same under the Rust
//! registries as under the pinned build.
//!
//! The pinned registries write some files through their own `upsert`; the
//! test adds the other shapes an old home holds: records without the
//! optional fields, unknown keys, one id twice, an invalid record next to a
//! valid one (the whole file is rejected), corrupt JSON, and a missing
//! file. Node and Rust load each file. `list()` (order and key order
//! included), `get` hits and misses, and whether the load failed must
//! match.
//!
//! Every input is fixed, so nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_store::js_value::{JsObject, JsValue, stringify};
use spocky_store::registry::{ProjectRegistry, RegistryRecord, WorkspaceRegistry};

/// `[file, kind, id to look up]`; `kind` is `project` or `workspace`.
const FILES: &[(&str, &str, &str)] = &[
    ("ok-projects.json", "project", "prj_1"),
    ("ok-workspaces.json", "workspace", "wks_1"),
    ("legacy-projects.json", "project", "prj_legacy"),
    ("extra-keys-workspaces.json", "workspace", "wks_extra"),
    ("dup-projects.json", "project", "prj_dup"),
    ("invalid-projects.json", "project", "prj_ok"),
    ("invalid-workspaces.json", "workspace", "wks_ok"),
    ("corrupt-projects.json", "project", "prj_1"),
    ("not-an-array-workspaces.json", "workspace", "wks_1"),
    ("missing-projects.json", "project", "prj_1"),
];

const NODE_SCRIPT: &str = r#"
const [dist, seed, filesJson] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const fs = await import("node:fs");
const path = await import("node:path");
const mod = await import(`${dist}/server/workspace-registry.js`);
let failed = false;
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() { failed = true; } };
fs.mkdirSync(seed, { recursive: true });
const at = "2026-09-20T09:00:00.000Z";
const project = (projectId, rootPath, extra = {}) => mod.createPersistedProjectRecord({
  projectId, rootPath, kind: "git", displayName: path.basename(rootPath), createdAt: at, updatedAt: at, ...extra });
const workspace = (workspaceId, projectId, cwd, extra = {}) => mod.createPersistedWorkspaceRecord({
  workspaceId, projectId, cwd, kind: "worktree", displayName: path.basename(cwd), createdAt: at, updatedAt: at, ...extra });
const projects = new mod.FileBackedProjectRegistry(path.join(seed, "ok-projects.json"), logger);
await projects.upsert(project("prj_1", "/work/alpha", { customName: "Alpha", customIconRevision: "r1", projectKey: "k" }));
await projects.upsert(project("prj_2", "/Work/Zeta", { kind: "non_git", archivedAt: "2026-09-21T00:00:00.000Z" }));
await projects.upsert(project("prj_3", "/work/é", {}));
const workspaces = new mod.FileBackedWorkspaceRegistry(path.join(seed, "ok-workspaces.json"), logger);
await workspaces.upsert(workspace("wks_1", "prj_1", "/work/alpha/wt", { title: "T", branch: "b", worktreeRoot: "/work/alpha/wt", baseBranch: "main", isPaseoOwnedWorktree: true, mainRepoRoot: "/work/alpha", pinnedAt: at, labels: ["a", "b"] }));
await workspaces.upsert(workspace("wks_2", "prj_1", "/work/alpha", { kind: "local_checkout", archivedAt: "2026-09-21T00:00:00.000Z" }));
const put = (name, text) => fs.writeFileSync(path.join(seed, name), text);
put("legacy-projects.json", JSON.stringify([{ projectId: "prj_legacy", rootPath: "/legacy", kind: "non_git", displayName: "legacy", createdAt: at, updatedAt: at, archivedAt: null }], null, 2));
put("extra-keys-workspaces.json", JSON.stringify([{ workspaceId: "wks_extra", projectId: "prj_1", cwd: "/x", kind: "directory", displayName: "x", createdAt: at, updatedAt: at, archivedAt: null, futureField: { a: 1 }, title: null }], null, 2));
const dup = (displayName) => ({ projectId: "prj_dup", rootPath: "/dup", kind: "git", displayName, createdAt: at, updatedAt: at, archivedAt: null });
put("dup-projects.json", JSON.stringify([dup("first"), { ...dup("other"), projectId: "prj_other" }, dup("second")], null, 2));
put("invalid-projects.json", JSON.stringify([dup("ok"), { projectId: "prj_bad" }].map((r, i) => (i === 0 ? { ...r, projectId: "prj_ok" } : r))));
put("invalid-workspaces.json", JSON.stringify([{ workspaceId: "wks_ok", projectId: "p", cwd: "/c", kind: "directory", displayName: "d", createdAt: at, updatedAt: at, archivedAt: null }, { workspaceId: "wks_bad", kind: "nope" }]));
put("corrupt-projects.json", "[ not json");
put("not-an-array-workspaces.json", JSON.stringify({ workspaceId: "wks_1" }));
const out = [];
for (const [file, kind, id] of JSON.parse(filesJson)) {
  failed = false;
  const file_ = path.join(seed, file);
  const registry = kind === "project" ? new mod.FileBackedProjectRegistry(file_, logger) : new mod.FileBackedWorkspaceRegistry(file_, logger);
  await registry.initialize();
  out.push({ file, failed, list: await registry.list(), got: await registry.get(id), missing: await registry.get("nope") });
}
process.stdout.write(JSON.stringify(out));
"#;

fn row(
    file: &str,
    failed: bool,
    list: Vec<JsValue>,
    got: Option<JsValue>,
    missing: Option<JsValue>,
) -> JsValue {
    let mut out = JsObject::new();
    out.insert("file", JsValue::String(file.to_owned()));
    out.insert("failed", JsValue::Bool(failed));
    out.insert("list", JsValue::Array(list));
    out.insert("got", got.unwrap_or(JsValue::Null));
    out.insert("missing", missing.unwrap_or(JsValue::Null));
    JsValue::Object(out)
}

fn rust_output(seed: &Path) -> String {
    let rows = FILES
        .iter()
        .map(|&(file, kind, id)| {
            let path = seed.join(file);
            if kind == "project" {
                let mut registry = ProjectRegistry::new(path);
                let failed = registry.initialize().is_some();
                row(
                    file,
                    failed,
                    registry
                        .list()
                        .iter()
                        .map(RegistryRecord::to_value)
                        .collect(),
                    registry.get(id).map(|record| record.to_value()),
                    registry.get("nope").map(|record| record.to_value()),
                )
            } else {
                let mut registry = WorkspaceRegistry::new(path);
                let failed = registry.initialize().is_some();
                row(
                    file,
                    failed,
                    registry
                        .list()
                        .iter()
                        .map(RegistryRecord::to_value)
                        .collect(),
                    registry.get(id).map(|record| record.to_value()),
                    registry.get("nope").map(|record| record.to_value()),
                )
            }
        })
        .collect();
    stringify(&JsValue::Array(rows))
}

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/workspace-registry.js",
    "30578109d7388b6d0cd0b76a1f1e5e19d1711a6ce13eedcdcb9fbc9eebcb9164",
)];

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

fn disposable() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "spocky-registry-old-home-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("home");
    path
}

#[test]
fn original_made_registries_load_the_same() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: registry old home differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let root = disposable();
    let seed = root.join("seed");
    let files = stringify(&JsValue::Array(
        FILES
            .iter()
            .map(|&(file, kind, id)| {
                JsValue::Array(vec![
                    JsValue::String(file.to_owned()),
                    JsValue::String(kind.to_owned()),
                    JsValue::String(id.to_owned()),
                ])
            })
            .collect(),
    ));
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
        .arg(&seed)
        .arg(files)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rust = rust_output(&seed);
    std::fs::remove_dir_all(&root).expect("remove root");
    assert_eq!(rust, String::from_utf8_lossy(&output.stdout));
}
