//! Emscripten runtime host for the pinned `PGlite` modules.
//!
//! This replaces the JavaScript glue around `pglite.wasm`, `initdb.wasm` and
//! the side modules they `dlopen`. Each function follows the glue function of
//! the same name; differences are recorded in the evidence, not hidden here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::time::Instant;

use wasmtime::{
    AsContext, AsContextMut, Engine, Extern, ExternType, Func, FuncType, Global, GlobalType,
    Instance, Memory, MemoryType, Module, Mutability, Ref, RefType, Table, TableType, Val, ValType,
};

use crate::jsdate::JsClock;
use crate::vfs::{Fs, FsError};

/// `throw Infinity` from `_emscripten_throw_longjmp`.
#[derive(Debug)]
pub struct Longjmp;

/// `ExitStatus` thrown by `exit` and `proc_exit`.
#[derive(Debug)]
pub struct ExitStatus(pub i32);

/// Any other JavaScript exception: `abort()`, a `TypeError`, an `Error`
/// thrown by a `PGlite` callback. The glue does not catch these.
#[derive(Debug)]
pub struct Abort(pub String);

impl fmt::Display for Longjmp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("longjmp")
    }
}
impl fmt::Display for ExitStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Program terminated with exit({})", self.0)
    }
}
impl fmt::Display for Abort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for Longjmp {}
impl std::error::Error for ExitStatus {}
impl std::error::Error for Abort {}

pub type HostResult<T> = wasmtime::Result<T>;

pub fn abort_error(message: impl Into<String>) -> wasmtime::Error {
    wasmtime::Error::new(Abort(message.into()))
}

/// Constants each glue file hard-codes.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub memory_initial_pages: u32,
    pub table_initial: u32,
    pub stack_pointer: i32,
    pub heap_base: i32,
}

pub const PGLITE_LAYOUT: Layout = Layout {
    memory_initial_pages: 2048,
    table_initial: 7367,
    stack_pointer: 11_373_728,
    heap_base: 11_373_728,
};

pub const INITDB_LAYOUT: Layout = Layout {
    memory_initial_pages: 1024,
    table_initial: 144,
    stack_pointer: 205_888,
    heap_base: 205_888,
};

const MEMORY_BASE: i32 = 1024;
const HEAP_MAX: u64 = 2_147_483_648;

/// A JavaScript function placed in the table with `addFunction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callback {
    PgliteSystem,
    PglitePopen,
    PglitePclose,
    PgliteRead,
    PgliteWrite,
    InitdbSystem,
    InitdbPopen,
    InitdbPclose,
}

/// A value in the glue's `wasmImports` namespace.
#[derive(Clone)]
pub enum Symbol {
    Func(Func),
    Data(i32),
}

pub struct Dso {
    pub exports: Vec<(String, Symbol)>,
    pub global: bool,
}

#[derive(Debug, Default)]
pub struct ProtocolIo {
    pub input: Vec<u8>,
    pub read_offset: usize,
    pub output: Vec<u8>,
    pub external_stream: Option<i32>,
}

/// State of the initdb callbacks (`Wr` in the glue).
#[derive(Debug, Default)]
pub struct InitdbIo {
    pub postgres: usize,
    pub snapshot: Vec<u8>,
    pub command: Vec<String>,
    pub result: i32,
    pub pending_write: bool,
    pub read_stream: i32,
    pub write_stream: i32,
}

/// The glue's `GOT` object: insertion-ordered globals and their
/// `required` flag.
#[derive(Default)]
pub struct Got {
    order: Vec<String>,
    entries: HashMap<String, (Global, bool)>,
}

impl Got {
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&(Global, bool)> {
        self.entries.get(name)
    }
    pub fn get_mut(&mut self, name: &str) -> Option<&mut (Global, bool)> {
        self.entries.get_mut(name)
    }
    pub fn insert(&mut self, name: String, value: (Global, bool)) {
        if !self.entries.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.entries.insert(name, value);
    }
    #[must_use]
    pub fn ordered(&self) -> Vec<(String, Global, bool)> {
        self.order
            .iter()
            .filter_map(|name| {
                self.entries
                    .get(name)
                    .map(|(global, required)| (name.clone(), *global, *required))
            })
            .collect()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the glue's runtime flags one to one"
)]
pub struct EmModule {
    pub label: &'static str,
    pub memory: Memory,
    pub table: Table,
    pub stack_pointer: Global,
    pub instance: Option<Instance>,
    pub exports: HashMap<String, Symbol>,
    pub symbols: HashMap<String, Symbol>,
    pub got: Got,
    pub dns: crate::netdb::Dns,
    pub fs: Rc<RefCell<Fs>>,
    pub env: Vec<(String, String)>,
    pub env_strings: Option<Vec<String>>,
    pub this_program: String,
    pub default_lang: String,
    pub no_exit_runtime: bool,
    pub exit_status: i32,
    pub runtime_initialized: bool,
    pub runtime_exited: bool,
    pub aborted: bool,
    pub heap_base: i32,
    pub table_map: Option<HashMap<usize, u32>>,
    pub free_slots: Vec<u32>,
    pub callbacks: HashMap<u32, Callback>,
    pub dsos_by_name: HashMap<String, usize>,
    pub dsos_by_handle: HashMap<i32, usize>,
    pub dsos: Vec<Dso>,
    pub timers: HashMap<i32, f64>,
    pub io: ProtocolIo,
    pub initdb: Option<InitdbIo>,
}

/// Store data: every Emscripten module instance and process-wide values.
pub struct Runtime {
    pub modules: Vec<EmModule>,
    pub clock: JsClock,
    pub origin: Instant,
    pub engine: Engine,
    pub counters: HashMap<&'static str, u64>,
    /// Text of exceptions the wire loop swallowed, as the glue does.
    pub swallowed: Vec<String>,
}

impl Runtime {
    #[must_use]
    pub fn new(engine: Engine) -> Self {
        Self {
            modules: Vec::new(),
            clock: JsClock::new(),
            origin: Instant::now(),
            engine,
            counters: HashMap::new(),
            swallowed: Vec::new(),
        }
    }

    pub fn count(&mut self, name: &'static str) {
        *self.counters.entry(name).or_insert(0) += 1;
    }
}

// ---------------------------------------------------------------------------
// Memory helpers (typed-array semantics: out-of-range reads give 0, writes
// are dropped)
// ---------------------------------------------------------------------------

pub fn read_u8(memory: &[u8], address: u32) -> u8 {
    memory.get(address as usize).copied().unwrap_or(0)
}

pub fn read_i32(memory: &[u8], address: u32) -> i32 {
    let start = address as usize;
    memory.get(start..start + 4).map_or(0, |bytes| {
        i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    })
}

pub fn read_u32(memory: &[u8], address: u32) -> u32 {
    read_i32(memory, address).cast_unsigned()
}

pub fn read_i16(memory: &[u8], address: u32) -> i16 {
    let start = address as usize;
    memory
        .get(start..start + 2)
        .map_or(0, |bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
}

pub fn write_bytes(memory: &mut [u8], address: u32, bytes: &[u8]) {
    let start = address as usize;
    if let Some(slot) = memory.get_mut(start..start + bytes.len()) {
        slot.copy_from_slice(bytes);
    }
}

pub fn write_u8(memory: &mut [u8], address: u32, value: u8) {
    if let Some(slot) = memory.get_mut(address as usize) {
        *slot = value;
    }
}

pub fn write_i16(memory: &mut [u8], address: u32, value: i16) {
    write_bytes(memory, address, &value.to_le_bytes());
}

pub fn write_i32(memory: &mut [u8], address: u32, value: i32) {
    write_bytes(memory, address, &value.to_le_bytes());
}

pub fn write_u32(memory: &mut [u8], address: u32, value: u32) {
    write_bytes(memory, address, &value.to_le_bytes());
}

pub fn write_i64(memory: &mut [u8], address: u32, value: i64) {
    write_bytes(memory, address, &value.to_le_bytes());
}

/// JavaScript `ToInt32` of a double, as typed-array stores apply it.
#[must_use]
pub fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    // Exact: modulo is an integer in [0, 2^32).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "modulo is an integer in [0, 2^32)"
    )]
    let unsigned = modulo as u32;
    unsigned.cast_signed()
}

/// `UTF8ToString(ptr, maxBytes)`.
#[must_use]
pub fn c_string(memory: &[u8], address: u32, max: Option<usize>) -> String {
    if address == 0 {
        return String::new();
    }
    let start = address as usize;
    let Some(tail) = memory.get(start..) else {
        return String::new();
    };
    let limit = max.map_or(tail.len(), |max| max.min(tail.len()));
    let end = tail[..limit]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(limit);
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// `stringToUTF8(str, ptr, maxBytes)`; returns bytes written without NUL.
pub fn string_to_utf8(memory: &mut [u8], text: &str, address: u32, max_bytes: usize) -> usize {
    if max_bytes == 0 {
        return 0;
    }
    let bytes = text.as_bytes();
    let mut count = 0;
    // Truncate on a character boundary, like the glue's per-code-point loop.
    for (index, character) in text.char_indices() {
        let length = character.len_utf8();
        if index + length > max_bytes - 1 {
            break;
        }
        count = index + length;
    }
    write_bytes(memory, address, &bytes[..count]);
    write_u8(memory, address + u32::try_from(count).unwrap_or(0), 0);
    count
}

// ---------------------------------------------------------------------------
// Module setup
// ---------------------------------------------------------------------------

pub struct ModuleSpec {
    pub label: &'static str,
    pub layout: Layout,
    pub this_program: String,
    pub no_exit_runtime: bool,
    pub default_lang: String,
}

/// Creates the memory, table and globals of a main module and registers an
/// empty Emscripten module record. Returns its index.
pub fn create_module(
    store: &mut wasmtime::Store<Runtime>,
    spec: &ModuleSpec,
    fs: Rc<RefCell<Fs>>,
) -> HostResult<usize> {
    let memory = Memory::new(
        store.as_context_mut(),
        MemoryType::new(spec.layout.memory_initial_pages, Some(32768)),
    )?;
    let table = Table::new(
        store.as_context_mut(),
        TableType::new(RefType::FUNCREF, spec.layout.table_initial, None),
        Ref::Func(None),
    )?;
    let stack_pointer = Global::new(
        store.as_context_mut(),
        GlobalType::new(ValType::I32, Mutability::Var),
        Val::I32(spec.layout.stack_pointer),
    )?;
    let module = EmModule {
        label: spec.label,
        memory,
        table,
        stack_pointer,
        instance: None,
        exports: HashMap::new(),
        symbols: HashMap::new(),
        got: Got::default(),
        dns: crate::netdb::Dns::default(),
        fs,
        env: Vec::new(),
        env_strings: None,
        this_program: spec.this_program.clone(),
        default_lang: spec.default_lang.clone(),
        no_exit_runtime: spec.no_exit_runtime,
        exit_status: 0,
        runtime_initialized: false,
        runtime_exited: false,
        aborted: false,
        heap_base: spec.layout.heap_base,
        table_map: None,
        free_slots: Vec::new(),
        callbacks: HashMap::new(),
        dsos_by_name: HashMap::new(),
        dsos_by_handle: HashMap::new(),
        dsos: Vec::new(),
        timers: HashMap::new(),
        io: ProtocolIo::default(),
        initdb: None,
    };
    store.as_context_mut().data_mut().modules.push(module);
    Ok(store.as_context().data().modules.len() - 1)
}

pub fn got_global(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
    weak: bool,
) -> HostResult<Global> {
    let existing = store.as_context_mut().data().modules[index]
        .got
        .get(name)
        .map(|(global, _)| *global);
    let global = if let Some(global) = existing {
        global
    } else {
        let global = Global::new(
            store.as_context_mut(),
            GlobalType::new(ValType::I32, Mutability::Var),
            Val::I32(0),
        )?;
        store.as_context_mut().data_mut().modules[index]
            .got
            .insert(name.to_owned(), (global, false));
        global
    };
    if !weak
        && let Some(entry) = store.as_context_mut().data_mut().modules[index]
            .got
            .get_mut(name)
    {
        entry.1 = true;
    }
    Ok(global)
}

/// Builds the import list for the main module the way `getWasmImports`
/// provides it, then instantiates and runs `createWasm`'s post steps.
pub fn instantiate_main(
    store: &mut wasmtime::Store<Runtime>,
    index: usize,
    module: &Module,
) -> HostResult<()> {
    let mut imports = Vec::new();
    for import in module.imports() {
        let name = import.name();
        let value = match (import.module(), import.ty()) {
            ("env", ExternType::Memory(_)) => {
                Extern::Memory(store.as_context().data().modules[index].memory)
            }
            ("env", ExternType::Table(_)) => {
                Extern::Table(store.as_context().data().modules[index].table)
            }
            ("env", ExternType::Global(_)) => match name {
                "__memory_base" => Extern::Global(Global::new(
                    store.as_context_mut(),
                    GlobalType::new(ValType::I32, Mutability::Const),
                    Val::I32(MEMORY_BASE),
                )?),
                "__table_base" => Extern::Global(Global::new(
                    store.as_context_mut(),
                    GlobalType::new(ValType::I32, Mutability::Const),
                    Val::I32(1),
                )?),
                "__stack_pointer" => {
                    Extern::Global(store.as_context().data().modules[index].stack_pointer)
                }
                other => return Err(abort_error(format!("unknown env global {other}"))),
            },
            ("GOT.mem" | "GOT.func", ExternType::Global(_)) => {
                Extern::Global(got_global(store, index, name, false)?)
            }
            (_, ExternType::Func(ty)) => {
                if name.starts_with("invoke_") {
                    let func = host_func(store, index, name, ty)?;
                    store.as_context_mut().data_mut().modules[index]
                        .symbols
                        .insert(name.to_owned(), Symbol::Func(func));
                    Extern::Func(func)
                } else {
                    Extern::Func(host_func_registered(store, index, name, ty)?)
                }
            }
            (namespace, _) => {
                return Err(abort_error(format!(
                    "unsupported import {namespace}.{name}"
                )));
            }
        };
        imports.push(value);
    }
    let instance = Instance::new(store.as_context_mut(), module, &imports)?;
    store.as_context_mut().data_mut().modules[index].instance = Some(instance);
    let exports = collect_exports(store, &instance, MEMORY_BASE);
    // relocateExports -> updateGOT(exports) for the main module.
    update_got(store, index, &exports, false)?;
    // mergeLibSymbols(wasmExports, "main")
    merge_lib_symbols(store, index, &exports);
    store.as_context_mut().data_mut().modules[index].exports = exports.iter().cloned().collect();
    // LDSO.init: newDSO("__main__", 0, wasmImports); loadDylibs() with no
    // needed libraries runs reportUndefinedSymbols.
    report_undefined_symbols(store, index)?;
    Ok(())
}

/// The ordered relocated exports of an instance.
pub fn collect_exports(
    store: &mut wasmtime::Store<Runtime>,
    instance: &Instance,
    memory_base: i32,
) -> Vec<(String, Symbol)> {
    let mut names = Vec::new();
    for export in instance.exports(store.as_context_mut()) {
        names.push((export.name().to_owned(), export.into_extern()));
    }
    let mut exports = Vec::new();
    for (name, value) in names {
        let symbol = match value {
            Extern::Func(func) => Symbol::Func(func),
            Extern::Global(global) => match global.get(store.as_context_mut()) {
                Val::I32(value) => Symbol::Data(value.wrapping_add(memory_base)),
                _ => continue,
            },
            _ => continue,
        };
        exports.push((name, symbol));
    }
    exports
}

fn is_internal_symbol(name: &str) -> bool {
    matches!(
        name,
        "__cpp_exception"
            | "__c_longjmp"
            | "__wasm_apply_data_relocs"
            | "__dso_handle"
            | "__tls_size"
            | "__tls_align"
            | "__set_stack_limits"
            | "_emscripten_tls_init"
            | "__wasm_init_tls"
            | "__wasm_call_ctors"
            | "__start_em_asm"
            | "__stop_em_asm"
            | "__start_em_js"
            | "__stop_em_js"
    ) || name.starts_with("__em_js__")
}

/// `updateGOT(exports, replace)`.
pub fn update_got(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    exports: &[(String, Symbol)],
    replace: bool,
) -> HostResult<()> {
    for (name, symbol) in exports {
        if is_internal_symbol(name) {
            continue;
        }
        let global =
            if let Some((global, _)) = store.as_context().data().modules[index].got.get(name) {
                *global
            } else {
                let global = Global::new(
                    store.as_context_mut(),
                    GlobalType::new(ValType::I32, Mutability::Var),
                    Val::I32(0),
                )?;
                store.as_context_mut().data_mut().modules[index]
                    .got
                    .insert(name.clone(), (global, false));
                global
            };
        let current = global.get(store.as_context_mut()).i32().unwrap_or(0);
        if replace || current == 0 {
            let value = match symbol {
                Symbol::Func(func) => add_function(store, index, *func)?,
                Symbol::Data(address) => *address,
            };
            global.set(store.as_context_mut(), Val::I32(value))?;
        }
    }
    Ok(())
}

/// `mergeLibSymbols`.
pub fn merge_lib_symbols(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    exports: &[(String, Symbol)],
) {
    let mut context = store.as_context_mut();
    let module = &mut context.data_mut().modules[index];
    for (name, symbol) in exports {
        module
            .symbols
            .entry(name.clone())
            .or_insert_with(|| symbol.clone());
        if name == "main" {
            module
                .symbols
                .entry("__main_argc_argv".to_owned())
                .or_insert_with(|| symbol.clone());
        }
        if name == "__main_argc_argv" {
            module
                .symbols
                .entry("main".to_owned())
                .or_insert_with(|| symbol.clone());
        }
    }
}

/// `reportUndefinedSymbols`.
pub fn report_undefined_symbols(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
) -> HostResult<()> {
    let entries = store.as_context_mut().data().modules[index].got.ordered();
    for (name, global, required) in entries {
        if global.get(store.as_context_mut()).i32().unwrap_or(0) != 0 {
            continue;
        }
        let symbol = resolve_global_symbol(store, index, &name)?;
        match symbol {
            Some(Symbol::Func(func)) => {
                let slot = add_function(store, index, func)?;
                global.set(store.as_context_mut(), Val::I32(slot))?;
            }
            Some(Symbol::Data(address)) => global.set(store.as_context_mut(), Val::I32(address))?,
            None if !required => {}
            None => {
                return Err(abort_error(format!(
                    "bad export type for '{name}': undefined"
                )));
            }
        }
    }
    Ok(())
}

/// `resolveGlobalSymbol`: the namespace value, or a new invoke wrapper.
pub fn resolve_global_symbol(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
) -> HostResult<Option<Symbol>> {
    if name == "__heap_base" {
        // wasmImports.__heap_base is the number ___heap_base.
        return Ok(Some(Symbol::Data(
            store.as_context().data().modules[index].heap_base,
        )));
    }
    if let Some(symbol) = store.as_context().data().modules[index].symbols.get(name) {
        return Ok(Some(symbol.clone()));
    }
    if let Some(host) = host_symbol(store, index, name)? {
        return Ok(Some(host));
    }
    if let Some(signature) = name.strip_prefix("invoke_") {
        let ty = invoke_type(&store.as_context().data().engine, signature)?;
        let func = host_func(store, index, name, ty)?;
        store.as_context_mut().data_mut().modules[index]
            .symbols
            .insert(name.to_owned(), Symbol::Func(func));
        return Ok(Some(Symbol::Func(func)));
    }
    Ok(None)
}

/// A host import function by name, when the glue defines one.
fn host_symbol(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
) -> HostResult<Option<Symbol>> {
    let Some(ty) = crate::syscalls::host_signature(&store.as_context().data().engine, name) else {
        return Ok(None);
    };
    let func = host_func(store, index, name, ty)?;
    store.as_context_mut().data_mut().modules[index]
        .symbols
        .insert(name.to_owned(), Symbol::Func(func));
    Ok(Some(Symbol::Func(func)))
}

fn invoke_type(engine: &Engine, signature: &str) -> HostResult<FuncType> {
    let to_type = |character: char| match character {
        'i' | 'p' => Ok(ValType::I32),
        'j' => Ok(ValType::I64),
        'f' => Ok(ValType::F32),
        'd' => Ok(ValType::F64),
        other => Err(abort_error(format!("bad signature character {other}"))),
    };
    let mut characters = signature.chars();
    let result = characters
        .next()
        .ok_or_else(|| abort_error("empty invoke signature"))?;
    let mut params = vec![ValType::I32];
    for character in characters {
        params.push(to_type(character)?);
    }
    let results = if result == 'v' {
        Vec::new()
    } else {
        vec![to_type(result)?]
    };
    Ok(FuncType::new(engine, params, results))
}

/// Wraps a host import as a Wasm function.
pub fn host_func(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
    ty: FuncType,
) -> HostResult<Func> {
    let host = crate::syscalls::HostFn::from_name(name)
        .ok_or_else(|| abort_error(format!("missing import {name}")))?;
    Ok(Func::new(
        store.as_context_mut(),
        ty,
        move |caller, params, results| {
            crate::syscalls::dispatch(caller, index, host, params, results)
        },
    ))
}

/// A host import registered in the `wasmImports` namespace, created on
/// first use with the importer's type.
pub fn host_func_registered(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
    ty: FuncType,
) -> HostResult<Func> {
    if let Some(Symbol::Func(func)) = store.as_context().data().modules[index].symbols.get(name) {
        return Ok(*func);
    }
    let func = host_func(store, index, name, ty)?;
    store.as_context_mut().data_mut().modules[index]
        .symbols
        .insert(name.to_owned(), Symbol::Func(func));
    Ok(func)
}

/// `getFunctionAddress(func)`: the table slot holding `func`, or 0.
pub fn function_address(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    func: &Func,
) -> i32 {
    let map = table_map(store, index);
    let key = func.to_raw(store.as_context_mut()) as usize;
    let slot = map.get(&key).copied().unwrap_or(0);
    store.as_context_mut().data_mut().modules[index].table_map = Some(map);
    slot.cast_signed()
}

/// Places a callback in the table (`addFunction` of a JavaScript function).
pub fn add_callback(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    callback: Callback,
    signature: &str,
) -> HostResult<i32> {
    let engine = store.as_context().data().engine.clone();
    let to_type = |character: char| match character {
        'j' => ValType::I64,
        'f' => ValType::F32,
        'd' => ValType::F64,
        _ => ValType::I32,
    };
    let mut characters = signature.chars();
    let result = characters.next().unwrap_or('v');
    let params: Vec<ValType> = characters.map(to_type).collect();
    let results = if result == 'v' {
        Vec::new()
    } else {
        vec![to_type(result)]
    };
    let ty = FuncType::new(&engine, params, results);
    let func = Func::new(
        store.as_context_mut(),
        ty,
        move |caller, params, results| {
            crate::pglite::run_callback(caller, index, callback, params, results)
        },
    );
    let slot = add_function(store, index, func)?;
    store.as_context_mut().data_mut().modules[index]
        .callbacks
        .insert(slot.cast_unsigned(), callback);
    Ok(slot)
}

fn table_map(store: &mut impl AsContextMut<Data = Runtime>, index: usize) -> HashMap<usize, u32> {
    if let Some(map) = store.as_context_mut().data_mut().modules[index]
        .table_map
        .take()
    {
        return map;
    }
    let table = store.as_context_mut().data().modules[index].table;
    let length = table.size(store.as_context_mut());
    let mut map = HashMap::new();
    for slot in 0..length {
        if let Some(Ref::Func(Some(func))) = table.get(store.as_context_mut(), slot) {
            map.insert(
                func.to_raw(store.as_context_mut()) as usize,
                u32::try_from(slot).unwrap_or(0),
            );
        }
    }
    map
}

/// `updateTableMap(offset, count)` after a side module instantiates.
pub fn update_table_map(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    offset: u32,
    count: u32,
) {
    if store.as_context_mut().data().modules[index]
        .table_map
        .is_none()
    {
        // functionsInTableMap is still lazy; the first lookup scans it all.
        return;
    }
    let mut map = table_map(store, index);
    let table = store.as_context_mut().data().modules[index].table;
    for slot in offset..offset + count {
        if let Some(Ref::Func(Some(func))) = table.get(store.as_context_mut(), u64::from(slot)) {
            map.insert(func.to_raw(store.as_context_mut()) as usize, slot);
        }
    }
    store.as_context_mut().data_mut().modules[index].table_map = Some(map);
}

/// `addFunction(func)`: reuse the slot of a function already in the table,
/// otherwise take a free slot or grow by one.
pub fn add_function(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    func: Func,
) -> HostResult<i32> {
    let mut map = table_map(store, index);
    let key = func.to_raw(store.as_context_mut()) as usize;
    if let Some(slot) = map.get(&key).copied().filter(|slot| *slot != 0) {
        store.as_context_mut().data_mut().modules[index].table_map = Some(map);
        return Ok(slot.cast_signed());
    }
    let table = store.as_context_mut().data().modules[index].table;
    let slot = if let Some(slot) = store.as_context_mut().data_mut().modules[index]
        .free_slots
        .pop()
    {
        slot
    } else {
        let previous = table.grow(store.as_context_mut(), 1, Ref::Func(None))?;
        u32::try_from(previous).unwrap_or(0)
    };
    table.set(
        store.as_context_mut(),
        u64::from(slot),
        Ref::Func(Some(func)),
    )?;
    map.insert(key, slot);
    store.as_context_mut().data_mut().modules[index].table_map = Some(map);
    Ok(slot.cast_signed())
}

/// `removeFunction(index)`.
pub fn remove_function(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    slot: i32,
) -> HostResult<()> {
    let table = store.as_context().data().modules[index].table;
    let slot_index = u64::from(slot.cast_unsigned());
    if let Some(Ref::Func(Some(func))) = table.get(store.as_context_mut(), slot_index) {
        let key = func.to_raw(store.as_context_mut()) as usize;
        let mut map = table_map(store, index);
        map.remove(&key);
        store.as_context_mut().data_mut().modules[index].table_map = Some(map);
    }
    table.set(store.as_context_mut(), slot_index, Ref::Func(None))?;
    let mut context = store.as_context_mut();
    let module = &mut context.data_mut().modules[index];
    module.free_slots.push(slot.cast_unsigned());
    module.callbacks.remove(&slot.cast_unsigned());
    Ok(())
}

// ---------------------------------------------------------------------------
// Calling exports
// ---------------------------------------------------------------------------

pub fn export_func(runtime: &Runtime, index: usize, name: &str) -> HostResult<Func> {
    match runtime.modules[index].exports.get(name) {
        Some(Symbol::Func(func)) => Ok(*func),
        _ => Err(abort_error(format!(
            "{} has no exported function {name}",
            runtime.modules[index].label
        ))),
    }
}

/// Calls an export and returns its single `i32` result, or 0.
pub fn call_i32(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    name: &str,
    params: &[Val],
) -> HostResult<i32> {
    let func = export_func(store.as_context_mut().data(), index, name)?;
    let ty = func.ty(store.as_context_mut());
    let mut results: Vec<Val> = ty.results().map(|kind| zero_value(&kind)).collect();
    func.call(store.as_context_mut(), params, &mut results)?;
    Ok(results.first().and_then(Val::i32).unwrap_or(0))
}

#[must_use]
pub fn zero_value(kind: &ValType) -> Val {
    match kind {
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        _ => Val::I32(0),
    }
}

pub fn stack_save(store: &mut impl AsContextMut<Data = Runtime>, index: usize) -> HostResult<i32> {
    call_i32(store, index, "emscripten_stack_get_current", &[])
}

pub fn stack_restore(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    value: i32,
) -> HostResult<()> {
    call_i32(
        store,
        index,
        "_emscripten_stack_restore",
        &[Val::I32(value)],
    )
    .map(|_| ())
}

pub fn stack_alloc(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    size: i32,
) -> HostResult<i32> {
    call_i32(store, index, "_emscripten_stack_alloc", &[Val::I32(size)])
}

/// `stringToUTF8OnStack`.
pub fn string_on_stack(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    text: &str,
) -> HostResult<i32> {
    let length = i32::try_from(text.len() + 1).unwrap_or(i32::MAX);
    let address = stack_alloc(store, index, length)?;
    let memory = store.as_context_mut().data().modules[index].memory;
    string_to_utf8(
        memory.data_mut(store.as_context_mut()),
        text,
        address.cast_unsigned(),
        text.len() + 1,
    );
    Ok(address)
}

/// `callMain(args)`.
pub fn call_main(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    args: &[String],
) -> HostResult<i32> {
    let main = match store.as_context().data().modules[index].symbols.get("main") {
        Some(Symbol::Func(func)) => *func,
        _ => return Err(abort_error("main is not defined")),
    };
    let mut argv_strings = vec![
        store.as_context().data().modules[index]
            .this_program
            .clone(),
    ];
    argv_strings.extend(args.iter().cloned());
    let argument_count = i32::try_from(argv_strings.len()).unwrap_or(0);
    let argument_vector = stack_alloc(store, index, (argument_count + 1) * 4)?;
    let mut cursor = argument_vector.cast_unsigned();
    for argument in &argv_strings {
        let pointer = string_on_stack(store, index, argument)?;
        let memory = store.as_context().data().modules[index].memory;
        write_i32(memory.data_mut(store.as_context_mut()), cursor, pointer);
        cursor += 4;
    }
    let memory = store.as_context().data().modules[index].memory;
    write_i32(memory.data_mut(store.as_context_mut()), cursor, 0);
    let mut results = [Val::I32(0)];
    let outcome = main
        .call(
            store.as_context_mut(),
            &[Val::I32(argument_count), Val::I32(argument_vector)],
            &mut results,
        )
        .and_then(|()| {
            let status = results[0].i32().unwrap_or(0);
            exit_js(store, index, status)
        });
    match outcome {
        Ok(()) => Ok(store.as_context().data().modules[index].exit_status),
        Err(error) if error.is::<ExitStatus>() => {
            Ok(store.as_context().data().modules[index].exit_status)
        }
        Err(error) => Err(error),
    }
}

fn keep_runtime_alive(module: &EmModule) -> bool {
    module.no_exit_runtime
}

/// `exitJS(status)`: always ends by raising `ExitStatus`.
pub fn exit_js(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    status: i32,
) -> HostResult<()> {
    store.as_context_mut().data_mut().modules[index].exit_status = status;
    if !keep_runtime_alive(&store.as_context_mut().data().modules[index]) {
        exit_runtime(store, index)?;
    }
    proc_exit(store, index, status)
}

/// `_proc_exit`.
pub fn proc_exit(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    status: i32,
) -> HostResult<()> {
    let mut context = store.as_context_mut();
    let module = &mut context.data_mut().modules[index];
    module.exit_status = status;
    if !keep_runtime_alive(module) {
        module.aborted = true;
    }
    Err(wasmtime::Error::new(ExitStatus(status)))
}

/// `exitRuntime()`.
pub fn exit_runtime(store: &mut impl AsContextMut<Data = Runtime>, index: usize) -> HostResult<()> {
    call_i32(store, index, "__funcs_on_exit", &[])?;
    fs_quit(store, index)?;
    store.as_context_mut().data_mut().modules[index].runtime_exited = true;
    Ok(())
}

/// `FS.quit()`: `_fflush(0)` then close every stream.
pub fn fs_quit(store: &mut impl AsContextMut<Data = Runtime>, index: usize) -> HostResult<()> {
    call_i32(store, index, "fflush", &[Val::I32(0)])?;
    let fs = Rc::clone(&store.as_context_mut().data().modules[index].fs);
    fs.borrow_mut().quit_streams();
    Ok(())
}

/// `initRuntime()` for a freshly instantiated main module.
pub fn init_runtime(store: &mut wasmtime::Store<Runtime>, index: usize) -> HostResult<()> {
    store.as_context_mut().data_mut().modules[index].runtime_initialized = true;
    call_i32(store, index, "__wasm_apply_data_relocs", &[])?;
    let fs = Rc::clone(&store.as_context().data().modules[index].fs);
    {
        let mut fs = fs.borrow_mut();
        if !fs.initialized {
            fs.init_standard_streams().map_err(fs_abort)?;
        }
        fs.init_runtime_mounts().map_err(fs_abort)?;
    }
    call_i32(store, index, "__wasm_call_ctors", &[])?;
    Ok(())
}

pub fn fs_abort(error: FsError) -> wasmtime::Error {
    match error {
        FsError::Errno(code) => abort_error(format!("ErrnoError {code}")),
        FsError::Fatal(message) => abort_error(message),
    }
}

/// The glue's `newSize` for one `cutDown` step of `_emscripten_resize_heap`:
/// `Math.min(maxHeapSize, alignMemory(Math.max(requestedSize,
/// Math.min(oldSize * (1 + .2 / cutDown), requestedSize + 100663296)), 65536))`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the glue computes in doubles and aligns before any rounding; sizes are at most 2^31, so the doubles are exact and the result is a non-negative integer"
)]
fn heap_target(old_size: u64, requested: u64, cut_down: u64) -> u64 {
    let over_grown =
        (old_size as f64 * (1.0 + 0.2 / cut_down as f64)).min(requested as f64 + 100_663_296.0);
    let aligned = ((requested as f64).max(over_grown) / 65536.0).ceil() * 65536.0;
    (HEAP_MAX as f64).min(aligned) as u64
}

/// `emscripten_resize_heap(requested)`.
pub fn resize_heap(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    requested: u32,
) -> bool {
    let memory = store.as_context_mut().data().modules[index].memory;
    let old_size = memory.data_size(store.as_context_mut()) as u64;
    let requested = u64::from(requested);
    if requested > HEAP_MAX {
        return false;
    }
    let mut cut_down = 1_u64;
    while cut_down <= 4 {
        let new_size = heap_target(old_size, requested, cut_down);
        let current = memory.data_size(store.as_context_mut()) as u64;
        let pages = new_size.saturating_sub(current).div_ceil(65536);
        if memory.grow(store.as_context_mut(), pages).is_ok() {
            return true;
        }
        cut_down *= 2;
    }
    false
}

/// `getMemory(size)` after the runtime started: `_calloc(size, 1)`.
pub fn get_memory(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    size: i32,
) -> HostResult<i32> {
    if store.as_context_mut().data().modules[index].runtime_initialized {
        return call_i32(store, index, "calloc", &[Val::I32(size), Val::I32(1)]);
    }
    let mut context = store.as_context_mut();
    let module = &mut context.data_mut().modules[index];
    let start = module.heap_base;
    let aligned = (size + 15) / 16 * 16;
    module.heap_base = start + aligned;
    Ok(start)
}

/// `mmapAlloc(size)`.
pub fn mmap_alloc(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    size: u32,
) -> HostResult<i32> {
    let aligned = size.div_ceil(65536) * 65536;
    let address = call_i32(
        store,
        index,
        "emscripten_builtin_memalign",
        &[Val::I32(65536), Val::I32(aligned.cast_signed())],
    )?;
    if address != 0 {
        let memory = store.as_context_mut().data().modules[index].memory;
        let data = memory.data_mut(store.as_context_mut());
        let start = address.cast_unsigned() as usize;
        if let Some(slot) = data.get_mut(start..start + aligned as usize) {
            slot.fill(0);
        }
    }
    Ok(address)
}

/// `dlSetError(message)`.
pub fn dl_set_error(
    store: &mut impl AsContextMut<Data = Runtime>,
    index: usize,
    message: &str,
) -> HostResult<()> {
    let saved = stack_save(store, index)?;
    let pointer = string_on_stack(store, index, message)?;
    call_i32(
        store,
        index,
        "__dl_seterr",
        &[Val::I32(pointer), Val::I32(0)],
    )?;
    stack_restore(store, index, saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_target_aligns_the_double_like_the_glue() {
        // Values from the glue's arithmetic in Node: the overgrown size is
        // 3604480.0000000005, which alignMemory rounds up a whole page.
        assert_eq!(heap_target(3_276_800, 3_276_801, 2), 3_670_016);
        assert_eq!(heap_target(3_276_800, 3_276_801, 1), 3_932_160);
        assert_eq!(heap_target(2_147_418_112, 2_147_418_113, 1), HEAP_MAX);
    }
}
