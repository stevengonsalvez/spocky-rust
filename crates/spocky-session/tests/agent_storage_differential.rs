//! Differential check of `AgentStorage`'s per-agent write chains against the
//! pinned build: a failed write fails the mutations queued behind it with
//! the same error, `remove` awaits and rethrows it, a delete begun first
//! skips the queued writes, and the chain starts fresh once it drains.
//!
//! Each outcome records the error's name and `code`, and whether it is the
//! first failure itself (`===` in JS, the same shared error in Rust). The
//! `TypeError` message is compared too. A file system error's message is
//! not: node's text (`EISDIR: illegal operation on a directory, rename
//! '<temp>' -> '<path>'`) is not reproduced by the store yet.
//!
//! The JS side lets a write start before `remove` by awaiting microtasks,
//! which never complete file I/O. The Rust side holds the projection open
//! instead. Nothing is normalized: dates are fixed inputs.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::task::Poll;

use spocky_session::agent_projection::{AgentAttention, ManagedAgentRecordView, SnapshotOverrides};
use spocky_session::agent_storage::{AgentStorage, StorageError};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const NODE_SCRIPT: &str = r#"
const [dist] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const fs = await import("node:fs");
const os = await import("node:os");
const path = await import("node:path");
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const logger = { child() { return this; }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
const agent = (id, config) => ({
  id, provider: "codex", cwd: "/w", createdAt: new Date(1700000000000),
  updatedAt: new Date(1700000001000), lastUserMessageAt: null, labels: {}, lifecycle: "idle",
  currentModeId: null, config, persistence: null, attention: { requiresAttention: false },
  internal: false,
});
const failing = () => agent("a1", { provider: "codex", cwd: "/w", toolPolicy: {} });
const record = (id, title) => ({ id, provider: "codex", cwd: "/w", title });
const outcomes = async (promises) => {
  const settled = await Promise.allSettled(promises);
  const first = settled.find((result) => result.status === "rejected")?.reason;
  return settled.map((result) => {
    if (result.status === "fulfilled") return { ok: true };
    const error = result.reason;
    const out = { name: error.constructor.name, code: error.code ?? null, first: error === first };
    if (error instanceof TypeError) out.message = error.message;
    return out;
  });
};
const title = async (storage, id) => (await storage.get(id))?.title ?? null;
const scenario = async (run) => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "spocky-storage-"));
  try {
    const storage = new AgentStorage(home, logger);
    await storage.initialize();
    return await run(storage, home);
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
};
const shortCircuit = await scenario(async (storage) => {
  const queued = [storage.applySnapshot(failing()), storage.upsert(record("a1", "queued"))];
  await storage.flush();
  const settled = await outcomes(queued);
  const afterFailure = await title(storage, "a1");
  await storage.upsert(record("a1", "fresh"));
  return { settled, afterFailure, afterFresh: await title(storage, "a1") };
});
const deleteFirst = await scenario(async (storage) => {
  const settled = await outcomes([
    storage.applySnapshot(failing()),
    storage.upsert(record("a1", "queued")),
    storage.remove("a1"),
  ]);
  const afterRemove = await title(storage, "a1");
  await storage.upsert(record("a1", "late"));
  return { settled, afterRemove, afterLate: await title(storage, "a1") };
});
const fsError = await scenario(async (storage, home) => {
  await storage.upsert(record("a0", "first"));
  const [projectDir] = fs.readdirSync(home);
  const directory = path.join(home, projectDir);
  fs.mkdirSync(path.join(directory, "a1.json"));
  const writes = [
    storage.applySnapshot(agent("a1", { provider: "codex", cwd: "/w" })),
    storage.upsert(record("a1", "queued")),
  ];
  for (let tick = 0; tick < 50; tick += 1) await null;
  const settled = await outcomes([...writes, storage.remove("a1")]);
  return {
    settled,
    afterRemove: await title(storage, "a1"),
    entries: fs.readdirSync(directory).length,
    blocked: fs.statSync(path.join(directory, "a1.json")).isDirectory(),
  };
});
process.stdout.write(JSON.stringify({ shortCircuit, deleteFirst, fsError }));
"#;

fn view(id: &str, config: &str) -> ManagedAgentRecordView {
    ManagedAgentRecordView {
        id: id.to_owned(),
        provider: "codex".to_owned(),
        cwd: "/w".to_owned(),
        workspace_id: None,
        created_at_millis: 1_700_000_000_000,
        updated_at_millis: 1_700_000_001_000,
        last_user_message_at_millis: None,
        labels: parse("{}").expect("labels"),
        lifecycle: "idle".to_owned(),
        current_mode_id: None,
        config: parse(config).expect("config"),
        runtime_info: None,
        features: None,
        persistence: None,
        last_error: None,
        attention: AgentAttention::None,
        internal: Some(false),
        owner: None,
    }
}

const FAILING: &str = r#"{"provider":"codex","cwd":"/w","toolPolicy":{}}"#;

fn record(id: &str, title: &str) -> JsValue {
    parse(&format!(
        r#"{{"id":"{id}","provider":"codex","cwd":"/w","title":"{title}"}}"#
    ))
    .expect("record")
}

/// Polls `future` once, as a JS call runs up to its first `await`.
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|context| {
        Poll::Ready(match Pin::new(&mut *future).poll(context) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        })
    })
    .await
}

/// A snapshot projection held open until `open` is sent.
fn gated(
    id: &'static str,
    config: &'static str,
) -> (
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    impl FnOnce() -> ManagedAgentRecordView + Send,
) {
    let (entered_tx, entered) = mpsc::channel();
    let (open, gate) = mpsc::channel::<()>();
    let agent = move || {
        entered_tx.send(()).expect("entered");
        gate.recv().expect("gate opened");
        view(id, config)
    };
    (entered, open, agent)
}

async fn wait_entered(entered: mpsc::Receiver<()>) {
    tokio::task::spawn_blocking(move || entered.recv())
        .await
        .expect("join")
        .expect("projection started");
}

fn io_code(error: &io::Error) -> JsValue {
    match error.kind() {
        io::ErrorKind::IsADirectory => JsValue::String("EISDIR".to_owned()),
        other => JsValue::String(format!("{other:?}")),
    }
}

/// The `io::Error` a store error wraps, at any depth.
fn io_source<'a>(error: &'a (dyn std::error::Error + 'static)) -> &'a io::Error {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<io::Error>() {
            return io;
        }
        current = error.source();
    }
    panic!("store error without an io::Error source")
}

fn same(left: &StorageError, right: &StorageError) -> bool {
    match (left, right) {
        (StorageError::Projection(left), StorageError::Projection(right)) => left == right,
        (StorageError::Store(left), StorageError::Store(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn outcomes<T>(results: &[Result<T, StorageError>]) -> JsValue {
    let first = results.iter().find_map(|result| result.as_ref().err());
    JsValue::Array(
        results
            .iter()
            .map(|result| {
                let mut out = JsObject::new();
                match result {
                    Ok(_) => out.insert("ok", JsValue::Bool(true)),
                    Err(error) => {
                        let (name, code) = match error {
                            StorageError::Projection(_) => ("TypeError", JsValue::Null),
                            StorageError::Store(store) => {
                                ("Error", io_code(io_source(store.as_ref())))
                            }
                        };
                        out.insert("name", JsValue::String(name.to_owned()));
                        out.insert("code", code);
                        out.insert(
                            "first",
                            JsValue::Bool(first.is_some_and(|first| same(first, error))),
                        );
                        if let StorageError::Projection(type_error) = error {
                            out.insert("message", JsValue::String(type_error.0.clone()));
                        }
                    }
                }
                JsValue::Object(out)
            })
            .collect(),
    )
}

async fn title(storage: &AgentStorage, id: &str) -> JsValue {
    storage
        .get(id)
        .await
        .and_then(|record| record.get("title").cloned())
        .unwrap_or(JsValue::Null)
}

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn scenario(name: &str) -> (Home, AgentStorage) {
    let home = Home(std::env::temp_dir().join(format!(
        "spocky-storage-differential-{name}-{}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&home.0);
    let storage = AgentStorage::new(&home.0);
    storage.initialize().await;
    (home, storage)
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key, value);
    }
    JsValue::Object(out)
}

async fn short_circuit() -> JsValue {
    let (_home, storage) = scenario("short-circuit").await;
    let mut snapshot = Box::pin(storage.apply_snapshot(
        "a1",
        || view("a1", FAILING),
        SnapshotOverrides::default(),
    ));
    let mut upsert = Box::pin(storage.upsert(record("a1", "queued")));
    assert!(poll_once(&mut snapshot).await.is_none());
    assert!(poll_once(&mut upsert).await.is_none());
    storage.flush().await;
    let settled = outcomes(&[snapshot.await, upsert.await]);
    let after_failure = title(&storage, "a1").await;
    storage
        .upsert(record("a1", "fresh"))
        .await
        .expect("fresh write");
    object(vec![
        ("settled", settled),
        ("afterFailure", after_failure),
        ("afterFresh", title(&storage, "a1").await),
    ])
}

async fn delete_first() -> JsValue {
    let (_home, storage) = scenario("delete-first").await;
    let mut snapshot = Box::pin(storage.apply_snapshot(
        "a1",
        || view("a1", FAILING),
        SnapshotOverrides::default(),
    ));
    let mut upsert = Box::pin(storage.upsert(record("a1", "queued")));
    let mut remove = Box::pin(storage.remove("a1"));
    assert!(poll_once(&mut snapshot).await.is_none());
    assert!(poll_once(&mut upsert).await.is_none());
    assert!(poll_once(&mut remove).await.is_none());
    let snapshot = snapshot.await;
    let upsert = upsert.await;
    let remove = remove.await.map(|failures| assert!(failures.is_empty()));
    let settled = outcomes(&[snapshot, upsert, remove]);
    let after_remove = title(&storage, "a1").await;
    storage
        .upsert(record("a1", "late"))
        .await
        .expect("skipped write");
    object(vec![
        ("settled", settled),
        ("afterRemove", after_remove),
        ("afterLate", title(&storage, "a1").await),
    ])
}

fn only_entry(directory: &Path) -> PathBuf {
    let entries: Vec<_> = std::fs::read_dir(directory)
        .expect("home")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(entries.len(), 1, "one project directory");
    entries.into_iter().next().expect("project directory")
}

async fn fs_error() -> JsValue {
    let (home, storage) = scenario("fs-error").await;
    storage
        .upsert(record("a0", "first"))
        .await
        .expect("first write");
    let directory = only_entry(&home.0);
    std::fs::create_dir(directory.join("a1.json")).expect("blocking directory");
    let (entered, open, agent) = gated("a1", r#"{"provider":"codex","cwd":"/w"}"#);
    let mut snapshot = Box::pin(storage.apply_snapshot("a1", agent, SnapshotOverrides::default()));
    let mut upsert = Box::pin(storage.upsert(record("a1", "queued")));
    assert!(poll_once(&mut snapshot).await.is_none());
    assert!(poll_once(&mut upsert).await.is_none());
    wait_entered(entered).await;
    let mut remove = Box::pin(storage.remove("a1"));
    assert!(poll_once(&mut remove).await.is_none());
    open.send(()).expect("open");
    let snapshot = snapshot.await;
    let upsert = upsert.await;
    let remove = remove.await;
    let settled = outcomes(&[snapshot, upsert, remove.map(|_| ())]);
    let after_remove = title(&storage, "a1").await;
    let entries = std::fs::read_dir(&directory).expect("project").count();
    object(vec![
        ("settled", settled),
        ("afterRemove", after_remove),
        (
            "entries",
            JsValue::Number(f64::from(u32::try_from(entries).expect("count"))),
        ),
        ("blocked", JsValue::Bool(directory.join("a1.json").is_dir())),
    ])
}

/// The pinned dist module this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with its SHA-256: a different build fails instead of silently passing.
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

#[tokio::test]
async fn write_chains_match_pinned_storage() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: storage differential not run");
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
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rust = object(vec![
        ("shortCircuit", short_circuit().await),
        ("deleteFirst", delete_first().await),
        ("fsError", fs_error().await),
    ]);
    assert_eq!(stringify(&rust), String::from_utf8_lossy(&output.stdout));
}
