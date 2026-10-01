//! In-process replacement for the retained Node child: the same operations
//! the Rust adapter in `spocky-hub-pilot` sends over IPC (`migrate`,
//! `query`, `execute`, `transaction`, `close`), run on a dedicated thread
//! that owns the Wasmtime store.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::package::{PACKAGE_NAME, PACKAGE_VERSION, PinnedPackage, hex};
use crate::pglite::{Compiled, EngineOptions, Pglite, QueryResult, RemoteError};
use crate::values::{IpcValue, js_trim};

/// Native stack of the store thread. `invoke_*` re-entry nests host and
/// Wasm frames, so the thread gets far more than the Wasm stack limit.
pub const HOST_THREAD_STACK: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PgliteHostConfig {
    pub package_root: PathBuf,
    pub migrations_root: PathBuf,
    pub data_directory: PathBuf,
    pub engine: EngineOptions,
    /// Bound on Wasm execution per request. When it passes the host stops
    /// the store without closing it, as the adapter kills a timed-out child.
    pub request_timeout: Option<std::time::Duration>,
}

/// Runs `work` on a new thread with the store stack size. The Wasmtime
/// store of a database must live on such a thread: `invoke_*` re-entry nests
/// host and Wasm frames on the native stack.
///
/// # Panics
///
/// Panics when the thread cannot be spawned or `work` panics.
pub fn run_on_store_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    thread::Builder::new()
        .name("pglite-host".into())
        .stack_size(HOST_THREAD_STACK)
        .spawn(work)
        .expect("spawn PGlite store thread")
        .join()
        .expect("PGlite store thread")
}

/// Identity of the host runtime, the counterpart of the Node identity the
/// retained host reports.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostIdentity {
    pub runtime: String,
    pub runtime_version: String,
    pub os: String,
    pub arch: String,
    pub package: String,
    pub package_version: String,
    pub package_dependencies: serde_json::Value,
    pub module_sha256: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlStatement {
    pub sql: String,
    pub params: Vec<IpcValue>,
}

impl SqlStatement {
    #[must_use]
    pub fn new(sql: impl Into<String>, params: Vec<IpcValue>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationOutcome {
    pub applied: usize,
    pub journal_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgliteHostError {
    /// The payload the retained host sends for a failed request.
    Remote {
        code: String,
        message: String,
        details: serde_json::Value,
    },
    /// The host could not start (package, compile, initdb or server start).
    Startup(String),
    /// The host is closed or its thread ended.
    Closed,
    /// The request exceeded `request_timeout`; the store was stopped and a
    /// write may have committed.
    Timeout,
}

impl fmt::Display for PgliteHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Remote { code, message, .. } => {
                write!(formatter, "PGlite host {code}: {message}")
            }
            Self::Startup(message) => write!(formatter, "PGlite host failed to start: {message}"),
            Self::Closed => formatter.write_str("PGlite host is closed"),
            Self::Timeout => formatter.write_str("PGlite host request timed out"),
        }
    }
}

impl std::error::Error for PgliteHostError {}

impl From<RemoteError> for PgliteHostError {
    fn from(error: RemoteError) -> Self {
        Self::Remote {
            code: error.code,
            message: error.message,
            details: error.details,
        }
    }
}

type Reply<T> = mpsc::Sender<Result<T, PgliteHostError>>;

enum Request {
    Migrate(Reply<MigrationOutcome>),
    Query(String, Vec<IpcValue>, Reply<QueryResult>),
    Execute(String, Reply<()>),
    Transaction(Vec<SqlStatement>, Reply<Vec<QueryResult>>),
    Close(Reply<()>),
}

pub struct PgliteHost {
    identity: HostIdentity,
    requests: Mutex<Option<mpsc::Sender<Request>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Package root plus every engine option, so hosts that ask for different
/// engine settings never share a compiled engine.
type CompiledKey = (PathBuf, usize, Option<PathBuf>);

fn compiled_key(package_root: &Path, options: &EngineOptions) -> CompiledKey {
    (
        package_root.to_path_buf(),
        options.max_wasm_stack,
        options.cache_directory.clone(),
    )
}

fn compiled_for(
    package_root: &Path,
    options: &EngineOptions,
) -> Result<Arc<Compiled>, PgliteHostError> {
    static CACHE: OnceLock<Mutex<HashMap<CompiledKey, Arc<Compiled>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .map_err(|_| PgliteHostError::Startup("compile cache poisoned".into()))?;
    let key = compiled_key(package_root, options);
    if let Some(compiled) = cache.get(&key) {
        return Ok(Arc::clone(compiled));
    }
    let package = PinnedPackage::load(package_root)
        .map_err(|error| PgliteHostError::Startup(error.to_string()))?;
    let compiled = Arc::new(
        Compiled::new(Arc::new(package), options)
            .map_err(|error| PgliteHostError::Startup(format!("{error:?}")))?,
    );
    cache.insert(key, Arc::clone(&compiled));
    Ok(compiled)
}

impl PgliteHost {
    /// Starts the store thread, opens the database and waits until it is
    /// ready, like `new PGlite(dataDir)` and `waitReady`.
    ///
    /// # Errors
    ///
    /// Fails when the package, compilation, initdb or server start fails.
    pub fn open(config: &PgliteHostConfig) -> Result<Self, PgliteHostError> {
        let compiled = compiled_for(&config.package_root, &config.engine)?;
        let identity = HostIdentity {
            runtime: "wasmtime".into(),
            runtime_version: "47.0.4".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            package: PACKAGE_NAME.into(),
            package_version: PACKAGE_VERSION.into(),
            package_dependencies: compiled
                .package
                .package_json
                .get("dependencies")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            module_sha256: vec![
                (
                    "dist/pglite.wasm".into(),
                    hex(&Sha256::digest(&compiled.package.pglite_wasm)),
                ),
                (
                    "dist/initdb.wasm".into(),
                    hex(&Sha256::digest(&compiled.package.initdb_wasm)),
                ),
                (
                    "dist/pglite.data".into(),
                    hex(&Sha256::digest(&compiled.package.data)),
                ),
            ],
        };
        let (requests, receiver) = mpsc::channel::<Request>();
        let (ready, started) = mpsc::channel::<Result<(), PgliteHostError>>();
        let data_directory = config.data_directory.clone();
        let migrations_root = config.migrations_root.clone();
        let timeout = config.request_timeout;
        let worker = thread::Builder::new()
            .name("pglite-host".into())
            .stack_size(HOST_THREAD_STACK)
            .spawn(move || {
                let mut database = match Pglite::open(&compiled, &data_directory) {
                    Ok(database) => {
                        let _ = ready.send(Ok(()));
                        database
                    }
                    Err(error) => {
                        let _ = ready.send(Err(PgliteHostError::Startup(error.to_string())));
                        return;
                    }
                };
                serve(&mut database, &receiver, &migrations_root, timeout);
            })
            .map_err(|error| PgliteHostError::Startup(error.to_string()))?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                identity,
                requests: Mutex::new(Some(requests)),
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(PgliteHostError::Startup(
                    "host thread ended during startup".into(),
                ))
            }
        }
    }

    #[must_use]
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    fn call<T>(&self, build: impl FnOnce(Reply<T>) -> Request) -> Result<T, PgliteHostError> {
        let (reply, response) = mpsc::channel();
        {
            let requests = self.requests.lock().map_err(|_| PgliteHostError::Closed)?;
            let sender = requests.as_ref().ok_or(PgliteHostError::Closed)?;
            sender
                .send(build(reply))
                .map_err(|_| PgliteHostError::Closed)?;
        }
        response.recv().map_err(|_| PgliteHostError::Closed)?
    }

    /// The retained host's `migrate`.
    ///
    /// # Errors
    ///
    /// Returns the failure payload; a failed batch rolls back as a whole.
    pub fn migrate(&self) -> Result<MigrationOutcome, PgliteHostError> {
        self.call(Request::Migrate)
    }

    /// # Errors
    ///
    /// Returns the failure payload of the query.
    pub fn query(&self, sql: &str, params: &[IpcValue]) -> Result<QueryResult, PgliteHostError> {
        self.call(|reply| Request::Query(sql.to_owned(), params.to_vec(), reply))
    }

    /// # Errors
    ///
    /// Returns the failure payload of the statement.
    pub fn execute(&self, sql: &str) -> Result<(), PgliteHostError> {
        self.call(|reply| Request::Execute(sql.to_owned(), reply))
    }

    /// # Errors
    ///
    /// Returns the first failure, after `ROLLBACK`.
    pub fn transaction(
        &self,
        statements: &[SqlStatement],
    ) -> Result<Vec<QueryResult>, PgliteHostError> {
        self.call(|reply| Request::Transaction(statements.to_vec(), reply))
    }

    /// Closes the database and stops the store thread.
    ///
    /// # Errors
    ///
    /// Returns the close failure; the host is closed either way.
    pub fn close(&self) -> Result<(), PgliteHostError> {
        let result = self.call(Request::Close);
        if let Ok(mut requests) = self.requests.lock() {
            requests.take();
        }
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
        result
    }
}

impl Drop for PgliteHost {
    fn drop(&mut self) {
        let open = self
            .requests
            .lock()
            .map(|requests| requests.is_some())
            .unwrap_or(false);
        if open {
            let _ = self.close();
        }
    }
}

/// Replies with `result`, or with `Timeout` when the epoch deadline
/// interrupted the request. Returns false when the store must stop.
fn answer<T>(database: &Pglite, reply: &Reply<T>, result: Result<T, PgliteHostError>) -> bool {
    if database.interrupted {
        let _ = reply.send(Err(PgliteHostError::Timeout));
        return false;
    }
    let _ = reply.send(result);
    true
}

fn serve(
    database: &mut Pglite,
    receiver: &mpsc::Receiver<Request>,
    migrations_root: &std::path::Path,
    timeout: Option<std::time::Duration>,
) {
    for request in receiver {
        database.set_deadline(timeout);
        let keep = match request {
            Request::Migrate(reply) => {
                let result = migrate(database, migrations_root);
                answer(database, &reply, result)
            }
            Request::Query(sql, params, reply) => {
                let result = database.query(&sql, &params).map_err(Into::into);
                answer(database, &reply, result)
            }
            Request::Execute(sql, reply) => {
                let result = database.execute(&sql).map_err(Into::into);
                answer(database, &reply, result)
            }
            Request::Transaction(statements, reply) => {
                let statements: Vec<(String, Vec<IpcValue>)> = statements
                    .into_iter()
                    .map(|statement| (statement.sql, statement.params))
                    .collect();
                let result = database.transaction(&statements).map_err(Into::into);
                answer(database, &reply, result)
            }
            Request::Close(reply) => {
                let result = database
                    .close()
                    .map_err(|error| PgliteHostError::from(RemoteError::from_host(&error)));
                let _ = reply.send(result);
                return;
            }
        };
        if !keep {
            // Like killing a timed-out child: no close, the data directory
            // keeps whatever the interrupted request wrote.
            return;
        }
    }
    let _ = database.close();
}

#[derive(Deserialize)]
struct Journal {
    entries: Vec<JournalEntry>,
}

#[derive(Deserialize)]
struct JournalEntry {
    when: f64,
    tag: String,
}

fn read_text(path: &std::path::Path) -> Result<String, PgliteHostError> {
    // readFile(path, "utf8") replaces invalid sequences.
    std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| {
            PgliteHostError::from(RemoteError {
                code: error_code(&error),
                message: format!("{error}"),
                details: serde_json::json!({"name": "Error", "severity": null, "detail": null, "hint": null, "position": null, "schema": null, "table": null, "column": null, "constraint": null}),
            })
        })
}

fn error_code(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".into(),
        std::io::ErrorKind::PermissionDenied => "EACCES".into(),
        _ => "REMOTE_ERROR".into(),
    }
}

/// The retained host's `migrate()` over the Drizzle journal.
fn migrate(
    database: &mut Pglite,
    root: &std::path::Path,
) -> Result<MigrationOutcome, PgliteHostError> {
    let journal_text = read_text(&root.join("meta/_journal.json"))?;
    let journal: Journal = serde_json::from_str(&journal_text).map_err(|error| {
        PgliteHostError::from(RemoteError {
            code: "REMOTE_ERROR".into(),
            message: error.to_string(),
            details: serde_json::json!({"name": "SyntaxError", "severity": null, "detail": null, "hint": null, "position": null, "schema": null, "table": null, "column": null, "constraint": null}),
        })
    })?;
    database.execute(
        "\n    create schema if not exists drizzle;\n    create table if not exists drizzle.__drizzle_migrations (\n      id serial primary key,\n      hash text not null,\n      created_at bigint\n    );\n  ",
    )?;
    let applied = database.query(
        "select created_at from drizzle.__drizzle_migrations order by created_at desc limit 1",
        &[],
    )?;
    // Number(applied.rows[0]?.created_at ?? 0)
    let last_created_at = applied
        .rows
        .first()
        .and_then(|row| row.first())
        .map_or(0.0, |value| match value {
            IpcValue::Numeric(text) | IpcValue::String(text) => crate::values::js_to_number(text),
            IpcValue::Null => 0.0,
            _ => f64::NAN,
        });
    let pending: Vec<&JournalEntry> = journal
        .entries
        .iter()
        .filter(|entry| entry.when > last_created_at)
        .collect();
    let batch = (|| -> Result<(), PgliteHostError> {
        database.execute("BEGIN")?;
        for entry in &pending {
            let migration = read_text(&root.join(format!("{}.sql", entry.tag)))?;
            for statement in migration.split("--> statement-breakpoint") {
                if !js_trim(statement).is_empty() {
                    database.execute(statement)?;
                }
            }
            let hash = hex(&Sha256::digest(migration.as_bytes()));
            database.query(
                "insert into drizzle.__drizzle_migrations (hash, created_at) values ($1, $2)",
                &[
                    IpcValue::String(hash),
                    IpcValue::String(crate::values::js_number_to_string(entry.when)),
                ],
            )?;
        }
        Ok(())
    })();
    match batch {
        Ok(()) => database.execute("COMMIT")?,
        Err(error) => {
            database.execute("ROLLBACK")?;
            return Err(error);
        }
    }
    let count = database.query(
        "select count(*)::bigint as count from drizzle.__drizzle_migrations",
        &[],
    )?;
    let journal_rows = count
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            IpcValue::Numeric(text) => text.parse().ok(),
            _ => None,
        })
        .unwrap_or(0);
    Ok(MigrationOutcome {
        applied: pending.len(),
        journal_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_engines_are_keyed_by_engine_options() {
        let root = Path::new("/package");
        let defaults = EngineOptions::default();
        let cached = EngineOptions {
            cache_directory: Some(PathBuf::from("/cache")),
            ..EngineOptions::default()
        };
        let smaller_stack = EngineOptions {
            max_wasm_stack: 1024 * 1024,
            ..EngineOptions::default()
        };
        assert_eq!(
            compiled_key(root, &defaults),
            compiled_key(root, &EngineOptions::default())
        );
        assert_ne!(compiled_key(root, &defaults), compiled_key(root, &cached));
        assert_ne!(
            compiled_key(root, &defaults),
            compiled_key(root, &smaller_stack)
        );
    }

    #[test]
    fn store_work_runs_on_the_library_thread() {
        let name = run_on_store_thread(|| thread::current().name().map(str::to_owned));
        assert_eq!(name.as_deref(), Some("pglite-host"));
    }
}
