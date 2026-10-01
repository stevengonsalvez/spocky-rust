//! The `PGlite` class of the pinned package: module setup in `preRun`
//! order, initdb through a second instance, tar handoff to the data
//! directory, server start, the wire loop and close.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use wasmtime::{Caller, Config, Engine, Module, Store, Val};

use crate::package::PinnedPackage;
use crate::protocol::{self, Backend, ErrorFields};
use crate::runtime::{
    self, Callback, ExitStatus, INITDB_LAYOUT, InitdbIo, ModuleSpec, PGLITE_LAYOUT, Runtime,
    abort_error, c_string, call_i32, string_on_stack,
};
use crate::values::{IpcValue, JsError, JsValue, TypeRegistry, encode_value, serialize_param};
use crate::vfs::{self, Device, Fs, MountKind, makedev};

/// An `ErrnoError` raised during setup, labelled with the step.
fn fs_step(step: &str, error: vfs::FsError) -> wasmtime::Error {
    match error {
        vfs::FsError::Errno(code) => abort_error(format!("{step}: ErrnoError {code}")),
        vfs::FsError::Fatal(message) => abort_error(format!("{step}: {message}")),
    }
}

pub const PGDATA: &str = "/pglite/data";
const POSTGRES_MAIN_LONGJMP: i32 = 100;
/// Interval of the epoch ticker that bounds request time.
pub const EPOCH_TICK: std::time::Duration = std::time::Duration::from_millis(10);
/// Epoch deadline used when a request has no time limit.
const NO_DEADLINE: u64 = u64::MAX / 4;
/// Swallowed exception texts kept for diagnostics.
const SWALLOWED_KEPT: usize = 32;

/// Bound on consecutive exceptions the wire loop swallows without reading
/// input. The glue loops forever in that case; the host stops instead.
const SWALLOWED_EXCEPTION_LIMIT: u32 = 10_000;

pub const START_PARAMS: [&str; 20] = [
    "--single",
    "-F",
    "-O",
    "-j",
    "-c",
    "search_path=public",
    "-c",
    "exit_on_error=false",
    "-c",
    "log_checkpoints=false",
    "-c",
    "max_worker_processes=0",
    "-c",
    "max_parallel_workers=0",
    "-c",
    "max_parallel_workers_per_gather=0",
    "-c",
    "io_method=sync",
    "-c",
    "max_parallel_maintenance_workers=0",
];

pub const INITDB_ARGS: [&str; 6] = [
    "--allow-group-access",
    "--encoding",
    "UTF8",
    "--locale=C.UTF-8",
    "--locale-provider=libc",
    "--auth=trust",
];

/// Compiled modules shared by every open in a process.
pub struct Compiled {
    pub engine: Engine,
    pub pglite: Module,
    pub initdb: Module,
    pub package: Arc<PinnedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EngineOptions {
    /// Wasm stack limit; `invoke_*` re-entry also consumes native stack.
    pub max_wasm_stack: usize,
    /// Directory for Wasmtime's compilation cache, or `None` to compile.
    pub cache_directory: Option<PathBuf>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            max_wasm_stack: 64 * 1024 * 1024,
            cache_directory: None,
        }
    }
}

impl Compiled {
    /// # Errors
    ///
    /// Fails when the engine cannot be configured or a module fails to
    /// compile.
    pub fn new(package: Arc<PinnedPackage>, options: &EngineOptions) -> wasmtime::Result<Self> {
        let mut config = Config::new();
        // The engine validates max_wasm_stack against async_stack_size even
        // though this host never runs Wasm asynchronously.
        config.async_stack_size(options.max_wasm_stack + 1024 * 1024);
        config.max_wasm_stack(options.max_wasm_stack);
        if let Some(directory) = &options.cache_directory {
            let mut cache_config = wasmtime::CacheConfig::new();
            cache_config.with_directory(directory);
            config.cache(Some(wasmtime::Cache::new(cache_config)?));
        }
        // Host-side request deadlines: Wasm checks the epoch counter, which a
        // ticker thread advances every EPOCH_TICK.
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        spawn_epoch_ticker(engine.weak()).map_err(|error| abort_error(error.to_string()))?;
        let pglite = Module::new(&engine, &package.pglite_wasm)?;
        let initdb = Module::new(&engine, &package.initdb_wasm)?;
        Ok(Self {
            engine,
            pglite,
            initdb,
            package,
        })
    }
}

/// Node's `navigator.languages[0]` turned into the glue's default `LANG`.
#[must_use]
pub fn default_lang() -> String {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default();
    let base = raw.split(['.', '@']).next().unwrap_or("");
    let tag = if base.is_empty() || base == "C" || base == "POSIX" {
        "en-US".to_owned()
    } else {
        base.replace('_', "-")
    };
    format!("{}.UTF-8", tag.replacen('-', "_", 1))
}

#[derive(Debug)]
pub enum HostError {
    Engine(wasmtime::Error),
    Database(Box<ErrorFields>),
    Startup(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(error) => write!(formatter, "{error:?}"),
            Self::Database(fields) => {
                write!(formatter, "{}", fields.message.clone().unwrap_or_default())
            }
            Self::Startup(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for HostError {}

impl From<wasmtime::Error> for HostError {
    fn from(error: wasmtime::Error) -> Self {
        Self::Engine(error)
    }
}

pub type Result<T> = std::result::Result<T, HostError>;

/// One open database: the main module plus its store.
pub struct Pglite {
    store: Store<Runtime>,
    pub main: usize,
    pub closed: bool,
    pub initdb_report: Option<InitdbReport>,
    pub types: TypeRegistry,
    /// Set once an epoch deadline interrupted Wasm; the instance state is
    /// then unknown and the host must not reuse it.
    pub interrupted: bool,
}

/// Advances the engine's epoch every `EPOCH_TICK`. Holds the engine weakly
/// and stops once the last `Compiled` using it is dropped.
fn spawn_epoch_ticker(
    engine: wasmtime::EngineWeak,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("pglite-epoch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(EPOCH_TICK);
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                engine.increment_epoch();
            }
        })
}

/// True when Wasm stopped because the epoch deadline passed.
#[must_use]
pub fn is_interrupt(error: &wasmtime::Error) -> bool {
    error.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::Interrupt)
}

/// Appends `item`, keeping only the newest `limit` entries.
fn push_bounded(list: &mut Vec<String>, item: String, limit: usize) {
    list.push(item);
    if list.len() > limit {
        let excess = list.len() - limit;
        list.drain(..excess);
    }
}

/// The read callback's copy: `HEAP8.set(input.subarray(...), pointer)`,
/// which throws a `RangeError` when the target lies outside memory.
fn copy_input(
    memory: &mut [u8],
    pointer: usize,
    input: &[u8],
    offset: usize,
    maximum: usize,
) -> std::result::Result<usize, String> {
    let available = input.len().saturating_sub(offset).min(maximum);
    let slot = pointer
        .checked_add(available)
        .and_then(|end| memory.get_mut(pointer..end))
        .ok_or_else(|| "RangeError: offset is out of bounds".to_owned())?;
    slot.copy_from_slice(&input[offset..offset + available]);
    Ok(available)
}

/// A failed request, shaped like the retained host's `errorPayload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
}

impl RemoteError {
    fn from_database(fields: &ErrorFields) -> Self {
        Self {
            code: fields.code.clone().unwrap_or_else(|| "REMOTE_ERROR".into()),
            message: fields.message.clone().unwrap_or_default(),
            details: serde_json::json!({
                "name": "error",
                "severity": fields.severity,
                "detail": fields.detail,
                "hint": fields.hint,
                "position": fields.position,
                "schema": fields.schema,
                "table": fields.table,
                "column": fields.column,
                "constraint": fields.constraint,
            }),
        }
    }

    fn from_js(error: &JsError) -> Self {
        Self::plain(error.name, &error.message)
    }

    fn plain(name: &str, message: &str) -> Self {
        Self {
            code: "REMOTE_ERROR".into(),
            message: message.to_owned(),
            details: serde_json::json!({
                "name": name,
                "severity": null,
                "detail": null,
                "hint": null,
                "position": null,
                "schema": null,
                "table": null,
                "column": null,
                "constraint": null,
            }),
        }
    }

    /// A host failure the JavaScript glue would raise as an exception.
    #[must_use]
    pub fn from_host(error: &HostError) -> Self {
        match error {
            HostError::Database(fields) => Self::from_database(fields),
            HostError::Engine(error) => Self::plain("RuntimeError", &format!("{error}")),
            HostError::Startup(message) => Self::plain("Error", message),
        }
    }
}

/// `encodeResult` of one `PGlite` query result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<IpcValue>>,
    pub affected_rows: u64,
}

/// `mn`: the affected-row count `PGlite` reads from a command tag.
fn command_rows(tag: &str) -> u64 {
    let parts: Vec<&str> = tag.split(' ').collect();
    let index = match parts.first().copied() {
        Some("INSERT") => 2,
        Some("UPDATE" | "DELETE" | "COPY" | "MERGE") => 1,
        _ => return 0,
    };
    // parseInt: leading digits, NaN (here 0) otherwise.
    parts
        .get(index)
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct InitdbReport {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub entries: usize,
}

fn postgres_spec() -> ModuleSpec {
    ModuleSpec {
        label: "pglite",
        layout: PGLITE_LAYOUT,
        this_program: "/pglite/bin/postgres".into(),
        no_exit_runtime: true,
        default_lang: default_lang(),
    }
}

/// Instantiates `pglite.wasm` and runs the `preRun` callbacks in the order
/// the glue executes them (reverse of registration), then `initRuntime` and
/// `onRuntimeInitialized`.
fn setup_postgres(
    store: &mut Store<Runtime>,
    compiled: &Compiled,
    host_root: Option<&Path>,
) -> Result<usize> {
    let fs = Rc::new(RefCell::new(Fs::new()));
    // PGlite's print and printErr drop output at debug level 0.
    fs.borrow_mut().console.discard = true;
    fs.borrow_mut()
        .static_init()
        .map_err(|error| fs_step("static init", error))?;
    let index = runtime::create_module(store, &postgres_spec(), Rc::clone(&fs))?;
    runtime::instantiate_main(store, index, &compiled.pglite)?;
    {
        let mut fs = fs.borrow_mut();
        // File packager.
        for (parent, name) in &compiled.package.directories {
            fs.create_path(parent, name);
        }
        for file in &compiled.package.files {
            fs.create_data_file(&file.path, compiled.package.file_bytes(file))
                .map_err(|error| fs_step("packaged file", error))?;
        }
        // NodeFS.init.
        if let Some(root) = host_root {
            fs.mkdir(PGDATA, 511)
                .map_err(|error| fs_step("mkdir data directory", error))?;
            fs.mount(&MountKind::Host(root.to_path_buf()), Some(PGDATA))
                .map_err(|error| fs_step("mount data directory", error))?;
        }
        // chmod of .pgpass and the two executables.
        fs.chmod_path("/home/postgres/.pgpass", 384, false)
            .map_err(|error| fs_step("chmod .pgpass", error))?;
        fs.chmod_path("/pglite/bin/initdb", 365, false)
            .map_err(|error| fs_step("chmod initdb", error))?;
        fs.chmod_path("/pglite/bin/postgres", 365, false)
            .map_err(|error| fs_step("chmod postgres", error))?;
    }
    store.data_mut().modules[index].env = [
        ("HOME", "/home/postgres"),
        ("USER", "postgres"),
        ("LOGNAME", "postgres"),
        ("PGDATA", PGDATA),
        ("PGUSER", "postgres"),
        ("PGDATABASE", "postgres"),
        ("LC_CTYPE", "en_US.UTF-8"),
        ("LC_COLLATE", "en_US.UTF-8"),
        ("LANG", "en_US.UTF-8"),
        ("TZ", "UTC"),
        ("PGTZ", "UTC"),
        ("PGCLIENTENCODING", "UTF8"),
        ("ICU_DATA", "/pglite/icu"),
    ]
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect();
    {
        let mut fs = fs.borrow_mut();
        fs.register_device(makedev(64, 0), Device::Blob);
        fs.mkdev("/dev/blob", None, makedev(64, 0))
            .map_err(|error| fs_step("mkdev /dev/blob", error))?;
    }
    runtime::init_runtime(store, index)?;
    install_pglite_callbacks(store, index)?;
    Ok(index)
}

/// `onRuntimeInitialized` of the `PGlite` class (`Je`).
fn install_pglite_callbacks(store: &mut Store<Runtime>, index: usize) -> Result<()> {
    let system = runtime::add_callback(store, index, Callback::PgliteSystem, "pi")?;
    call_i32(store, index, "pgl_set_system_fn", &[Val::I32(system)])?;
    let popen = runtime::add_callback(store, index, Callback::PglitePopen, "ppp")?;
    call_i32(store, index, "pgl_set_popen_fn", &[Val::I32(popen)])?;
    let pclose = runtime::add_callback(store, index, Callback::PglitePclose, "pi")?;
    call_i32(store, index, "pgl_set_pclose_fn", &[Val::I32(pclose)])?;
    let write = runtime::add_callback(store, index, Callback::PgliteWrite, "iii")?;
    let read = runtime::add_callback(store, index, Callback::PgliteRead, "iii")?;
    call_i32(
        store,
        index,
        "pgl_set_rw_cbs",
        &[Val::I32(read), Val::I32(write)],
    )?;
    Ok(())
}

/// Runs initdb against a memory-filesystem `PGlite` instance and returns the
/// walked data directory, as `PGlite.create({noInitDb})` plus `Be` do.
#[allow(
    clippy::too_many_lines,
    reason = "follows the glue initdb runner step by step"
)]
fn run_initdb(
    store: &mut Store<Runtime>,
    compiled: &Compiled,
) -> Result<(Vec<vfs::WalkEntry>, InitdbReport)> {
    let postgres = setup_postgres(store, compiled, None)?;
    let snapshot = {
        let memory = store.data().modules[postgres].memory;
        memory.data(&*store).to_vec()
    };
    let initdb_fs = Rc::new(RefCell::new(Fs::new()));
    initdb_fs
        .borrow_mut()
        .static_init()
        .map_err(|error| fs_step("initdb static init", error))?;
    let initdb = runtime::create_module(
        store,
        &ModuleSpec {
            label: "initdb",
            layout: INITDB_LAYOUT,
            this_program: "/pglite/bin/initdb".into(),
            no_exit_runtime: false,
            default_lang: default_lang(),
        },
        Rc::clone(&initdb_fs),
    )?;
    runtime::instantiate_main(store, initdb, &compiled.initdb)?;
    let postgres_fs = Rc::clone(&store.data().modules[postgres].fs);
    {
        let mut fs = initdb_fs.borrow_mut();
        fs.mkdir("/pglite", 511)
            .map_err(|error| fs_step("mkdir /pglite in initdb", error))?;
        fs.mount(
            &MountKind::Proxy {
                target: postgres_fs,
                root: "/pglite".into(),
            },
            Some("/pglite"),
        )
        .map_err(|error| fs_step("mount proxy", error))?;
    }
    store.data_mut().modules[initdb].env = [
        ("PGDATA", PGDATA),
        ("HOME", "/home/postgres"),
        ("USER", "postgres"),
        ("LOGNAME", "postgres"),
        ("ICU_DATA", "/pglite/icu"),
    ]
    .iter()
    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
    .collect();
    runtime::init_runtime(store, initdb)?;
    // onRuntimeInitialized of the initdb module.
    let system = runtime::add_callback(store, initdb, Callback::InitdbSystem, "pi")?;
    call_i32(store, initdb, "pgl_set_system_fn", &[Val::I32(system)])?;
    let popen = runtime::add_callback(store, initdb, Callback::InitdbPopen, "ppi")?;
    call_i32(store, initdb, "pgl_set_popen_fn", &[Val::I32(popen)])?;
    let pclose = runtime::add_callback(store, initdb, Callback::InitdbPclose, "pi")?;
    call_i32(store, initdb, "pgl_set_pclose_fn", &[Val::I32(pclose)])?;
    let stdin_path = string_on_stack(store, postgres, "/pglite/pgstdin")?;
    let read_mode = string_on_stack(store, postgres, "r")?;
    call_i32(
        store,
        postgres,
        "pgl_freopen",
        &[Val::I32(stdin_path), Val::I32(read_mode), Val::I32(0)],
    )?;
    let stdout_path = string_on_stack(store, postgres, "/pglite/pgstdout")?;
    let write_mode = string_on_stack(store, postgres, "w")?;
    call_i32(
        store,
        postgres,
        "pgl_freopen",
        &[Val::I32(stdout_path), Val::I32(write_mode), Val::I32(1)],
    )?;
    let out_path = string_on_stack(store, initdb, "/pglite/pgstdout")?;
    let out_mode = string_on_stack(store, initdb, "r")?;
    let read_stream = call_i32(
        store,
        initdb,
        "fopen",
        &[Val::I32(out_path), Val::I32(out_mode)],
    )?;
    let in_path = string_on_stack(store, initdb, "/pglite/pgstdin")?;
    let in_mode = string_on_stack(store, initdb, "w")?;
    let write_stream = call_i32(
        store,
        initdb,
        "fopen",
        &[Val::I32(in_path), Val::I32(in_mode)],
    )?;
    store.data_mut().modules[initdb].initdb = Some(InitdbIo {
        postgres,
        snapshot,
        command: Vec::new(),
        result: 0,
        pending_write: false,
        read_stream,
        write_stream,
    });
    let args: Vec<String> = INITDB_ARGS
        .iter()
        .map(|value| (*value).to_owned())
        .collect();
    let exit_code = runtime::call_main(store, initdb, &args)?;
    let console = initdb_fs.borrow().console.clone();
    let report = InitdbReport {
        exit_code,
        stdout: console.stdout.concat(),
        stderr: console.stderr.concat(),
        entries: 0,
    };
    if exit_code != 0 && !report.stderr.contains("exists but is not empty") {
        return Err(HostError::Startup(format!(
            "INITDB failed to initialize: {}",
            report.stderr
        )));
    }
    let entries = store.data().modules[postgres]
        .fs
        .borrow_mut()
        .walk(PGDATA)
        .map_err(|error| fs_step("walk data directory", error))?;
    let report = InitdbReport {
        entries: entries.len(),
        ..report
    };
    Ok((entries, report))
}

/// The tar loader `at`: recreate every entry under `PGDATA`.
fn load_entries(fs: &mut Fs, entries: &[vfs::WalkEntry]) -> Result<()> {
    for entry in entries {
        // tinytar keeps at most 99 bytes of a name.
        let name: String = entry.name.chars().take(99).collect();
        let path = format!("{PGDATA}/{name}");
        let parts: Vec<&str> = path.split('/').collect();
        for count in 1..parts.len() {
            let prefix = parts[..count].join("/");
            if !fs.exists(&prefix) {
                fs.mkdir(&prefix, 511)
                    .map_err(|error| fs_step("tar mkdir parent", error))?;
            }
        }
        if entry.is_file {
            fs.write_file(&path, &entry.data, 577, 438)
                .map_err(|error| fs_step("tar write file", error))?;
            // Seconds from the tar header passed where the glue expects
            // milliseconds.
            let seconds = (entry.mtime / 1000.0).floor();
            fs.utime(&path, Some(seconds), Some(seconds))
                .map_err(|error| fs_step("tar utime", error))?;
        } else if !fs.exists(&path) {
            fs.mkdir(&path, 511)
                .map_err(|error| fs_step("tar mkdir", error))?;
        }
    }
    Ok(())
}

impl Pglite {
    /// `new PGlite(dataDir)` followed by `waitReady`.
    ///
    /// The store must stay on a thread with the store stack size: call this
    /// inside `host::run_on_store_thread`, or use `host::PgliteHost`, which
    /// owns that thread.
    ///
    /// # Errors
    ///
    /// Fails when the data directory cannot be created, initdb fails, or the
    /// server does not start.
    pub fn open(compiled: &Compiled, data_directory: &Path) -> Result<Self> {
        let root = std::path::absolute(data_directory)
            .map_err(|error| HostError::Startup(error.to_string()))?;
        if !root.exists() {
            std::fs::create_dir(&root).map_err(|error| HostError::Startup(error.to_string()))?;
        }
        let mut store = Store::new(&compiled.engine, Runtime::new(compiled.engine.clone()));
        store.epoch_deadline_trap();
        store.set_epoch_deadline(NO_DEADLINE);
        let main = setup_postgres(&mut store, compiled, Some(&root))?;
        let fs = Rc::clone(&store.data().modules[main].fs);
        let mut initdb_report = None;
        if !fs.borrow_mut().exists(&format!("{PGDATA}/PG_VERSION")) {
            let (entries, report) = run_initdb(&mut store, compiled)?;
            load_entries(&mut fs.borrow_mut(), &entries)?;
            initdb_report = Some(report);
        }
        call_i32(&mut store, main, "pgl_setPGliteActive", &[Val::I32(1)])?;
        let mut args: Vec<String> = START_PARAMS
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        args.extend(["-D".to_owned(), PGDATA.to_owned(), "postgres".to_owned()]);
        let status = runtime::call_main(&mut store, main, &args)?;
        if status != 99 {
            return Err(HostError::Startup(format!(
                "PGlite failed to initialize properly (main returned {status})"
            )));
        }
        call_i32(&mut store, main, "pgl_startPGlite", &[])?;
        let mut pglite = Self {
            store,
            main,
            closed: false,
            initdb_report,
            types: TypeRegistry::default(),
            interrupted: false,
        };
        let array_types = pglite.query_messages(
            "\n      SELECT b.oid, b.typarray\n      FROM pg_catalog.pg_type a\n      LEFT JOIN pg_catalog.pg_type b ON b.oid = a.typelem\n      WHERE a.typcategory = 'A'\n      GROUP BY b.oid, b.typarray\n      ORDER BY b.oid\n    ",
            &[],
        )?;
        for message in array_types {
            if let Backend::DataRow(cells) = message {
                let element = cells
                    .first()
                    .cloned()
                    .flatten()
                    .and_then(|text| text.parse().ok());
                let array = cells
                    .get(1)
                    .cloned()
                    .flatten()
                    .and_then(|text| text.parse().ok());
                if let Some(array) = array {
                    pglite.types.arrays.insert(array, element.unwrap_or(0));
                }
            }
        }
        Ok(pglite)
    }

    /// `query(sql, params, { parsers: jsonParsers })` then `encodeResult`.
    ///
    /// # Errors
    ///
    /// Returns the payload the retained host sends for a failed query.
    pub fn query(
        &mut self,
        sql: &str,
        params: &[IpcValue],
    ) -> std::result::Result<QueryResult, RemoteError> {
        let mut messages = Vec::new();
        let mut parameter_types = Vec::new();
        let outcome: std::result::Result<(), RemoteError> = (|| {
            for message in [protocol::parse(sql, &[]), protocol::describe(b'S')] {
                let (results, error) = self
                    .exec_protocol(&message)
                    .map_err(|error| RemoteError::from_host(&error))?;
                if let Some(error) = error {
                    return Err(RemoteError::from_database(&error));
                }
                for result in &results {
                    if let Backend::ParameterDescription(types) = result {
                        parameter_types.clone_from(types);
                    }
                }
                messages.extend(results);
            }
            let mut bound = Vec::with_capacity(params.len());
            for (index, value) in params.iter().enumerate() {
                let oid = parameter_types.get(index).copied().unwrap_or(0);
                bound.push(
                    serialize_param(value, oid, &self.types)
                        .map_err(|error| RemoteError::from_js(&error))?,
                );
            }
            for message in [
                protocol::bind(&bound),
                protocol::describe(b'P'),
                protocol::execute(),
            ] {
                let (results, error) = self
                    .exec_protocol(&message)
                    .map_err(|error| RemoteError::from_host(&error))?;
                if let Some(error) = error {
                    return Err(RemoteError::from_database(&error));
                }
                messages.extend(results);
            }
            Ok(())
        })();
        let sync = self.exec_protocol(&protocol::sync());
        outcome?;
        sync.map_err(|error| RemoteError::from_host(&error))?;
        self.encode_first_result(&messages)
    }

    /// `parseResults(messages)[0]` followed by `encodeResult`.
    fn encode_first_result(
        &self,
        messages: &[Backend],
    ) -> std::result::Result<QueryResult, RemoteError> {
        let clock = &self.store.data().clock;
        let mut fields: Vec<(String, i32)> = Vec::new();
        let mut rows: Vec<Vec<IpcValue>> = Vec::new();
        let mut affected = 0_u64;
        for message in messages {
            match message {
                Backend::RowDescription(described) => {
                    fields = described
                        .iter()
                        .map(|field| (field.name.clone(), field.type_oid))
                        .collect();
                }
                Backend::DataRow(cells) => {
                    // Object.fromEntries: a later duplicate name wins.
                    let mut by_name: Vec<(String, JsValue, i32)> = Vec::new();
                    for (index, cell) in cells.iter().enumerate() {
                        let (name, oid) = fields.get(index).cloned().unwrap_or_default();
                        let value = self
                            .types
                            .parse_cell(clock, oid, cell.as_deref())
                            .map_err(|error| RemoteError::from_js(&error))?;
                        if let Some(entry) = by_name.iter_mut().find(|entry| entry.0 == name) {
                            entry.1 = value;
                        } else {
                            by_name.push((name, value, oid));
                        }
                    }
                    let mut row = Vec::with_capacity(fields.len());
                    for (name, oid) in &fields {
                        let value = by_name
                            .iter()
                            .find(|entry| &entry.0 == name)
                            .map_or(JsValue::Null, |entry| entry.1.clone());
                        row.push(
                            encode_value(&value, *oid)
                                .map_err(|error| RemoteError::from_js(&error))?,
                        );
                    }
                    rows.push(row);
                }
                Backend::CommandComplete(tag) => {
                    affected += command_rows(tag);
                    let columns = fields.iter().map(|(name, _)| name.clone()).collect();
                    let row_count = rows.len() as u64;
                    return Ok(QueryResult {
                        columns,
                        rows,
                        affected_rows: if row_count > 0 { row_count } else { affected },
                    });
                }
                _ => {}
            }
        }
        Ok(QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
            affected_rows: 0,
        })
    }

    /// `exec(sql)`.
    ///
    /// # Errors
    ///
    /// Returns the payload the retained host sends for a failed statement.
    pub fn execute(&mut self, sql: &str) -> std::result::Result<(), RemoteError> {
        match self.exec_messages(sql) {
            Ok(_) => Ok(()),
            Err(error) => Err(RemoteError::from_host(&error)),
        }
    }

    /// `transaction(async (tx) => ...)` running each statement as a query.
    ///
    /// # Errors
    ///
    /// Returns the first failure after `ROLLBACK`.
    pub fn transaction(
        &mut self,
        statements: &[(String, Vec<IpcValue>)],
    ) -> std::result::Result<Vec<QueryResult>, RemoteError> {
        self.execute("BEGIN")?;
        let mut results = Vec::with_capacity(statements.len());
        for (sql, params) in statements {
            match self.query(sql, params) {
                Ok(result) => results.push(result),
                Err(error) => {
                    self.execute("ROLLBACK")?;
                    return Err(error);
                }
            }
        }
        // The closed flag is set before COMMIT runs, so a failed COMMIT is
        // not followed by ROLLBACK.
        self.execute("COMMIT")?;
        Ok(results)
    }

    /// Limits Wasm execution of the following requests to about `limit`, or
    /// removes the limit.
    pub fn set_deadline(&mut self, limit: Option<std::time::Duration>) {
        let ticks = limit.map_or(NO_DEADLINE, |limit| {
            let ticks = limit.as_millis() / EPOCH_TICK.as_millis();
            u64::try_from(ticks).unwrap_or(NO_DEADLINE).max(1)
        });
        self.store.set_epoch_deadline(ticks);
    }

    /// `execProtocolRawSync`.
    ///
    /// # Errors
    ///
    /// Fails when the module raises an exception the glue does not catch.
    pub fn exec_protocol_raw(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        let main = self.main;
        self.fire_timers()?;
        {
            let io = &mut self.store.data_mut().modules[main].io;
            io.read_offset = 0;
            io.output.clear();
            io.input = message.to_vec();
        }
        if message.first() == Some(&88) {
            return Ok(Vec::new());
        }
        if message.first() == Some(&0) {
            return Err(HostError::Startup(
                "startup packets are not used by this host".into(),
            ));
        }
        let mut swallowed = 0_u32;
        let loop_result: Result<()> = (|| {
            loop {
                let io = &self.store.data().modules[main].io;
                let pending = io.read_offset < io.input.len();
                let remaining = call_i32(&mut self.store, main, "pq_buffer_remaining_data", &[])?;
                if !pending && remaining <= 0 {
                    return Ok(());
                }
                let before = self.store.data().modules[main].io.read_offset;
                match call_i32(&mut self.store, main, "PostgresMainLoopOnce", &[]) {
                    Ok(_) => swallowed = 0,
                    Err(error) if is_interrupt(&error) => {
                        return Err(HostError::Engine(error));
                    }
                    Err(error) => {
                        let status = error.downcast_ref::<ExitStatus>().map(|status| status.0);
                        if status == Some(POSTGRES_MAIN_LONGJMP) {
                            self.store.data_mut().count("postgres_main_longjmp");
                            call_i32(&mut self.store, main, "PostgresMainLongJmp", &[])?;
                            swallowed = 0;
                        } else {
                            self.store.data_mut().count("wire_loop_swallowed_exception");
                            push_bounded(
                                &mut self.store.data_mut().swallowed,
                                format!("{error:?}"),
                                SWALLOWED_KEPT,
                            );
                            if self.store.data().modules[main].io.read_offset == before {
                                swallowed += 1;
                                if swallowed >= SWALLOWED_EXCEPTION_LIMIT {
                                    return Err(HostError::Engine(error));
                                }
                            }
                        }
                    }
                }
            }
        })();
        let finish = call_i32(
            &mut self.store,
            main,
            "PostgresSendReadyForQueryIfNecessary",
            &[],
        )
        .and_then(|_| call_i32(&mut self.store, main, "pgl_pq_flush", &[]));
        if let Err(HostError::Engine(error)) = &loop_result
            && is_interrupt(error)
        {
            self.interrupted = true;
        }
        if let Err(error) = &finish
            && is_interrupt(error)
        {
            self.interrupted = true;
        }
        loop_result?;
        finish?;
        let io = &mut self.store.data_mut().modules[main].io;
        io.input.clear();
        Ok(std::mem::take(&mut io.output))
    }

    /// `execProtocol` with `throwOnError`: the messages up to the first
    /// error, and that error.
    ///
    /// # Errors
    ///
    /// Fails when the raw exchange fails.
    pub fn exec_protocol(&mut self, message: &[u8]) -> Result<(Vec<Backend>, Option<ErrorFields>)> {
        let output = self.exec_protocol_raw(message)?;
        let mut results = Vec::new();
        let mut error = None;
        for backend in protocol::parse_messages(&output) {
            if error.is_some() {
                continue;
            }
            if let Backend::Error(fields) = &backend {
                error = Some(fields.clone());
            }
            results.push(backend);
        }
        Ok((results, error))
    }

    /// `exec(sql)`: simple query then sync.
    ///
    /// # Errors
    ///
    /// Returns the first `ErrorResponse` as [`HostError::Database`].
    pub fn exec_messages(&mut self, sql: &str) -> Result<Vec<Backend>> {
        let (mut messages, error) = self.exec_protocol(&protocol::query(sql))?;
        let (sync_messages, _) = self.exec_protocol(&protocol::sync())?;
        if let Some(error) = error {
            return Err(HostError::Database(Box::new(error)));
        }
        messages.extend(sync_messages);
        Ok(messages)
    }

    /// `query(sql, params)` message exchange with already-serialized values.
    ///
    /// # Errors
    ///
    /// Returns the first `ErrorResponse` as [`HostError::Database`].
    pub fn query_messages(
        &mut self,
        sql: &str,
        params: &[protocol::BindValue],
    ) -> Result<Vec<Backend>> {
        let mut messages = Vec::new();
        let outcome: Result<()> = (|| {
            for message in [protocol::parse(sql, &[]), protocol::describe(b'S')] {
                let (results, error) = self.exec_protocol(&message)?;
                if let Some(error) = error {
                    return Err(HostError::Database(Box::new(error)));
                }
                messages.extend(results);
            }
            for message in [
                protocol::bind(params),
                protocol::describe(b'P'),
                protocol::execute(),
            ] {
                let (results, error) = self.exec_protocol(&message)?;
                if let Some(error) = error {
                    return Err(HostError::Database(Box::new(error)));
                }
                messages.extend(results);
            }
            Ok(())
        })();
        let (sync_messages, _) = self.exec_protocol(&protocol::sync())?;
        outcome?;
        messages.extend(sync_messages);
        Ok(messages)
    }

    /// Runs `setTimeout` callbacks whose deadline passed, as Node does when
    /// the host returns to its event loop between requests.
    fn fire_timers(&mut self) -> Result<()> {
        let main = self.main;
        let now = self.store.data().origin.elapsed().as_secs_f64() * 1000.0;
        let mut due: Vec<i32> = self.store.data().modules[main]
            .timers
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(which, _)| *which)
            .collect();
        due.sort_unstable();
        for which in due {
            self.store.data_mut().modules[main].timers.remove(&which);
            let now = self.store.data().origin.elapsed().as_secs_f64() * 1000.0;
            let outcome = call_i32(
                &mut self.store,
                main,
                "_emscripten_timeout",
                &[Val::I32(which), Val::F64(now.to_bits())],
            );
            match outcome {
                Ok(_) => {}
                Err(error) if error.is::<ExitStatus>() => {}
                Err(error) => return Err(HostError::Engine(error)),
            }
        }
        Ok(())
    }

    /// `close()`.
    ///
    /// # Errors
    ///
    /// Fails when a step raises an exception the glue would rethrow.
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        let main = self.main;
        let steps: Result<()> = (|| {
            call_i32(&mut self.store, main, "pgl_setPGliteActive", &[Val::I32(0)])?;
            self.exec_protocol(&protocol::end())?;
            call_i32(&mut self.store, main, "pgl_run_atexit_funcs", &[])?;
            Ok(())
        })();
        if let Err(HostError::Engine(error)) = &steps {
            let status = error.downcast_ref::<ExitStatus>().map(|status| status.0);
            if status != Some(0) {
                self.store.data_mut().count("close_step_error");
            }
        }
        let callbacks: Vec<u32> = {
            let module = &self.store.data().modules[main];
            module
                .callbacks
                .iter()
                .filter(|(_, callback)| {
                    matches!(callback, Callback::PgliteRead | Callback::PgliteWrite)
                })
                .map(|(slot, _)| *slot)
                .collect()
        };
        let mut ordered = callbacks;
        ordered.sort_unstable();
        // removeFunction(me) then removeFunction(ce): read callback first.
        let read_slot = ordered.iter().copied().find(|slot| {
            self.store.data().modules[main].callbacks.get(slot) == Some(&Callback::PgliteRead)
        });
        let write_slot = ordered.iter().copied().find(|slot| {
            self.store.data().modules[main].callbacks.get(slot) == Some(&Callback::PgliteWrite)
        });
        for slot in [read_slot, write_slot].into_iter().flatten() {
            runtime::remove_function(&mut self.store, main, slot.cast_signed())?;
        }
        runtime::fs_quit(&mut self.store, main)?;
        self.closed = true;
        // _emscripten_force_exit(0): keepalive clear, then exit.
        self.store.data_mut().modules[main].no_exit_runtime = false;
        match runtime::exit_js(&mut self.store, main, 0) {
            Ok(()) => Ok(()),
            Err(error)
                if error
                    .downcast_ref::<ExitStatus>()
                    .is_some_and(|status| status.0 == 0) =>
            {
                Ok(())
            }
            Err(error) => Err(HostError::Engine(error)),
        }
    }

    #[must_use]
    pub fn counters(&self) -> std::collections::HashMap<&'static str, u64> {
        self.store.data().counters.clone()
    }
}

/// Body of a function placed in the table with `addFunction`.
#[allow(clippy::too_many_lines, reason = "one arm per glue callback")]
pub(crate) fn run_callback(
    mut caller: Caller<'_, Runtime>,
    index: usize,
    callback: Callback,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let arg = |position: usize| params.get(position).and_then(Val::i32).unwrap_or(0);
    let mut set = |value: i32| {
        if let Some(slot) = results.first_mut() {
            *slot = Val::I32(value);
        }
    };
    match callback {
        Callback::PgliteRead => {
            let pointer = arg(0).cast_unsigned() as usize;
            let maximum = usize::try_from(arg(1)).unwrap_or(0);
            let memory = caller.data().modules[index].memory;
            let (data, runtime) = memory.data_and_store_mut(&mut caller);
            let io = &mut runtime.modules[index].io;
            let available = copy_input(data, pointer, &io.input, io.read_offset, maximum)
                .map_err(abort_error)?;
            io.read_offset += available;
            set(i32::try_from(available).unwrap_or(i32::MAX));
            Ok(())
        }
        Callback::PgliteWrite => {
            let pointer = arg(0).cast_unsigned() as usize;
            let length = usize::try_from(arg(1)).unwrap_or(0);
            let memory = caller.data().modules[index].memory;
            let (data, runtime) = memory.data_and_store_mut(&mut caller);
            let bytes = data
                .get(pointer..pointer + length)
                .ok_or_else(|| abort_error("RangeError: write callback outside memory"))?;
            runtime.modules[index].io.output.extend_from_slice(bytes);
            set(i32::try_from(length).unwrap_or(i32::MAX));
            Ok(())
        }
        Callback::PgliteSystem => {
            // "Postgres tried to execute <cmd>, returning 1."
            set(1);
            Ok(())
        }
        Callback::PglitePopen => {
            let memory = caller.data().modules[index].memory;
            let command = c_string(memory.data(&caller), arg(0).cast_unsigned(), None);
            let mode = c_string(memory.data(&caller), arg(1).cast_unsigned(), None);
            if command.starts_with("locale -a") && mode == "r" {
                let path = string_on_stack(&mut caller, index, "/pglite/locale-a")?;
                let mode_pointer = string_on_stack(&mut caller, index, &mode)?;
                let stream = call_i32(
                    &mut caller,
                    index,
                    "fopen",
                    &[Val::I32(path), Val::I32(mode_pointer)],
                )?;
                caller.data_mut().modules[index].io.external_stream = Some(stream);
                set(stream);
                return Ok(());
            }
            Err(abort_error("Error: Unhandled cmd"))
        }
        Callback::PglitePclose => {
            let stream = arg(0);
            if caller.data().modules[index].io.external_stream == Some(stream) {
                call_i32(&mut caller, index, "fclose", &[Val::I32(stream)])?;
                caller.data_mut().modules[index].io.external_stream = None;
                set(0);
                return Ok(());
            }
            Err(abort_error(format!("Unhandled pclose {stream}")))
        }
        Callback::InitdbSystem => {
            let memory = caller.data().modules[index].memory;
            let command = c_string(memory.data(&caller), arg(0).cast_unsigned(), None);
            let words = crate::shell::command_words(&command);
            set_command(&mut caller, index, words.clone());
            let status = run_postgres_command(&mut caller, index, words)?;
            set(status);
            Ok(())
        }
        Callback::InitdbPopen => {
            let memory = caller.data().modules[index].memory;
            let command = c_string(memory.data(&caller), arg(0).cast_unsigned(), None);
            let mode = c_string(memory.data(&caller), arg(1).cast_unsigned(), None);
            let words = crate::shell::command_words(&command);
            set_command(&mut caller, index, words.clone());
            match mode.as_str() {
                "r" => {
                    let status = run_postgres_command(&mut caller, index, words)?;
                    let io = caller.data_mut().modules[index]
                        .initdb
                        .as_mut()
                        .ok_or_else(|| abort_error("initdb state missing"))?;
                    io.result = status;
                    let stream = io.read_stream;
                    set(stream);
                    Ok(())
                }
                "w" => {
                    let io = caller.data_mut().modules[index]
                        .initdb
                        .as_mut()
                        .ok_or_else(|| abort_error("initdb state missing"))?;
                    io.pending_write = true;
                    let stream = io.write_stream;
                    set(stream);
                    Ok(())
                }
                other => Err(abort_error(format!("Unexpected popen mode value {other}"))),
            }
        }
        Callback::InitdbPclose => {
            let stream = arg(0);
            let (read_stream, write_stream, pending) = {
                let io = caller.data().modules[index]
                    .initdb
                    .as_ref()
                    .ok_or_else(|| abort_error("initdb state missing"))?;
                (io.read_stream, io.write_stream, io.pending_write)
            };
            if stream == read_stream || stream == write_stream {
                if pending {
                    let words = caller.data().modules[index]
                        .initdb
                        .as_ref()
                        .map(|io| io.command.clone())
                        .unwrap_or_default();
                    if let Some(io) = caller.data_mut().modules[index].initdb.as_mut() {
                        io.pending_write = false;
                    }
                    let status = run_postgres_command(&mut caller, index, words)?;
                    if let Some(io) = caller.data_mut().modules[index].initdb.as_mut() {
                        io.result = status;
                    }
                }
                let result = caller.data().modules[index]
                    .initdb
                    .as_ref()
                    .map_or(0, |io| io.result);
                set(result);
                return Ok(());
            }
            let status = call_i32(&mut caller, index, "pclose", &[Val::I32(stream)])?;
            set(status);
            Ok(())
        }
    }
}

fn set_command(caller: &mut Caller<'_, Runtime>, index: usize, words: Vec<String>) {
    if let Some(io) = caller.data_mut().modules[index].initdb.as_mut() {
        io.command = words;
    }
}

/// `_` in the initdb runner: restore the postgres memory snapshot and call
/// its `main` with the parsed command.
fn run_postgres_command(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    words: Vec<String>,
) -> wasmtime::Result<i32> {
    let mut words = words.into_iter();
    let program = words.next().unwrap_or_default();
    if program != "/pglite/bin/postgres" {
        return Err(abort_error(format!("trying to execute {program}")));
    }
    let (postgres, snapshot) = {
        let io = caller.data().modules[index]
            .initdb
            .as_ref()
            .ok_or_else(|| abort_error("initdb state missing"))?;
        (io.postgres, io.snapshot.clone())
    };
    let memory = caller.data().modules[postgres].memory;
    let data = memory.data_mut(&mut *caller);
    let length = snapshot.len().min(data.len());
    data[..length].copy_from_slice(&snapshot[..length]);
    caller.data_mut().count("initdb_postgres_main");
    let args: Vec<String> = words.collect();
    runtime::call_main(caller, postgres, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_callback_copy_raises_range_error_outside_memory() {
        let mut memory = vec![0_u8; 8];
        assert_eq!(copy_input(&mut memory, 2, b"abcdef", 1, 3), Ok(3));
        assert_eq!(&memory[2..5], b"bcd");
        assert_eq!(
            copy_input(&mut memory, 6, b"abcdef", 0, 4),
            Err("RangeError: offset is out of bounds".to_owned())
        );
        assert_eq!(copy_input(&mut memory, 8, b"abc", 3, 4), Ok(0));
    }

    #[test]
    fn epoch_ticker_stops_when_the_engine_is_dropped() {
        let engine = Engine::default();
        let ticker = spawn_epoch_ticker(engine.weak()).expect("spawn ticker");
        drop(engine);
        ticker.join().expect("ticker exits");
    }

    #[test]
    fn swallowed_exception_log_keeps_the_newest_entries() {
        let mut list = Vec::new();
        for item in 0..40 {
            push_bounded(&mut list, item.to_string(), 32);
        }
        assert_eq!(list.len(), 32);
        assert_eq!(list.first().map(String::as_str), Some("8"));
        assert_eq!(list.last().map(String::as_str), Some("39"));
    }
}
