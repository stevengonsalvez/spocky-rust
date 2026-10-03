//! Differential check of the workspace label service against the pinned
//! Paseo build. `labels_driver.mjs` runs `labels_scenarios.json` with the
//! pinned node against the built `server/workspace-labels` modules; this file
//! runs the same script against the Rust port. Each step prints one line with
//! its result or error; the raw lines must match after normalization.
//!
//! Normalization (each covered by a test below) replaces only the disposable
//! root path with `<root>`, the `randomUUID()` generation with `<uuid>` and a
//! `new Date().toISOString()` stamp the services generate with `<now>`. The
//! script's own fixed timestamps, results, error text, ordering and file bytes
//! are never normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0) and `SPOCKY_PASEO_DIST` (the
//! pinned build's `packages/server/dist/server`). Without them the tests FAIL;
//! `SPOCKY_ALLOW_SKIP=1` (exactly) skips them explicitly outside the gate.
//! `SPOCKY_LABELS_EVIDENCE` names a directory that receives the raw and
//! normalized output of both sides.
//!
//! Failure injection is in the script, so the scenarios need no permissions
//! tricks and run on every platform.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use spocky_contracts::js_value::{JsObject, JsValue, js_text, parse, stringify};
use spocky_store::StoreError;
use spocky_store::atomic::write_json_atomic;
use spocky_store::registry::{
    PersistedWorkspaceRecord, RegistryRecord, WorkspaceKind, WorkspaceRegistry,
};
use spocky_workspace_labels::catalog_store::{
    remove_file, write_catalog_file, write_transaction_file,
};
use spocky_workspace_labels::sequence::{
    SequencedChange, SubscriberError, WorkspaceLabelChange, WorkspaceLabelCursor,
    WorkspaceLabelSync,
};
use spocky_workspace_labels::{
    CatalogIo, LabelError, TransactionPhase, WorkspaceLabelColor, WorkspaceLabelDefinition,
    WorkspaceLabelService, WorkspaceLabelServiceOptions, WorkspaceLabelTransaction,
    create_workspace_label_service,
};

const SCENARIOS: &str = include_str!("labels_scenarios.json");
const DRIVER: &str = include_str!("labels_driver.mjs");

/// SHA-256 of the pinned dist modules under test, recorded in
/// `evidence/phase4/daemon-svc-labels.md`.
const PINNED_MODULES: [(&str, &str); 6] = [
    (
        "server/workspace-labels/index.js",
        "2038e94cff0576cb3a0f7519bc003e1e12182add621f0feb8e26488156355f8a",
    ),
    (
        "server/workspace-labels/internal/catalog-store.js",
        "35f571009cb2ae1c687f4f875ef03a5b9c4b8103b56ecba7e73a0402ca0526eb",
    ),
    (
        "server/workspace-labels/internal/sequence.js",
        "bd2910debb9ae0ce21aeeec4a2d069dad5fd96f8f8a8e2c8af650498089ac972",
    ),
    (
        "server/workspace-labels/internal/service.js",
        "d1a6fdc1ea0ea8cc9e40196b8185fcd1ab2836df7a46b61b249e0e4228f57c05",
    ),
    (
        "server/workspace-registry.js",
        "30578109d7388b6d0cd0b76a1f1e5e19d1711a6ce13eedcdcb9fbc9eebcb9164",
    ),
    (
        "server/atomic-file.js",
        "835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25",
    ),
];

/// A disposable root, removed on drop.
struct Disposable(PathBuf);

impl Drop for Disposable {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn disposable_root(side: &str) -> (Disposable, String) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    // The same parent on both sides keeps path lengths, hence error text, alike.
    let root = PathBuf::from("/private/tmp").join(format!(
        "spocky-labels-{side}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create disposable root");
    let text = root.to_string_lossy().into_owned();
    (Disposable(root), text)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Fails unless `node` reports v22.20.0 and `dist` holds the pinned modules.
fn verify_pinned(node: &std::ffi::OsStr, dist: &std::ffi::OsStr) {
    let version = Command::new(node)
        .args(["-p", "process.version"])
        .output()
        .expect("run pinned node");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.20.0",
        "SPOCKY_PINNED_NODE is not node 22.20.0"
    );
    for (module, expected) in PINNED_MODULES {
        let path = Path::new(dist).join(module);
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(
            sha256_hex(&bytes),
            expected,
            "{} is not the pinned build",
            path.display()
        );
    }
}

fn pinned_inputs() -> Option<(std::ffi::OsString, std::ffi::OsString)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => {
            verify_pinned(&node, &dist);
            Some((node, dist))
        }
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: pinned differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

fn timeout_program() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

fn run_node(
    node: &std::ffi::OsStr,
    dist: &std::ffi::OsStr,
    root: &str,
    scenarios: &Path,
) -> String {
    let driver = scenarios.with_file_name("labels_driver.mjs");
    fs::write(&driver, DRIVER).expect("write driver");
    let output = Command::new(timeout_program())
        .args(["--kill-after=5", "300"])
        .arg(node)
        .arg(&driver)
        .arg(dist)
        .arg(root)
        .arg(scenarios)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("node prints UTF-8")
}

// ---- the script ----------------------------------------------------------

fn text<'a>(value: &'a JsValue, key: &str) -> &'a str {
    value.get(key).and_then(JsValue::as_str).unwrap_or_default()
}

fn optional_text<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

fn truthy(value: &JsValue, key: &str) -> bool {
    matches!(value.get(key), Some(JsValue::Bool(true)))
}

fn string(value: &str) -> JsValue {
    JsValue::String(js_text(value))
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

#[allow(clippy::cast_precision_loss)]
fn number(value: u64) -> JsValue {
    JsValue::Number(value as f64)
}

/// One injected failure, as the script spells it.
#[derive(Clone)]
struct Fault {
    flag: Option<String>,
    mode: Option<String>,
    error: String,
    once: bool,
    phase: Option<String>,
    has: Option<String>,
    non_empty: bool,
    empty: bool,
    min_write: Option<usize>,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn faults_of(step: &JsValue, key: &str) -> Vec<Fault> {
    step.get(key)
        .and_then(JsValue::as_array)
        .unwrap_or_default()
        .iter()
        .map(|fault| Fault {
            flag: optional_text(fault, "flag").map(str::to_owned),
            mode: optional_text(fault, "mode").map(str::to_owned),
            error: text(fault, "error").to_owned(),
            once: truthy(fault, "once"),
            phase: optional_text(fault, "phase").map(str::to_owned),
            has: optional_text(fault, "has").map(str::to_owned),
            non_empty: truthy(fault, "nonEmpty"),
            empty: truthy(fault, "empty"),
            min_write: match fault.get("minWrite") {
                Some(JsValue::Number(count)) => Some(*count as usize),
                _ => None,
            },
        })
        .collect()
}

#[derive(Default)]
struct Context {
    phase: Option<&'static str>,
    names: Option<Vec<String>>,
    write: usize,
}

type Flags = Arc<Mutex<HashMap<String, bool>>>;

/// The first fault whose flag and conditions hold; `once` flags are consumed.
fn firing(faults: &[Fault], flags: &Flags, context: &Context) -> Option<Fault> {
    let mut flags = flags.lock().expect("flags");
    for fault in faults {
        if let Some(flag) = &fault.flag
            && !flags.get(flag).copied().unwrap_or(false)
        {
            continue;
        }
        if fault
            .phase
            .as_deref()
            .is_some_and(|phase| context.phase != Some(phase))
        {
            continue;
        }
        let names = context.names.as_deref();
        if fault
            .has
            .as_ref()
            .is_some_and(|has| !names.is_some_and(|names| names.contains(has)))
        {
            continue;
        }
        if fault.non_empty && names.is_none_or(<[String]>::is_empty) {
            continue;
        }
        if fault.empty && names.map(<[String]>::len) != Some(0) {
            continue;
        }
        if fault
            .min_write
            .is_some_and(|minimum| context.write < minimum)
        {
            continue;
        }
        if fault.once
            && let Some(flag) = &fault.flag
        {
            flags.insert(flag.clone(), false);
        }
        return Some(fault.clone());
    }
    None
}

/// `faultedWrite`: raise before or after the real write.
fn faulted(
    faults: &[Fault],
    flags: &Flags,
    context: &Context,
    write: impl FnOnce() -> Result<(), LabelError>,
) -> Result<(), LabelError> {
    let fault = firing(faults, flags, context);
    let raise = |fault: &Fault| LabelError::Store(StoreError::Message(fault.error.clone()));
    if let Some(fault) = fault
        .as_ref()
        .filter(|fault| fault.mode.as_deref() == Some("before"))
    {
        return Err(raise(fault));
    }
    write()?;
    match fault
        .as_ref()
        .filter(|fault| fault.mode.as_deref() == Some("after"))
    {
        Some(fault) => Err(raise(fault)),
        None => Ok(()),
    }
}

type Registry = Arc<Mutex<WorkspaceRegistry>>;
type Service = WorkspaceLabelService<Registry>;

#[derive(Default)]
struct Logs {
    watching: HashSet<String>,
    /// Registry id, then its publications, in creation order.
    publications: Vec<(String, Vec<JsValue>)>,
    /// Subscription id, then its change events, in subscription order.
    changes: Vec<(String, Vec<JsValue>)>,
}

impl Logs {
    fn publish(&mut self, registry: &str, kind: &str, workspace_id: &str) {
        if !self.watching.contains(registry) {
            return;
        }
        let entry = object(vec![
            ("kind", string(kind)),
            ("workspaceId", string(workspace_id)),
        ]);
        if let Some((_, list)) = self.publications.iter_mut().find(|(id, _)| id == registry) {
            list.push(entry);
        }
    }
}

struct Subscription {
    service: String,
    snapshot: WorkspaceLabelSync,
    id: u64,
}

struct Scenario {
    home: PathBuf,
    flags: Flags,
    registries: Vec<(String, Registry)>,
    services: HashMap<String, Service>,
    subscriptions: HashMap<String, Subscription>,
    logs: Arc<Mutex<Logs>>,
}

fn definition_value(label: &WorkspaceLabelDefinition) -> JsValue {
    object(vec![
        ("name", string(&label.name)),
        ("color", string(label.color.as_str())),
    ])
}

fn labels_value(labels: &[WorkspaceLabelDefinition]) -> JsValue {
    JsValue::Array(labels.iter().map(definition_value).collect())
}

fn snapshot_value(snapshot: &WorkspaceLabelSync) -> JsValue {
    let removals = snapshot
        .sync
        .removals
        .iter()
        .map(|removal| {
            object(vec![
                ("name", string(&removal.name)),
                ("seq", number(removal.seq)),
            ])
        })
        .collect();
    object(vec![
        ("labels", labels_value(&snapshot.labels)),
        (
            "sync",
            object(vec![
                ("mode", string(snapshot.sync.mode.as_str())),
                ("generation", string(&snapshot.sync.generation)),
                ("headSeq", number(snapshot.sync.head_seq)),
                ("removals", JsValue::Array(removals)),
            ]),
        ),
    ])
}

fn change_value(entry: &SequencedChange) -> JsValue {
    let mut fields = Vec::new();
    match &entry.change {
        WorkspaceLabelChange::Upsert {
            label,
            previous_name,
        } => {
            fields.push(("kind", string("upsert")));
            fields.push(("label", definition_value(label)));
            if let Some(previous) = previous_name {
                fields.push(("previousName", string(previous)));
            }
        }
        WorkspaceLabelChange::Remove { name } => {
            fields.push(("kind", string("remove")));
            fields.push(("name", string(name)));
        }
    }
    fields.push(("generation", string(&entry.generation)));
    fields.push(("seq", number(entry.seq)));
    object(fields)
}

fn failure(code: Option<String>, message: &str) -> JsValue {
    object(vec![(
        "error",
        object(vec![
            ("code", code.map_or(JsValue::Null, |code| string(&code))),
            ("message", string(message)),
        ]),
    )])
}

fn label_failure(error: &LabelError) -> JsValue {
    failure(error.code(), &error.to_string())
}

fn store_failure(error: &StoreError) -> JsValue {
    let code = match error {
        StoreError::Fs(error) => Some(error.code()),
        _ => None,
    };
    failure(code, &error.to_string())
}

fn workspace_value(record: Option<&PersistedWorkspaceRecord>) -> JsValue {
    record.map_or(JsValue::Null, RegistryRecord::to_value)
}

fn color_of(value: &str) -> WorkspaceLabelColor {
    WorkspaceLabelColor::parse(value).expect("script colour")
}

impl Scenario {
    fn registry(&self, id: &str) -> &Registry {
        &self
            .registries
            .iter()
            .find(|(existing, _)| existing == id)
            .unwrap_or_else(|| panic!("registry {id}"))
            .1
    }

    fn add_registry(&mut self, step: &JsValue) {
        let id = text(step, "id").to_owned();
        let faults = faults_of(step, "fault");
        let flags = Arc::clone(&self.flags);
        let calls = AtomicUsize::new(0);
        let file = self.home.join("projects").join(text(step, "file"));
        let writer = Box::new(
            move |path: &Path, records: &str| -> Result<(), StoreError> {
                let write = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let context = Context {
                    write,
                    ..Context::default()
                };
                faulted(&faults, &flags, &context, || {
                    write_json_atomic(path, records).map_err(LabelError::from)
                })
                .map_err(|error| match error {
                    LabelError::Store(error) => error,
                    other => StoreError::Message(other.to_string()),
                })
            },
        );
        let mut registry = WorkspaceRegistry::new(file).with_writer(writer);
        for workspace_id in step
            .get("workspaces")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            registry
                .upsert(PersistedWorkspaceRecord {
                    workspace_id: workspace_id.as_str().unwrap_or_default().to_owned(),
                    project_id: "prj_one".to_owned(),
                    cwd: "/repo".to_owned(),
                    kind: WorkspaceKind::LocalCheckout,
                    display_name: "main".to_owned(),
                    title: None,
                    branch: None,
                    worktree_root: None,
                    base_branch: None,
                    is_paseo_owned_worktree: false,
                    main_repo_root: None,
                    created_at: "2026-08-14T00:00:00.000Z".to_owned(),
                    updated_at: "2026-08-14T00:00:00.000Z".to_owned(),
                    archived_at: None,
                    auto_archived_change_request_url: None,
                    pinned_at: None,
                    labels: None,
                    untrusted_source: None,
                })
                .expect("setup upsert");
        }
        self.logs
            .lock()
            .expect("logs")
            .publications
            .push((id.clone(), Vec::new()));
        self.registries.push((id, Arc::new(Mutex::new(registry))));
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn add_service(&mut self, step: &JsValue) {
        let registry_id = text(step, "registry").to_owned();
        let catalog_faults = faults_of(step, "catalog");
        let transaction_faults = faults_of(step, "transaction");
        let remove_faults = faults_of(step, "remove");
        let (c_flags, t_flags, r_flags) = (
            Arc::clone(&self.flags),
            Arc::clone(&self.flags),
            Arc::clone(&self.flags),
        );
        let io = CatalogIo {
            write_catalog: Box::new(move |path: &Path, labels: &[WorkspaceLabelDefinition]| {
                let context = Context {
                    names: Some(labels.iter().map(|label| label.name.clone()).collect()),
                    ..Context::default()
                };
                faulted(&catalog_faults, &c_flags, &context, || {
                    write_catalog_file(path, labels)
                })
            }),
            write_transaction: Box::new(
                move |path: &Path, transaction: &WorkspaceLabelTransaction| {
                    let context = Context {
                        phase: Some(match transaction.phase {
                            TransactionPhase::Prepared => "prepared",
                            TransactionPhase::Committed => "committed",
                        }),
                        ..Context::default()
                    };
                    faulted(&transaction_faults, &t_flags, &context, || {
                        write_transaction_file(path, transaction)
                    })
                },
            ),
            remove_transaction: Box::new(move |path: &Path| {
                match firing(&remove_faults, &r_flags, &Context::default()) {
                    Some(fault) => Err(LabelError::Store(StoreError::Message(fault.error))),
                    None => remove_file(path),
                }
            }),
        };
        let logs = Arc::clone(&self.logs);
        let publisher_registry = registry_id.clone();
        let journal_limit = match step.get("journalLimit") {
            Some(JsValue::Number(limit)) => Some(*limit as usize),
            _ => None,
        };
        let service = create_workspace_label_service(WorkspaceLabelServiceOptions {
            paseo_home: self.home.join(text(step, "home")),
            workspace_registry: Arc::clone(self.registry(&registry_id)),
            io,
            publisher: Some(Box::new(move |workspace: &PersistedWorkspaceRecord| {
                logs.lock().expect("logs").publish(
                    &publisher_registry,
                    "upsert",
                    &workspace.workspace_id,
                );
            })),
            journal_limit,
        });
        self.services.insert(text(step, "id").to_owned(), service);
    }

    fn walk(directory: &Path, relative: &str, files: &mut Vec<JsValue>) {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .expect("read dir")
            .map(|entry| entry.expect("entry"))
            .collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let entry_relative = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if entry.path().is_dir() {
                Self::walk(&entry.path(), &entry_relative, files);
            } else {
                let content = String::from_utf8_lossy(&fs::read(entry.path()).expect("read file"))
                    .into_owned();
                files.push(JsValue::Array(vec![
                    string(&entry_relative),
                    string(&content),
                ]));
            }
        }
    }

    fn dump(&self) -> JsValue {
        let mut records = JsObject::new();
        for (id, registry) in &self.registries {
            let list = registry.lock().expect("registry").list();
            records.insert(
                id.clone(),
                JsValue::Array(list.iter().map(RegistryRecord::to_value).collect()),
            );
        }
        let logs = self.logs.lock().expect("logs");
        let mut publications = JsObject::new();
        for (id, list) in &logs.publications {
            publications.insert(id.clone(), JsValue::Array(list.clone()));
        }
        let mut changes = JsObject::new();
        for (id, list) in &logs.changes {
            changes.insert(id.clone(), JsValue::Array(list.clone()));
        }
        let mut files = Vec::new();
        Self::walk(&self.home, "", &mut files);
        object(vec![
            ("records", JsValue::Object(records)),
            ("publications", JsValue::Object(publications)),
            ("changes", JsValue::Object(changes)),
            ("files", JsValue::Array(files)),
        ])
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn subscribe(&mut self, step: &JsValue) -> JsValue {
        let cursor = match step.get("from") {
            Some(JsValue::String(from)) => {
                let sync = &self.subscriptions[from.as_str()].snapshot.sync;
                Some(WorkspaceLabelCursor {
                    generation: sync.generation.clone(),
                    after_seq: sync.head_seq,
                })
            }
            Some(from @ JsValue::Object(_)) => Some(WorkspaceLabelCursor {
                generation: text(from, "generation").to_owned(),
                after_seq: match from.get("afterSeq") {
                    Some(JsValue::Number(seq)) => *seq as u64,
                    _ => 0,
                },
            }),
            _ => None,
        };
        let id = text(step, "id").to_owned();
        {
            let mut logs = self.logs.lock().expect("logs");
            match logs
                .changes
                .iter_mut()
                .find(|(existing, _)| *existing == id)
            {
                Some((_, list)) => list.clear(),
                None => logs.changes.push((id.clone(), Vec::new())),
            }
        }
        let logs = Arc::clone(&self.logs);
        let log_id = id.clone();
        let throwing = truthy(step, "throwing");
        let result = self.services[text(step, "service")].subscribe(
            cursor.as_ref(),
            Box::new(move |entry: &SequencedChange| {
                if throwing {
                    return Err(SubscriberError);
                }
                let mut logs = logs.lock().expect("logs");
                if let Some((_, list)) = logs.changes.iter_mut().find(|(id, _)| *id == log_id) {
                    list.push(change_value(entry));
                }
                Ok(())
            }),
        );
        match result {
            Ok(subscription) => {
                let value = snapshot_value(&subscription.snapshot);
                self.subscriptions.insert(
                    id,
                    Subscription {
                        service: text(step, "service").to_owned(),
                        snapshot: subscription.snapshot,
                        id: subscription.id,
                    },
                );
                value
            }
            Err(error) => label_failure(&error),
        }
    }

    fn registry_op(&self, step: &JsValue) -> JsValue {
        let registry_id = text(step, "registry");
        let workspace_id = text(step, "ws");
        let mut registry = self.registry(registry_id).lock().expect("registry");
        match text(step, "kind") {
            "get" => workspace_value(registry.get(workspace_id).as_ref()),
            "archive" => match registry.archive(workspace_id, text(step, "at"), None) {
                Ok(record) => {
                    if record.is_some() {
                        self.logs.lock().expect("logs").publish(
                            registry_id,
                            "archive",
                            workspace_id,
                        );
                    }
                    string("ok")
                }
                Err(error) => store_failure(&error),
            },
            _ => {
                let title = text(step, "title").to_owned();
                match registry.update(workspace_id, |workspace| PersistedWorkspaceRecord {
                    title: Some(title),
                    ..workspace.clone()
                }) {
                    Ok(record) => {
                        if record.is_some() {
                            self.logs.lock().expect("logs").publish(
                                registry_id,
                                "upsert",
                                workspace_id,
                            );
                        }
                        workspace_value(record.as_ref())
                    }
                    Err(error) => store_failure(&error),
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run(&mut self, step: &JsValue) -> JsValue {
        let service = || &self.services[text(step, "service")];
        match text(step, "op") {
            "registry" => {
                self.add_registry(step);
                string("ok")
            }
            "service" => {
                self.add_service(step);
                string("ok")
            }
            "flag" => {
                let value = truthy(step, "value");
                self.flags
                    .lock()
                    .expect("flags")
                    .insert(text(step, "name").to_owned(), value);
                string("ok")
            }
            "watch" => {
                self.logs
                    .lock()
                    .expect("logs")
                    .watching
                    .insert(text(step, "registry").to_owned());
                string("ok")
            }
            "clearLog" => {
                let mut logs = self.logs.lock().expect("logs");
                logs.publications
                    .iter_mut()
                    .for_each(|(_, list)| list.clear());
                logs.changes.iter_mut().for_each(|(_, list)| list.clear());
                string("ok")
            }
            "subscribe" => self.subscribe(step),
            "unsubscribe" => {
                let subscription = &self.subscriptions[text(step, "id")];
                self.services[subscription.service.as_str()].unsubscribe(subscription.id);
                string("ok")
            }
            "initialize" => match service().initialize() {
                Ok(()) => string("ok"),
                Err(error) => label_failure(&error),
            },
            "assign" => {
                let label = WorkspaceLabelDefinition {
                    name: text(step, "name").to_owned(),
                    color: color_of(text(step, "color")),
                };
                match service().set_assignment(text(step, "ws"), &label, truthy(step, "assigned")) {
                    Ok(assignment) => object(vec![
                        ("label", definition_value(&assignment.label)),
                        (
                            "workspaceLabels",
                            JsValue::Array(
                                assignment
                                    .workspace_labels
                                    .iter()
                                    .map(|l| string(l))
                                    .collect(),
                            ),
                        ),
                    ]),
                    Err(error) => label_failure(&error),
                }
            }
            "update" => {
                let color = optional_text(step, "color").map(color_of);
                match service().update(text(step, "name"), optional_text(step, "newName"), color) {
                    Ok(update) => object(vec![
                        ("label", definition_value(&update.label)),
                        (
                            "affectedWorkspaceCount",
                            number(update.affected_workspace_count as u64),
                        ),
                    ]),
                    Err(error) => label_failure(&error),
                }
            }
            "delete" => match service().delete(text(step, "name")) {
                Ok(affected) => object(vec![("affectedWorkspaceCount", number(affected as u64))]),
                Err(error) => label_failure(&error),
            },
            "count" => match service().count_affected_workspaces(text(step, "name")) {
                Ok(count) => number(count as u64),
                Err(error) => label_failure(&error),
            },
            "regop" => self.registry_op(step),
            "write" => {
                let target = self.home.join(text(step, "path"));
                fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
                fs::write(target, text(step, "text")).expect("write");
                string("ok")
            }
            "parallel" => JsValue::Array(
                step.get("steps")
                    .and_then(JsValue::as_array)
                    .unwrap_or_default()
                    .iter()
                    .map(|inner| {
                        let outcome = self.run(inner);
                        if outcome.get("error").is_some() {
                            outcome
                        } else {
                            string("ok")
                        }
                    })
                    .collect(),
            ),
            "dump" => self.dump(),
            other => panic!("unknown op {other}"),
        }
    }
}

fn run_rust(root: &str, scenarios: &JsValue) -> String {
    let mut lines = String::new();
    for scenario in scenarios.as_array().expect("scenario list") {
        let name = text(scenario, "name");
        let home = Path::new(root).join(name);
        fs::create_dir_all(&home).expect("scenario home");
        let mut state = Scenario {
            home,
            flags: Arc::default(),
            registries: Vec::new(),
            services: HashMap::new(),
            subscriptions: HashMap::new(),
            logs: Arc::default(),
        };
        for (index, step) in scenario
            .get("steps")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let result = state.run(step);
            let _ = writeln!(
                lines,
                "{name}#{index} {}: {}",
                text(step, "op"),
                stringify(&result)
            );
        }
    }
    lines
}

// ---- normalization -------------------------------------------------------

/// Timestamps the script itself writes; every other one is generated.
fn is_fixed_timestamp(stamp: &str) -> bool {
    stamp.starts_with("2026-08-14T") || stamp.starts_with("2099-")
}

fn is_uuid(candidate: &[u8]) -> bool {
    candidate.len() == 36
        && candidate
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
            })
}

fn is_timestamp(candidate: &[u8]) -> bool {
    candidate.len() == 24
        && candidate
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => *byte == b'-',
                10 => *byte == b'T',
                13 | 16 => *byte == b':',
                19 => *byte == b'.',
                23 => *byte == b'Z',
                _ => byte.is_ascii_digit(),
            })
}

fn normalize(output: &str, root: &str) -> String {
    let rooted = output.replace(root, "<root>");
    let bytes = rooted.as_bytes();
    let mut normalized = String::with_capacity(rooted.len());
    let mut index = 0;
    while index < bytes.len() {
        if index + 36 <= bytes.len() && is_uuid(&bytes[index..index + 36]) {
            normalized.push_str("<uuid>");
            index += 36;
        } else if index + 24 <= bytes.len()
            && is_timestamp(&bytes[index..index + 24])
            && !is_fixed_timestamp(&rooted[index..index + 24])
        {
            normalized.push_str("<now>");
            index += 24;
        } else {
            let character = rooted[index..].chars().next().expect("char");
            normalized.push(character);
            index += character.len_utf8();
        }
    }
    normalized
}

#[test]
fn normalization_masks_only_root_generation_and_generated_stamps() {
    let output = "<x> /r/a 58d74552-3c84-4365-b410-4c9306027867 2026-10-03T13:46:14.570Z \
                  2026-08-14T00:00:00.000Z 2099-01-01T00:00:00.000Z 58D74552-3c84-4365-b410-4c9306027867";
    assert_eq!(
        normalize(output, "/r/a"),
        "<x> <root> <uuid> <now> 2026-08-14T00:00:00.000Z 2099-01-01T00:00:00.000Z \
         58D74552-3c84-4365-b410-4c9306027867"
    );
}

#[test]
fn label_scenarios_match_the_pinned_build() {
    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let scenarios = parse(SCENARIOS).expect("scenario script");
    let (_scratch, scratch) = disposable_root("script");
    let script_path = Path::new(&scratch).join("labels_scenarios.json");
    fs::write(&script_path, SCENARIOS).expect("write script");

    let (_node_root, node_root) = disposable_root("node");
    let (_rust_root, rust_root) = disposable_root("rust");
    let node_raw = run_node(&node, &dist, &node_root, &script_path);
    let rust_raw = run_rust(&rust_root, &scenarios);
    let node_normalized = normalize(&node_raw, &node_root);
    let rust_normalized = normalize(&rust_raw, &rust_root);

    if let Some(evidence) = std::env::var_os("SPOCKY_LABELS_EVIDENCE") {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence).expect("evidence directory");
        for (name, content) in [
            ("labels-node-raw.txt", &node_raw),
            ("labels-rust-raw.txt", &rust_raw),
            ("labels-node-normalized.txt", &node_normalized),
            ("labels-rust-normalized.txt", &rust_normalized),
        ] {
            fs::write(evidence.join(name), content).expect("write evidence");
        }
    }

    let node_lines: Vec<&str> = node_normalized.lines().collect();
    let rust_lines: Vec<&str> = rust_normalized.lines().collect();
    for (index, (node_line, rust_line)) in node_lines.iter().zip(&rust_lines).enumerate() {
        assert_eq!(node_line, rust_line, "trace line {index} differs");
    }
    assert_eq!(node_lines.len(), rust_lines.len(), "trace length differs");
    assert!(node_lines.len() > 300, "the script ran");
}
