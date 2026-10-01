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

#[derive(Debug, Clone)]
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
        let engine = Engine::new(&config)?;
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
    Database(ErrorFields),
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
    pub store: Store<Runtime>,
    pub main: usize,
    pub closed: bool,
    pub initdb_report: Option<InitdbReport>,
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
            fs.mount(MountKind::Host(root.to_path_buf()), Some(PGDATA))
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

/// Runs initdb against a memory-filesystem PGlite instance and returns the
/// walked data directory, as `PGlite.create({noInitDb})` plus `Be` do.
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
            MountKind::Proxy {
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
        };
        pglite.query_messages(
            "\n      SELECT b.oid, b.typarray\n      FROM pg_catalog.pg_type a\n      LEFT JOIN pg_catalog.pg_type b ON b.oid = a.typelem\n      WHERE a.typcategory = 'A'\n      GROUP BY b.oid, b.typarray\n      ORDER BY b.oid\n    ",
            &[],
        )?;
        Ok(pglite)
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
                    Err(error) => {
                        let status = error.downcast_ref::<ExitStatus>().map(|status| status.0);
                        if status == Some(POSTGRES_MAIN_LONGJMP) {
                            self.store.data_mut().count("postgres_main_longjmp");
                            call_i32(&mut self.store, main, "PostgresMainLongJmp", &[])?;
                            swallowed = 0;
                        } else {
                            self.store.data_mut().count("wire_loop_swallowed_exception");
                            self.store.data_mut().swallowed.push(format!("{error:?}"));
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
            return Err(HostError::Database(error));
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
                    return Err(HostError::Database(error));
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
                    return Err(HostError::Database(error));
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
pub fn run_callback(
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
            let available = io.input.len().saturating_sub(io.read_offset).min(maximum);
            if let Some(slot) = data.get_mut(pointer..pointer + available) {
                slot.copy_from_slice(&io.input[io.read_offset..io.read_offset + available]);
            }
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
