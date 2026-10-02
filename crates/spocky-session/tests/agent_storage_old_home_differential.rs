//! Old-state home check for G4: a home the original wrote loads the same
//! under `AgentStorage` on the Rust side as under the pinned build.
//!
//! The pinned `AgentStorage` writes records through its own `upsert` (so
//! the files are the original's), and the test adds the other shapes an
//! old home holds: a record at the home root, a corrupt file, a record
//! that fails the schema, a non-JSON file, a nested directory, and the
//! same id in two project directories, and names whose byte order differs
//! from case-folded or locale order. Node loads a copy of that home and
//! so does Rust. `list()` (order included), `get` hits and misses, and the
//! file tree afterwards must match.
//!
//! The reverse direction is the byte-for-byte comparison of what Rust
//! writes in `agent_manager_differential` and `agent_storage_differential`:
//! equal bytes load identically on the original.
//!
//! Every input is fixed, so nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_session::agent_storage::AgentStorage;
use spocky_store::js_value::{JsObject, JsValue, stringify};

const NODE_SCRIPT: &str = r#"
const [dist, seed, nodeHome] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const fs = await import("node:fs");
const path = await import("node:path");
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const record = (id, cwd, extra = {}) => ({
  id, provider: "codex", cwd, workspaceId: "wks_1",
  createdAt: "2026-09-20T09:00:00.000Z", updatedAt: "2026-09-20T09:05:00.000Z",
  lastActivityAt: "2026-09-20T09:04:59.000Z", lastUserMessageAt: null, title: `Agent ${id}`,
  labels: { lane: "state", "paseo.parent-agent-id": "parent" }, lastStatus: "closed", lastModeId: "auto",
  config: { modeId: "auto", model: "gpt", thinkingOptionId: null, featureValues: { fast: true, future: { on: true } },
            providerOptions: { x: ["a", 7, false] }, mcpServers: { c: { type: "http", url: "https://e.invalid/mcp", future: { r: 3 } } } },
  runtimeInfo: { provider: "codex", sessionId: `s-${id}`, model: "gpt", modeId: "auto", extra: { future: { n: "kept" } } },
  features: [{ type: "toggle", id: "fast", label: "Fast", value: true }],
  persistence: { provider: "codex", sessionId: `s-${id}`, nativeHandle: "h", metadata: { cwd, mcpServers: {} } },
  requiresAttention: false, attentionReason: null, attentionTimestamp: null, archivedAt: null,
  ...extra,
});
const writer = new AgentStorage(seed, logger);
await writer.upsert(record("a1", "/work/alpha"));
await writer.upsert(record("a2", "/work/alpha", { archivedAt: "2026-09-21T00:00:00.000Z", internal: true, lastStatus: "idle", labels: {} }));
await writer.upsert(record("b1", "/work/beta project", { owner: { kind: "user" }, lastError: "boom", requiresAttention: true, attentionReason: "error", attentionTimestamp: "2026-09-20T09:06:00.000Z" }));
await writer.upsert(record("c1", "/", { title: null, config: null, runtimeInfo: undefined, features: undefined }));
// Project directories whose names differ in byte order from case-folded
// or locale order: uppercase before lowercase, non-ASCII last.
await writer.upsert(record("u1", "/Work/Zeta"));
await writer.upsert(record("u2", "/work/zeta"));
await writer.upsert(record("u3", "/work/\u00e9clair"));
const put = (relative, text) => {
  const file = path.join(seed, relative);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
};
put("legacy-root.json", JSON.stringify(record("r1", "/legacy/root"), null, 2));
put("Upper-root.json", JSON.stringify(record("r2", "/legacy/upper"), null, 2));
put("lower-root.json", JSON.stringify(record("r3", "/legacy/lower"), null, 2));
put("\u00e9-root.json", JSON.stringify(record("r4", "/legacy/accent"), null, 2));
put("corrupt/zz-corrupt.json", "{ not json");
put("invalid/zz-invalid.json", JSON.stringify({ id: "bad", provider: "codex" }));
put("notes.txt", "not a record");
put("nested/deeper/ignored.json", JSON.stringify(record("deep", "/deep")));
put("dup-one/d1.json", JSON.stringify(record("d1", "/dup/one", { title: "first copy" }), null, 2));
put("dup-two/d1.json", JSON.stringify(record("d1", "/dup/two", { title: "second copy" }), null, 2));
const tree = (root) => {
  const out = [];
  const walk = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true }).sort((l, r) => (l.name < r.name ? -1 : 1))) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(full);
      else out.push(path.relative(root, full));
    }
  };
  walk(root);
  return out;
};
fs.cpSync(seed, nodeHome, { recursive: true });
const storage = new AgentStorage(nodeHome, logger);
await storage.initialize();
const list = await storage.list();
const out = {
  list,
  get: [await storage.get("a1"), await storage.get("d1"), await storage.get("bad"), await storage.get("deep"), await storage.get("u1"), await storage.get("r4")],
  tree: tree(nodeHome),
};
process.stdout.write(JSON.stringify(out));
"#;

fn tree(root: &Path) -> JsValue {
    fn walk(directory: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(directory)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push(
                    path.strip_prefix(root)
                        .expect("prefix")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    // node sorts entries by name within each directory; so does this.
    JsValue::Array(files.into_iter().map(JsValue::String).collect())
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy");
        }
    }
}

async fn rust_output(seed: &Path, home: &Path) -> String {
    copy_tree(seed, home);
    let storage = AgentStorage::new(home);
    storage.initialize().await;
    let mut out = JsObject::new();
    out.insert("list", JsValue::Array(storage.list().await));
    let mut gets = Vec::new();
    for id in ["a1", "d1", "bad", "deep", "u1", "r4"] {
        gets.push(storage.get(id).await.unwrap_or(JsValue::Null));
    }
    out.insert("get", JsValue::Array(gets));
    out.insert("tree", tree(home));
    stringify(&JsValue::Object(out))
}

const PINNED_MODULES: &[(&str, &str)] = &[(
    "server/agent/agent-storage.js",
    "f1e3ccb1cf1e4caf75084627304450ddab8f93098291049819b312174f1e17ea",
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

fn disposable(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "spocky-old-home-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("home");
    path
}

#[tokio::test]
async fn original_made_home_loads_the_same() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: old home differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
    let root = disposable("root");
    let (seed, node_home, rust_home) = (root.join("seed"), root.join("node"), root.join("rust"));
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
        .arg(&node_home)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rust = rust_output(&seed, &rust_home).await;
    std::fs::remove_dir_all(&root).expect("remove root");
    assert_eq!(rust, String::from_utf8_lossy(&output.stdout));
}
