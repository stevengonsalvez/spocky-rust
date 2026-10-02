//! `CreationService` from pinned Paseo `server/creation/index.ts`: owns
//! workspace and agent creation across socket lifetimes, committing each
//! milestone to `<paseoHome>/creations/<sha256>.json` and claiming resource
//! ids with `<sha256>.claim` files.
//!
//! Snapshots and records stay JavaScript objects, so their key order follows
//! the baseline exactly: `initialRecord` and each `publish` spread keep
//! existing keys in place and append new ones (an `undefined` update keeps a
//! slot that `JSON.stringify` omits), while a record read back from disk
//! takes `RecordSchema.parse` order ([`spocky_contracts::zod::output`]).
//!
//! The admission chain, the runner and the per-receipt queue run as tokio
//! tasks, as the baseline's promises run without anyone awaiting them:
//! [`CreationService::create`] joins the admission chain when called and
//! must be called inside a tokio runtime.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};
use spocky_contracts::creation_schema::CREATION_SNAPSHOT;
use spocky_contracts::js::{js_string, spread_into, truthy};
use spocky_contracts::request::CreationKind;
use spocky_contracts::zod::output::{Catchall, Shape, parse_output};
use spocky_store::atomic::{FsError, mkdirp, write_file_atomic};
use spocky_store::collate::locale_compare;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify, stringify_pretty};
use tokio::sync::{oneshot, watch};

use crate::clock::{generate_workspace_id, random_uuid};

/// A thrown JavaScript error as the baseline reads it: `error.message` and a
/// string `error.code`, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreationError {
    pub message: String,
    pub code: Option<String>,
}

impl CreationError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
}

impl From<FsError> for CreationError {
    fn from(error: FsError) -> Self {
        Self {
            message: error.to_string(),
            code: Some(error.code()),
        }
    }
}

impl std::fmt::Display for CreationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CreationError {}

/// A callback's result.
pub type CreationFuture<T> = Pin<Box<dyn Future<Output = Result<T, CreationError>> + Send>>;

/// `exists(kind, id)`.
pub type Exists = Arc<dyn Fn(CreationKind, String) -> CreationFuture<bool> + Send + Sync>;

/// `provision(workspaceId)`; the id is the snapshot's (`workspaceId!`).
pub type Provision = Box<dyn FnOnce(Option<String>) -> CreationFuture<Provisioned> + Send>;

/// `onReady(agent)` handed to [`CreateAgent`].
pub type OnReady = Box<dyn FnOnce(JsValue) -> CreationFuture<()> + Send>;

/// `createAgent(agentId, workspace, onReady)`; the id is the snapshot's
/// (`agentId!`).
pub type CreateAgent =
    Box<dyn FnOnce(Option<String>, Option<JsValue>, OnReady) -> CreationFuture<JsValue> + Send>;

/// `readAgent(id)`.
pub type ReadAgent = Box<dyn FnOnce(String) -> CreationFuture<Option<JsValue>> + Send>;

/// `validateCompleted(snapshot)`.
pub type ValidateCompleted = Arc<dyn Fn(JsValue) -> CreationFuture<()> + Send + Sync>;

/// A creation observer, called with each snapshot.
pub type Observer = Arc<dyn Fn(&JsValue) + Send + Sync>;

/// `provision`'s result.
#[derive(Debug, Clone)]
pub struct Provisioned {
    /// The `WorkspaceDescriptorPayload`.
    pub workspace: JsValue,
    pub setup_skipped_reason: Option<String>,
}

/// `CreationInput`'s kind and its kind-specific member.
pub enum CreationTarget {
    Workspace,
    Agent { read_agent: ReadAgent },
}

impl CreationTarget {
    const fn kind(&self) -> CreationKind {
        match self {
            Self::Workspace => CreationKind::Workspace,
            Self::Agent { .. } => CreationKind::Agent,
        }
    }
}

/// `CreationInput`.
pub struct CreationInput {
    pub target: CreationTarget,
    pub key: String,
    /// The request whose digest is the receipt's fingerprint.
    pub request: JsValue,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub has_agent: bool,
    pub has_prompt: bool,
    pub exists: Exists,
    pub provision: Option<Provision>,
    pub create_agent: Option<CreateAgent>,
}

const fn kind_name(kind: CreationKind) -> &'static str {
    match kind {
        CreationKind::Workspace => "workspace",
        CreationKind::Agent => "agent",
    }
}

fn kind_from_name(name: &str) -> CreationKind {
    if name == "workspace" {
        CreationKind::Workspace
    } else {
        CreationKind::Agent
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn string(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// `value ?? fallback` on an optional property.
fn nullish(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
}

fn nullable(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::Null, string)
}

/// `RecordSchema`.
static RECORD: std::sync::LazyLock<Shape> = std::sync::LazyLock::new(|| {
    Shape::Object(
        vec![
            ("fingerprint", Shape::String),
            ("snapshot", Shape::Lazy(|| &CREATION_SNAPSHOT)),
            (
                "inFlight",
                Shape::Nullable(Box::new(Shape::Enum(&["workspace", "agent", "prompt"]))),
            ),
        ],
        Catchall::Strip,
    )
});

/// The legacy agent receipt schema of `readLegacyAgent`.
static LEGACY_RECEIPT: std::sync::LazyLock<Shape> = std::sync::LazyLock::new(|| {
    Shape::Object(
        vec![
            ("fingerprint", Shape::String),
            ("state", Shape::Enum(&["pending", "completed"])),
            ("agentId", Shape::String),
        ],
        Catchall::Strip,
    )
});

/// `digest(input)`: SHA-256 hex of `JSON.stringify` with every object's keys
/// sorted by `localeCompare` (`Object.fromEntries` then puts array-index
/// keys first, as [`JsObject`] does).
#[must_use]
pub fn digest(input: &JsValue) -> String {
    fn sorted(value: &JsValue) -> JsValue {
        match value {
            JsValue::Object(object) => {
                let mut entries: Vec<(&str, &JsValue)> = object.iter().collect();
                entries.sort_by(|(left, _), (right, _)| locale_compare(left, right));
                let mut out = JsObject::new();
                for (key, item) in entries {
                    out.insert(key, sorted(item));
                }
                JsValue::Object(out)
            }
            JsValue::Array(items) => JsValue::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let text = stringify(&sorted(input));
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
            hex
        })
}

/// `identityFor(kind, key)`.
fn identity_for(kind: CreationKind, key: &str) -> String {
    digest(&JsValue::Array(vec![string(kind_name(kind)), string(key)]))
}

/// `readOptional(file)`: the file's UTF-8 text, or `None` when missing.
fn read_optional(file: &Path) -> Result<Option<String>, CreationError> {
    let mut handle = match fs::File::open(file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(FsError {
                syscall: "open",
                path: Some(file.to_string_lossy().into_owned()),
                dest: None,
                source,
            }
            .into());
        }
    };
    let mut bytes = Vec::new();
    handle.read_to_end(&mut bytes).map_err(|source| FsError {
        syscall: "read",
        path: None,
        dest: None,
        source,
    })?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn parse_json(text: &str) -> Result<JsValue, CreationError> {
    parse(text).map_err(|error| CreationError::new(error.message))
}

/// `schema.parse(value)`. A rejection names its path only; the baseline
/// throws the `ZodError` whose message lists zod's issues.
fn zod_parse(shape: &Shape, value: &JsValue) -> Result<JsValue, CreationError> {
    parse_output(shape, value).map_err(|rejected| {
        CreationError::new(format!("Invalid input at [{}]", rejected.0.join(", ")))
    })
}

/// The completion every caller of one run awaits.
type Completion = watch::Receiver<Option<Result<JsValue, CreationError>>>;

async fn completed(mut completion: Completion) -> Result<JsValue, CreationError> {
    match completion.wait_for(Option::is_some).await {
        Ok(result) => result
            .clone()
            .unwrap_or_else(|| unreachable!("waited for Some")),
        Err(_) => Err(CreationError::new("creation runner stopped")),
    }
}

struct Inner {
    directory: PathBuf,
    legacy_directory: Option<PathBuf>,
    validate_completed: ValidateCompleted,
    /// `admission`: settles when the latest admission has.
    admission: Mutex<watch::Receiver<bool>>,
    active: Mutex<HashMap<String, Completion>>,
    receipt_operations: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    observers: Mutex<HashMap<String, Vec<(u64, Observer)>>>,
    next_observer: AtomicU64,
}

/// Unsubscribes its observer when dropped.
pub struct ObserverGuard {
    inner: Arc<Inner>,
    identity: String,
    id: u64,
}

impl Drop for ObserverGuard {
    fn drop(&mut self) {
        let mut observers = lock(&self.inner.observers);
        if let Some(list) = observers.get_mut(&self.identity) {
            list.retain(|(id, _)| *id != self.id);
            if list.is_empty() {
                observers.remove(&self.identity);
            }
        }
    }
}

/// `CreationService`.
#[derive(Clone)]
pub struct CreationService {
    inner: Arc<Inner>,
}

impl CreationService {
    /// `new CreationService(join(paseoHome, "creations"), logger,
    /// validateCompleted, join(paseoHome, "agent-requests"))`, as
    /// `websocket-server.ts` constructs it.
    #[must_use]
    pub fn new(paseo_home: &Path, validate_completed: Option<ValidateCompleted>) -> Self {
        Self::with_directories(
            paseo_home.join("creations"),
            Some(paseo_home.join("agent-requests")),
            validate_completed,
        )
    }

    /// The constructor with its directories given.
    #[must_use]
    pub fn with_directories(
        directory: PathBuf,
        legacy_directory: Option<PathBuf>,
        validate_completed: Option<ValidateCompleted>,
    ) -> Self {
        let (settled, admission) = watch::channel(true);
        drop(settled);
        Self {
            inner: Arc::new(Inner {
                directory,
                legacy_directory,
                validate_completed: validate_completed
                    .unwrap_or_else(|| Arc::new(|_| Box::pin(async { Ok(()) }))),
                admission: Mutex::new(admission),
                active: Mutex::new(HashMap::new()),
                receipt_operations: Mutex::new(HashMap::new()),
                observers: Mutex::new(HashMap::new()),
                next_observer: AtomicU64::new(0),
            }),
        }
    }

    /// `create(input, observer)`: joins the admission chain now; the result
    /// is the creation's final snapshot.
    ///
    /// # Errors
    ///
    /// Returns what the baseline's `create` rejects with: a key or id
    /// conflict, a failed receipt read or write, a callback's error during
    /// admission, or a failure to record the run's own failure.
    pub fn create(
        &self,
        input: CreationInput,
        observer: Option<Observer>,
    ) -> impl Future<Output = Result<JsValue, CreationError>> + Send + 'static + use<> {
        let inner = Arc::clone(&self.inner);
        let identity = identity_for(input.target.kind(), &input.key);
        let (settled, next) = watch::channel(false);
        let previous = std::mem::replace(&mut *lock(&inner.admission), next);
        // Install before admission starts, so even a fast local provision
        // cannot outrun observation.
        let guard = observer
            .clone()
            .map(|observer| observe(&inner, &identity, observer));
        let (admitted_tx, admitted) = oneshot::channel();
        let admission_inner = Arc::clone(&inner);
        let admission_identity = identity.clone();
        let initiating = guard.as_ref().map(|guard| guard.id);
        tokio::spawn(async move {
            settle(previous).await;
            let result = admit(
                &admission_inner,
                &admission_identity,
                input,
                observer,
                initiating,
            )
            .await;
            let _ = admitted_tx.send(result);
            let _ = settled.send(true);
        });
        async move {
            let _guard = guard;
            let completion = admitted
                .await
                .unwrap_or_else(|_| Err(CreationError::new("creation admission stopped")))?;
            completed(completion).await
        }
    }

    /// `subscribe(kind, key, observer)`: the stored snapshot, if any, and the
    /// subscription.
    ///
    /// # Errors
    ///
    /// Returns a failed receipt read or write, or `validateCompleted`'s error.
    pub async fn subscribe(
        &self,
        kind: CreationKind,
        key: &str,
        observer: Observer,
    ) -> Result<(Option<JsValue>, ObserverGuard), CreationError> {
        let inner = &self.inner;
        let identity = identity_for(kind, key);
        let guard = observe(inner, &identity, observer);
        let admission = lock(&inner.admission).clone();
        settle(admission).await;
        let Some(mut record) = read(inner, &identity)? else {
            return Ok((None, guard));
        };
        let in_flight = record
            .get("inFlight")
            .and_then(JsValue::as_str)
            .map(str::to_owned);
        let running = lock(&inner.active).contains_key(&identity);
        if let Some(stage) = in_flight
            && !running
        {
            publish(
                inner,
                &identity,
                &mut record,
                vec![
                    ("phase", string("failed")),
                    ("failedStage", string(&stage)),
                    ("outcomeUnknown", JsValue::Bool(true)),
                    (
                        "error",
                        string(&format!("{}_request_outcome_unknown", kind_name(kind))),
                    ),
                ],
            )?;
        }
        let snapshot = snapshot_of(&record);
        if phase(&snapshot) == Some("completed") {
            (inner.validate_completed)(snapshot.clone()).await?;
        }
        Ok((Some(snapshot), guard))
    }
}

/// Waits for an admission to settle; a dropped sender has settled too.
async fn settle(mut admission: watch::Receiver<bool>) {
    let _ = admission.wait_for(|settled| *settled).await;
}

fn observe(inner: &Arc<Inner>, identity: &str, observer: Observer) -> ObserverGuard {
    let id = inner.next_observer.fetch_add(1, Ordering::Relaxed);
    lock(&inner.observers)
        .entry(identity.to_owned())
        .or_default()
        .push((id, observer));
    ObserverGuard {
        inner: Arc::clone(inner),
        identity: identity.to_owned(),
        id,
    }
}

fn snapshot_of(record: &JsObject) -> JsValue {
    record
        .get("snapshot")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

/// The record's snapshot object, to spread updates into.
fn snapshot_object(record: &JsObject) -> JsObject {
    match record.get("snapshot") {
        Some(JsValue::Object(snapshot)) => snapshot.clone(),
        _ => unreachable!("records hold an object snapshot"),
    }
}

fn phase(snapshot: &JsValue) -> Option<&str> {
    snapshot.get("phase").and_then(JsValue::as_str)
}

fn snapshot_text(snapshot: &JsValue, key: &str) -> Option<String> {
    snapshot
        .get(key)
        .and_then(JsValue::as_str)
        .map(str::to_owned)
}

/// The admission body: the completion to await, after notifying the
/// initiating observer of the admitted snapshot.
#[allow(clippy::too_many_lines)]
async fn admit(
    inner: &Arc<Inner>,
    identity: &str,
    mut input: CreationInput,
    observer: Option<Observer>,
    initiating: Option<u64>,
) -> Result<Completion, CreationError> {
    let kind = input.target.kind();
    mkdirp(&inner.directory.to_string_lossy())?;
    let fingerprint = digest(&input.request);
    let mut record = match read(inner, identity)? {
        Some(record) => Some(record),
        None => read_legacy_agent(inner, &mut input, &fingerprint).await?,
    };
    if let Some(stored) = &record
        && stored.get("fingerprint").and_then(JsValue::as_str) != Some(fingerprint.as_str())
    {
        return Err(CreationError::new(format!(
            "{}_request_key_conflict",
            kind_name(kind)
        )));
    }
    let notify_initiating = |snapshot: &JsValue| {
        let subscribed = initiating.is_some_and(|id| {
            lock(&inner.observers)
                .get(identity)
                .is_some_and(|list| list.iter().any(|(existing, _)| *existing == id))
        });
        if subscribed && let Some(observer) = &observer {
            observer(snapshot);
        }
    };
    let running = lock(&inner.active).get(identity).cloned();
    if let (Some(running), Some(stored)) = (running, &record) {
        notify_initiating(&snapshot_of(stored));
        return Ok(running);
    }
    if let Some(stored) = &record {
        let snapshot = snapshot_of(stored);
        if phase(&snapshot) == Some("completed") {
            (inner.validate_completed)(snapshot.clone()).await?;
        }
        if phase(&snapshot) == Some("completed") || truthy(snapshot.get("outcomeUnknown")) {
            notify_initiating(&snapshot);
            return Ok(settled_completion(Ok(snapshot)));
        }
    }
    if let Some(stored) = &mut record
        && let Some(stage) = stored
            .get("inFlight")
            .and_then(JsValue::as_str)
            .map(str::to_owned)
    {
        publish(
            inner,
            identity,
            stored,
            vec![
                ("phase", string("failed")),
                (
                    "error",
                    string(&format!("{}_request_outcome_unknown", kind_name(kind))),
                ),
                ("failedStage", string(&stage)),
                ("outcomeUnknown", JsValue::Bool(true)),
            ],
        )?;
        let snapshot = snapshot_of(stored);
        notify_initiating(&snapshot);
        return Ok(settled_completion(Ok(snapshot)));
    }
    let mut record = if let Some(record) = record {
        record
    } else {
        let record = initial_record(&input, input.agent_id.clone(), &fingerprint);
        // Persist identity before claiming resources. A failed admission
        // can only retry these IDs.
        write(inner, identity, &record)?;
        record
    };
    claim_resources(
        inner,
        identity,
        &snapshot_of(&record),
        input.target.kind(),
        &input.exists,
    )
    .await?;
    let snapshot = snapshot_of(&record);
    if phase(&snapshot) == Some("failed") {
        let resumed = if truthy(snapshot.get("workspace")) {
            "workspace_ready"
        } else {
            "accepted"
        };
        publish(
            inner,
            identity,
            &mut record,
            vec![
                ("phase", string(resumed)),
                ("error", JsValue::Null),
                ("errorCode", JsValue::Undefined),
                ("failedStage", JsValue::Undefined),
                ("outcomeUnknown", JsValue::Undefined),
            ],
        )?;
    }
    let snapshot = snapshot_of(&record);
    let (finished, completion) = watch::channel(None);
    lock(&inner.active).insert(identity.to_owned(), completion.clone());
    notify_initiating(&snapshot);
    let runner = Arc::clone(inner);
    let runner_identity = identity.to_owned();
    tokio::spawn(async move {
        let result = run(&runner, &runner_identity, record, input).await;
        lock(&runner.active).remove(&runner_identity);
        let _ = finished.send(Some(result));
    });
    Ok(completion)
}

fn settled_completion(result: Result<JsValue, CreationError>) -> Completion {
    let (finished, completion) = watch::channel(Some(result));
    drop(finished);
    completion
}

/// `initialRecord(input, fingerprint)`, with `agentId` as given.
fn initial_record(input: &CreationInput, agent_id: Option<String>, fingerprint: &str) -> JsObject {
    let kind = input.target.kind();
    let workspace_id = input
        .workspace_id
        .clone()
        .or_else(|| (kind == CreationKind::Workspace).then(generate_workspace_id));
    let agent_id = input
        .has_agent
        .then(|| agent_id.unwrap_or_else(random_uuid));
    let mut snapshot = JsObject::new();
    snapshot.insert("kind", string(kind_name(kind)));
    snapshot.insert("idempotencyKey", string(&input.key));
    snapshot.insert("revision", JsValue::Number(0.0));
    snapshot.insert("phase", string("accepted"));
    snapshot.insert("error", JsValue::Null);
    snapshot.insert("workspaceId", nullable(workspace_id.as_deref()));
    snapshot.insert("agentId", nullable(agent_id.as_deref()));
    let mut record = JsObject::new();
    record.insert("fingerprint", string(fingerprint));
    record.insert("inFlight", JsValue::Null);
    record.insert("snapshot", JsValue::Object(snapshot));
    record
}

/// `readLegacyAgent(input, fingerprint)`.
// COMPAT(agentRequestReceipts): added in v0.8.0, remove after 2027-03-11 once
// old creation receipts can expire.
async fn read_legacy_agent(
    inner: &Arc<Inner>,
    input: &mut CreationInput,
    fingerprint: &str,
) -> Result<Option<JsObject>, CreationError> {
    let Some(legacy_directory) = &inner.legacy_directory else {
        return Ok(None);
    };
    if !matches!(input.target, CreationTarget::Agent { .. }) {
        return Ok(None);
    }
    let file = legacy_directory.join(format!(
        "{}.json",
        digest(&JsValue::Array(vec![string("create"), string(&input.key)]))
    ));
    let Some(text) = read_optional(&file)? else {
        return Ok(None);
    };
    let receipt = zod_parse(&LEGACY_RECEIPT, &parse_json(&text)?)?;
    let mut legacy_request = JsObject::new();
    legacy_request.insert("type", string("create_agent_request"));
    spread_into(&mut legacy_request, Some(&input.request));
    if receipt.get("fingerprint").and_then(JsValue::as_str)
        != Some(digest(&JsValue::Object(legacy_request)).as_str())
    {
        return Err(CreationError::new("agent_request_key_conflict"));
    }
    let agent_id = snapshot_text(&receipt, "agentId").unwrap_or_default();
    let target = std::mem::replace(&mut input.target, CreationTarget::Workspace);
    let CreationTarget::Agent { read_agent } = target else {
        unreachable!("checked above")
    };
    // The baseline keeps `readAgent` on the input; it is never called again.
    input.target = CreationTarget::Agent {
        read_agent: Box::new(|_| Box::pin(async { Ok(None) })),
    };
    let agent = read_agent(agent_id.clone()).await?;
    if agent.is_none() && receipt.get("state").and_then(JsValue::as_str) == Some("completed") {
        return Err(CreationError::new(
            "Previously created agent no longer exists",
        ));
    }
    let mut record = initial_record(input, Some(agent_id), fingerprint);
    let mut snapshot = snapshot_object(&record);
    let workspace_id = agent
        .as_ref()
        .and_then(|agent| nullish(agent.get("workspaceId")).cloned())
        .or_else(|| snapshot.get("workspaceId").cloned())
        .unwrap_or(JsValue::Undefined);
    snapshot.insert(
        "phase",
        string(if agent.is_some() {
            "completed"
        } else {
            "failed"
        }),
    );
    snapshot.insert("agent", agent.clone().unwrap_or(JsValue::Undefined));
    snapshot.insert("workspaceId", workspace_id);
    snapshot.insert(
        "error",
        if agent.is_some() {
            JsValue::Null
        } else {
            string("agent_request_outcome_unknown")
        },
    );
    if agent.is_none() {
        snapshot.insert("failedStage", string("agent"));
        snapshot.insert("outcomeUnknown", JsValue::Bool(true));
    }
    record.insert("snapshot", JsValue::Object(snapshot));
    write(
        inner,
        &identity_for(CreationKind::Agent, &input.key),
        &record,
    )?;
    Ok(Some(record))
}

async fn claim_resources(
    inner: &Arc<Inner>,
    identity: &str,
    snapshot: &JsValue,
    kind: CreationKind,
    exists: &Exists,
) -> Result<(), CreationError> {
    if kind == CreationKind::Workspace
        && let Some(id) = snapshot_text(snapshot, "workspaceId").filter(|id| !id.is_empty())
    {
        claim(inner, identity, CreationKind::Workspace, &id, exists).await?;
    }
    if let Some(id) = snapshot_text(snapshot, "agentId").filter(|id| !id.is_empty()) {
        claim(inner, identity, CreationKind::Agent, &id, exists).await?;
    }
    Ok(())
}

/// `claim(identity, kind, id, exists)`: `<digest([kind, id])>.claim` holds
/// the owning identity, created with `wx` and mode `0o600`.
async fn claim(
    inner: &Arc<Inner>,
    identity: &str,
    kind: CreationKind,
    id: &str,
    exists: &Exists,
) -> Result<(), CreationError> {
    let file = inner.directory.join(format!(
        "{}.claim",
        digest(&JsValue::Array(vec![string(kind_name(kind)), string(id)]))
    ));
    let conflict = || CreationError::new(format!("{}_id_conflict", kind_name(kind)));
    if let Some(owner) = read_optional(&file)? {
        return if owner == identity {
            Ok(())
        } else {
            Err(conflict())
        };
    }
    if exists(kind, id.to_owned()).await? {
        return Err(conflict());
    }
    let open_error = |source| FsError {
        syscall: "open",
        path: Some(file.to_string_lossy().into_owned()),
        dest: None,
        source,
    };
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .map_err(open_error)?;
    handle
        .write_all(identity.as_bytes())
        .map_err(|source| FsError {
            syscall: "write",
            path: None,
            dest: None,
            source,
        })?;
    Ok(())
}

/// `accessReceipt(identity, access)`: one receipt read or write at a time
/// per identity.
fn access_receipt<T>(inner: &Inner, identity: &str, access: impl FnOnce() -> T) -> T {
    let operation = Arc::clone(
        lock(&inner.receipt_operations)
            .entry(identity.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    );
    let result = {
        let _turn = lock(&operation);
        access()
    };
    let mut operations = lock(&inner.receipt_operations);
    // The map's reference and this one: nobody else is queued.
    if Arc::strong_count(&operation) == 2 {
        operations.remove(identity);
    }
    result
}

/// `write(identity, record)`: `JSON.stringify(record, null, 2)`, written
/// atomically.
fn write(inner: &Inner, identity: &str, record: &JsObject) -> Result<(), CreationError> {
    let contents = stringify_pretty(&JsValue::Object(record.clone()));
    let file = inner.directory.join(format!("{identity}.json"));
    access_receipt(inner, identity, || write_file_atomic(&file, &contents)).map_err(Into::into)
}

/// `read(identity)`: the stored record in `RecordSchema` order.
fn read(inner: &Inner, identity: &str) -> Result<Option<JsObject>, CreationError> {
    let file = inner.directory.join(format!("{identity}.json"));
    access_receipt(inner, identity, || {
        let Some(text) = read_optional(&file)? else {
            return Ok(None);
        };
        match &zod_parse(&RECORD, &parse_json(&text)?)? {
            JsValue::Object(record) => Ok(Some(record.clone())),
            _ => unreachable!("RecordSchema outputs an object"),
        }
    })
}

/// `publish(identity, record, update)`: spreads the update over the
/// snapshot, bumps `revision`, writes, then notifies observers.
fn publish(
    inner: &Inner,
    identity: &str,
    record: &mut JsObject,
    update: Vec<(&str, JsValue)>,
) -> Result<(), CreationError> {
    let mut snapshot = snapshot_object(record);
    let revision = match snapshot.get("revision") {
        Some(JsValue::Number(revision)) => *revision,
        _ => f64::NAN,
    };
    for (key, value) in update {
        snapshot.insert(key, value);
    }
    snapshot.insert("revision", JsValue::Number(revision + 1.0));
    record.insert("snapshot", JsValue::Object(snapshot));
    write(inner, identity, record)?;
    let snapshot = snapshot_of(record);
    let observers: Vec<Observer> = lock(&inner.observers)
        .get(identity)
        .map(|list| {
            list.iter()
                .map(|(_, observer)| Arc::clone(observer))
                .collect()
        })
        .unwrap_or_default();
    for observer in observers {
        observer(&snapshot);
    }
    Ok(())
}

fn set_in_flight(record: &mut JsObject, stage: Option<&str>) {
    record.insert("inFlight", nullable(stage));
}

/// `run(identity, record, input)`.
async fn run(
    inner: &Arc<Inner>,
    identity: &str,
    record: JsObject,
    input: CreationInput,
) -> Result<JsValue, CreationError> {
    let record = Arc::new(Mutex::new(record));
    let exists = Arc::clone(&input.exists);
    if let Err(error) = run_steps(inner, identity, &record, input).await {
        let mut record = lock(&record).clone();
        let stage = record
            .get("inFlight")
            .and_then(JsValue::as_str)
            .unwrap_or("agent")
            .to_owned();
        let snapshot = snapshot_of(&record);
        let resource_id = if stage == "workspace" {
            snapshot.get("workspaceId")
        } else {
            snapshot.get("agentId")
        }
        .cloned();
        let rejected_before_provision = matches!(
            error.code.as_deref(),
            Some("directory_not_found" | "source_required")
        );
        let unknown = stage == "prompt"
            || (stage == "workspace" && !rejected_before_provision)
            || match resource_id {
                Some(JsValue::Null) => false,
                id => {
                    let id = js_string(id.as_ref());
                    exists(kind_from_name(&stage), id).await?
                }
            };
        // A failed provider startup with no registered resource can retry
        // under the same ID. A partially provisioned resource or an
        // attempted prompt needs reconciliation.
        if !unknown {
            set_in_flight(&mut record, None);
        }
        publish(
            inner,
            identity,
            &mut record,
            vec![
                ("phase", string("failed")),
                ("error", string(&error.message)),
                (
                    "errorCode",
                    error.code.as_deref().map_or(JsValue::Undefined, string),
                ),
                ("failedStage", string(&stage)),
                ("outcomeUnknown", JsValue::Bool(unknown)),
            ],
        )?;
        return Ok(snapshot_of(&record));
    }
    let snapshot = snapshot_of(&lock(&record));
    Ok(snapshot)
}

#[allow(clippy::too_many_lines)]
async fn run_steps(
    inner: &Arc<Inner>,
    identity: &str,
    record: &Arc<Mutex<JsObject>>,
    input: CreationInput,
) -> Result<(), CreationError> {
    let has_workspace = truthy(snapshot_of(&lock(record)).get("workspace"));
    if let Some(provision) = input.provision
        && !has_workspace
    {
        let workspace_id = {
            let mut record = lock(record);
            set_in_flight(&mut record, Some("workspace"));
            write(inner, identity, &record)?;
            snapshot_text(&snapshot_of(&record), "workspaceId")
        };
        let provisioned = provision(workspace_id).await?;
        let mut record = lock(record);
        set_in_flight(&mut record, None);
        let id = provisioned
            .workspace
            .get("id")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        publish(
            inner,
            identity,
            &mut record,
            vec![
                ("phase", string("workspace_ready")),
                ("workspace", provisioned.workspace),
                ("workspaceId", id),
                (
                    "setupSkippedReason",
                    provisioned
                        .setup_skipped_reason
                        .as_deref()
                        .map_or(JsValue::Undefined, string),
                ),
            ],
        )?;
    }
    let has_agent = truthy(snapshot_of(&lock(record)).get("agent"));
    if input.has_agent
        && let Some(create_agent) = input.create_agent
        && !has_agent
    {
        let (agent_id, workspace) = {
            let mut record = lock(record);
            set_in_flight(&mut record, Some("agent"));
            write(inner, identity, &record)?;
            let snapshot = snapshot_of(&record);
            (
                snapshot_text(&snapshot, "agentId"),
                snapshot
                    .get("workspace")
                    .cloned()
                    .filter(|w| !matches!(w, JsValue::Undefined)),
            )
        };
        let has_prompt = input.has_prompt;
        let ready_inner = Arc::clone(inner);
        let ready_identity = identity.to_owned();
        let ready_record = Arc::clone(record);
        let on_ready: OnReady = Box::new(move |ready_agent: JsValue| {
            Box::pin(async move {
                let mut record = lock(&ready_record);
                set_in_flight(&mut record, has_prompt.then_some("prompt"));
                let workspace_id = nullish(ready_agent.get("workspaceId"))
                    .cloned()
                    .or_else(|| snapshot_of(&record).get("workspaceId").cloned())
                    .unwrap_or(JsValue::Undefined);
                publish(
                    &ready_inner,
                    &ready_identity,
                    &mut record,
                    vec![
                        ("phase", string("agent_ready")),
                        ("agent", ready_agent),
                        ("workspaceId", workspace_id),
                    ],
                )
            })
        });
        let agent = create_agent(agent_id, workspace, on_ready).await?;
        let mut record = lock(record);
        set_in_flight(&mut record, None);
        if has_prompt {
            publish(
                inner,
                identity,
                &mut record,
                vec![("phase", string("prompt_started")), ("agent", agent)],
            )?;
        } else {
            let mut snapshot = snapshot_object(&record);
            snapshot.insert("agent", agent);
            record.insert("snapshot", JsValue::Object(snapshot));
        }
    }
    let mut record = lock(record);
    set_in_flight(&mut record, None);
    publish(
        inner,
        identity,
        &mut record,
        vec![("phase", string("completed"))],
    )
}

#[cfg(test)]
mod tests {
    use spocky_store::js_value::parse;

    use super::digest;

    #[test]
    fn digest_sorts_keys_and_puts_index_keys_first() {
        let left =
            digest(&parse(r#"{"b":1,"a":{"z":[{"y":1,"x":2}],"10":0,"2":0}}"#).expect("json"));
        let right =
            digest(&parse(r#"{"a":{"2":0,"10":0,"z":[{"x":2,"y":1}]},"b":1}"#).expect("json"));
        assert_eq!(left, right);
        // sha256 of `["workspace","key-1"]`.
        assert_eq!(
            digest(&parse(r#"["workspace","key-1"]"#).expect("json")).len(),
            64
        );
    }
}
