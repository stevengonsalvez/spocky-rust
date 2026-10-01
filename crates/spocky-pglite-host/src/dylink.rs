//! Side-module loading: `_dlopen_js`, `_dlsym_js` and the glue's
//! `loadDynamicLibrary` / `loadWebAssemblyModule`.

use std::sync::Arc;

use wasmparser::{Dylink0Subsection, KnownCustom, Parser, Payload, SymbolFlags};
use wasmtime::{
    AsContextMut, Caller, Extern, ExternType, Func, Global, GlobalType, Instance, Module,
    Mutability, Ref, Val, ValType,
};

use crate::runtime::{
    self, Dso, Runtime, Symbol, abort_error, c_string, read_i32, read_u8, read_u32, write_i32,
    write_u8, write_u32,
};
use crate::vfs;

const RTLD_GLOBAL: i32 = 256;

#[derive(Debug, Default)]
struct DylinkInfo {
    memory_size: u32,
    memory_align: u32,
    table_size: u32,
    needed: Vec<String>,
    weak_imports: Vec<String>,
}

fn dylink_info(bytes: &[u8]) -> wasmtime::Result<DylinkInfo> {
    let mut info = DylinkInfo::default();
    let mut found = false;
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|error| abort_error(error.to_string()))?;
        let Payload::CustomSection(section) = payload else {
            continue;
        };
        let KnownCustom::Dylink0(reader) = section.as_known() else {
            continue;
        };
        found = true;
        for subsection in reader {
            match subsection.map_err(|error| abort_error(error.to_string()))? {
                Dylink0Subsection::MemInfo(memory) => {
                    info.memory_size = memory.memory_size;
                    info.memory_align = memory.memory_alignment;
                    info.table_size = memory.table_size;
                }
                Dylink0Subsection::Needed(needed) => {
                    info.needed = needed.iter().map(|name| (*name).to_owned()).collect();
                }
                Dylink0Subsection::ImportInfo(imports) => {
                    for import in imports {
                        if import.flags.contains(SymbolFlags::BINDING_WEAK) {
                            info.weak_imports.push(import.field.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        break;
    }
    if !found {
        return Err(abort_error("need dylink section"));
    }
    Ok(info)
}

/// `_dlopen_js(handle)`: 1 on success, 0 after `dlSetError`.
pub fn dlopen(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    handle: i32,
) -> wasmtime::Result<i32> {
    caller.data_mut().count("dlopen");
    let memory = caller.data().modules[index].memory;
    let data = memory.data(&*caller);
    let name = vfs::path_normalize(&c_string(data, handle.cast_unsigned() + 36, None));
    let flags = read_i32(data, handle.cast_unsigned() + 4);
    let global = flags & RTLD_GLOBAL != 0;
    match load_dynamic_library(caller, index, &name, global, handle) {
        Ok(()) => Ok(1),
        Err(error) => {
            if error.is::<runtime::ExitStatus>() || error.is::<runtime::Longjmp>() {
                return Err(error);
            }
            runtime::dl_set_error(
                caller,
                index,
                &format!("Could not load dynamic lib: {name}\nError: {error}"),
            )?;
            Ok(0)
        }
    }
}

fn load_dynamic_library(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    name: &str,
    global: bool,
    handle: i32,
) -> wasmtime::Result<()> {
    if let Some(&dso) = caller.data().modules[index].dsos_by_name.get(name) {
        if global && !caller.data().modules[index].dsos[dso].global {
            caller.data_mut().modules[index].dsos[dso].global = true;
            let exports = caller.data().modules[index].dsos[dso].exports.clone();
            merge_symbols(caller.data_mut(), index, &exports);
        }
        caller.data_mut().modules[index]
            .dsos_by_handle
            .insert(handle, dso);
        return Ok(());
    }
    // newDSO(name, handle, "loading")
    let dso = caller.data().modules[index].dsos.len();
    {
        let module = &mut caller.data_mut().modules[index];
        module.dsos.push(Dso {
            exports: Vec::new(),
            global,
        });
        module.dsos_by_name.insert(name.to_owned(), dso);
        module.dsos_by_handle.insert(handle, dso);
    }
    let memory = caller.data().modules[index].memory;
    let data = memory.data(&*caller);
    let file_data = read_u32(data, handle.cast_unsigned() + 28) as usize;
    let file_size = read_u32(data, handle.cast_unsigned() + 32) as usize;
    if file_data == 0 || file_size == 0 {
        return Err(abort_error(format!(
            "ENOENT: no such file or directory, open '{name}'"
        )));
    }
    let bytes = data
        .get(file_data..file_data + file_size)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| abort_error("library bytes outside memory"))?;
    let exports = load_webassembly_module(caller, index, &bytes, dso, handle)?;
    if global {
        merge_symbols(caller.data_mut(), index, &exports);
    }
    caller.data_mut().modules[index].dsos[dso].exports = exports;
    Ok(())
}

fn merge_symbols(runtime: &mut Runtime, index: usize, exports: &[(String, Symbol)]) {
    let symbols = &mut runtime.modules[index].symbols;
    for (name, symbol) in exports {
        symbols
            .entry(name.clone())
            .or_insert_with(|| symbol.clone());
    }
}

#[allow(clippy::too_many_lines)]
fn load_webassembly_module(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    bytes: &[u8],
    dso: usize,
    handle: i32,
) -> wasmtime::Result<Vec<(String, Symbol)>> {
    let info = dylink_info(bytes)?;
    if !info.needed.is_empty() {
        return Err(abort_error(format!(
            "needed libraries are not supported: {:?}",
            info.needed
        )));
    }
    let memory = caller.data().modules[index].memory;
    let table = caller.data().modules[index].table;
    let handle_address = handle.cast_unsigned();
    let first_load = handle == 0 || read_u8(memory.data(&*caller), handle_address + 8) == 0;
    let (memory_base, table_base) = if first_load {
        let alignment = 1_i32 << info.memory_align;
        let memory_base = if info.memory_size == 0 {
            0
        } else {
            let size = i32::try_from(info.memory_size).unwrap_or(i32::MAX) + alignment;
            let start = runtime::get_memory(caller, index, size)?;
            (start + alignment - 1) / alignment * alignment
        };
        let table_base = if info.table_size == 0 {
            0
        } else {
            u32::try_from(table.size(&mut *caller)).unwrap_or(0)
        };
        if handle != 0 {
            let data = memory.data_mut(&mut *caller);
            write_u8(data, handle_address + 8, 1);
            write_u32(data, handle_address + 12, memory_base.cast_unsigned());
            write_i32(
                data,
                handle_address + 16,
                i32::try_from(info.memory_size).unwrap_or(0),
            );
            write_u32(data, handle_address + 20, table_base);
            write_i32(
                data,
                handle_address + 24,
                i32::try_from(info.table_size).unwrap_or(0),
            );
        }
        (memory_base, table_base)
    } else {
        let data = memory.data(&*caller);
        (
            read_i32(data, handle_address + 12),
            read_u32(data, handle_address + 20),
        )
    };
    let current = u32::try_from(table.size(&mut *caller)).unwrap_or(0);
    let needed = table_base + info.table_size;
    if needed > current {
        table.grow(&mut *caller, u64::from(needed - current), Ref::Func(None))?;
    }
    let engine = caller.data().engine.clone();
    let module = Module::new(&engine, bytes)?;
    let mut imports = Vec::new();
    for import in module.imports() {
        let name = import.name();
        let value = match (import.module(), import.ty()) {
            (_, ExternType::Memory(_)) => Extern::Memory(memory),
            (_, ExternType::Table(_)) => Extern::Table(table),
            ("env" | "wasi_snapshot_preview1", ExternType::Global(_)) => match name {
                "__memory_base" => Extern::Global(Global::new(
                    &mut *caller,
                    GlobalType::new(ValType::I32, Mutability::Const),
                    Val::I32(memory_base),
                )?),
                "__table_base" => Extern::Global(Global::new(
                    &mut *caller,
                    GlobalType::new(ValType::I32, Mutability::Const),
                    Val::I32(table_base.cast_signed()),
                )?),
                "__stack_pointer" => Extern::Global(caller.data().modules[index].stack_pointer),
                other => return Err(abort_error(format!("unsupported env global {other}"))),
            },
            ("GOT.mem" | "GOT.func", ExternType::Global(_)) => {
                let weak = info.weak_imports.iter().any(|weak| weak == name);
                Extern::Global(runtime::got_global(caller, index, name, weak)?)
            }
            (_, ExternType::Func(ty)) => {
                let resolved = match caller.data().modules[index].symbols.get(name) {
                    Some(Symbol::Func(func)) => Some(func.clone()),
                    _ => None,
                };
                let resolved = match resolved {
                    Some(func) => Some(func),
                    None if crate::syscalls::HostFn::from_name(name).is_some()
                        && !name.starts_with("invoke_") =>
                    {
                        Some(runtime::host_func_registered(
                            caller,
                            index,
                            name,
                            ty.clone(),
                        )?)
                    }
                    None => None,
                };
                match resolved {
                    Some(func) => Extern::Func(func),
                    None => Extern::Func(lazy_stub(caller, index, dso, name, ty)),
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
    let instance = Instance::new(&mut *caller, &module, &imports)?;
    runtime::update_table_map(caller, index, table_base, info.table_size)?;
    let exports = collect_exports(caller, &instance, memory_base);
    runtime::update_got(caller, index, &exports, false)?;
    runtime::report_undefined_symbols(caller, index)?;
    if exports
        .iter()
        .any(|(name, _)| name == "__start_em_asm" || name.starts_with("__em_js__"))
    {
        return Err(abort_error(
            "side module uses EM_ASM or EM_JS, which needs eval",
        ));
    }
    for name in ["__wasm_apply_data_relocs", "__wasm_call_ctors"] {
        if let Some((_, Symbol::Func(func))) = exports.iter().find(|(export, _)| export == name) {
            func.call(&mut *caller, &[], &mut [])?;
        }
    }
    Ok(exports)
}

fn collect_exports(
    caller: &mut Caller<'_, Runtime>,
    instance: &Instance,
    memory_base: i32,
) -> Vec<(String, Symbol)> {
    let mut found = Vec::new();
    for export in instance.exports(&mut *caller) {
        found.push((export.name().to_owned(), export.into_extern()));
    }
    let mut exports = Vec::new();
    for (name, value) in found {
        let symbol = match value {
            Extern::Func(func) => Symbol::Func(func),
            Extern::Global(global) => match global.get(&mut *caller) {
                Val::I32(value) => Symbol::Data(value.wrapping_add(memory_base)),
                _ => continue,
            },
            _ => continue,
        };
        exports.push((name, symbol));
    }
    exports
}

/// The glue's proxy stub: resolve the symbol on first call, then call it.
fn lazy_stub(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    dso: usize,
    name: &str,
    ty: wasmtime::FuncType,
) -> Func {
    let name: Arc<str> = Arc::from(name);
    Func::new(&mut *caller, ty, move |mut caller, params, results| {
        let target = resolve_symbol(&mut caller, index, dso, &name)?;
        target.call(&mut caller, params, results)
    })
}

fn resolve_symbol(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    dso: usize,
    name: &str,
) -> wasmtime::Result<Func> {
    if let Some(Symbol::Func(func)) = runtime::resolve_global_symbol(caller, index, name)? {
        return Ok(func);
    }
    let own = caller.data().modules[index].dsos[dso]
        .exports
        .iter()
        .find(|(export, _)| export == name)
        .map(|(_, symbol)| symbol.clone());
    match own {
        Some(Symbol::Func(func)) => Ok(func),
        _ => Err(abort_error(format!("TypeError: {name} is not a function"))),
    }
}

/// `_dlsym_js(handle, symbol, symbolIndex)`.
pub fn dlsym(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    handle: i32,
    symbol: u32,
    symbol_index: u32,
) -> wasmtime::Result<i32> {
    caller.data_mut().count("dlsym");
    let memory = caller.data().modules[index].memory;
    let name = c_string(memory.data(&*caller), symbol, None);
    let Some(&dso) = caller.data().modules[index].dsos_by_handle.get(&handle) else {
        return Err(abort_error(
            "TypeError: Cannot read properties of undefined (reading 'exports')",
        ));
    };
    let library = caller.data().modules[index].dsos[dso]
        .exports
        .iter()
        .position(|(export, _)| *export == name);
    let Some(position) = library else {
        runtime::dl_set_error(
            caller,
            index,
            &format!(
                "Tried to lookup unknown symbol \"{name}\" in dynamic lib: {}",
                dso_name(caller.data(), index, dso)
            ),
        )?;
        return Ok(0);
    };
    let symbol = caller.data().modules[index].dsos[dso].exports[position]
        .1
        .clone();
    match symbol {
        Symbol::Data(address) => Ok(address),
        Symbol::Func(func) => {
            let existing = runtime::function_address(caller, index, &func)?;
            if existing != 0 {
                return Ok(existing);
            }
            let slot = runtime::add_function(caller, index, func)?;
            write_u32(
                memory.data_mut(&mut *caller),
                symbol_index,
                u32::try_from(position).unwrap_or(0),
            );
            Ok(slot)
        }
    }
}

fn dso_name(runtime: &Runtime, index: usize, dso: usize) -> String {
    runtime.modules[index]
        .dsos_by_name
        .iter()
        .find(|(_, value)| **value == dso)
        .map(|(name, _)| name.clone())
        .unwrap_or_default()
}

#[allow(dead_code)]
fn context_type(_: &mut impl AsContextMut<Data = Runtime>) {}
