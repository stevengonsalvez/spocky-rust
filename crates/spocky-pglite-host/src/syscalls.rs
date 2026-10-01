//! Host implementations of the `env` and `wasi_snapshot_preview1` imports.
//!
//! Each arm ports the glue function of the same name. Syscalls return the
//! negated Emscripten errno; WASI functions return it positive, as the glue
//! does.

use std::rc::Rc;

use wasmtime::{Caller, Engine, FuncType, Ref, Val, ValType};

use crate::runtime::{
    self, Abort, ExitStatus, Longjmp, Runtime, abort_error, c_string, read_i32, read_i64, read_u32,
    string_to_utf8, to_int32, write_bytes, write_i16, write_i32, write_i64, write_u8, write_u32,
    zero_value,
};
use crate::vfs::{self, Fs, FsError, FsResult};

macro_rules! host_fns {
    ($($variant:ident => $name:literal : $sig:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum HostFn {
            Invoke,
            $($variant),*
        }

        impl HostFn {
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                if name.starts_with("invoke_") {
                    return Some(Self::Invoke);
                }
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        /// The glue's `.sig` of a host function, for symbols a side module
        /// resolves through its GOT.
        #[must_use]
        pub fn host_signature(engine: &Engine, name: &str) -> Option<FuncType> {
            let signature = match name {
                $($name => $sig,)*
                _ => return None,
            };
            Some(signature_type(engine, signature))
        }
    };
}

host_fns! {
    AssertFail => "__assert_fail": "vppip",
    CallSighandler => "__call_sighandler": "vpi",
    Newselect => "__syscall__newselect": "iipppp",
    Accept4 => "__syscall_accept4": "iippiii",
    Bind => "__syscall_bind": "iippiii",
    Chdir => "__syscall_chdir": "ip",
    Chmod => "__syscall_chmod": "ipi",
    Connect => "__syscall_connect": "iippiii",
    Dup => "__syscall_dup": "ii",
    Dup3 => "__syscall_dup3": "iiii",
    Faccessat => "__syscall_faccessat": "iipii",
    Fadvise64 => "__syscall_fadvise64": "iijji",
    Fallocate => "__syscall_fallocate": "iiijj",
    Fchmod => "__syscall_fchmod": "iii",
    Fchmodat2 => "__syscall_fchmodat2": "iipii",
    Fchown32 => "__syscall_fchown32": "iiii",
    Fchownat => "__syscall_fchownat": "iipiii",
    Fcntl64 => "__syscall_fcntl64": "iiip",
    Fdatasync => "__syscall_fdatasync": "ii",
    Fstat64 => "__syscall_fstat64": "iip",
    Ftruncate64 => "__syscall_ftruncate64": "iij",
    Getcwd => "__syscall_getcwd": "ipp",
    Getdents64 => "__syscall_getdents64": "iipp",
    Ioctl => "__syscall_ioctl": "iiip",
    Listen => "__syscall_listen": "iiiiiii",
    Lstat64 => "__syscall_lstat64": "ipp",
    Mkdirat => "__syscall_mkdirat": "iipi",
    Newfstatat => "__syscall_newfstatat": "iippi",
    Openat => "__syscall_openat": "iipip",
    Pipe => "__syscall_pipe": "ip",
    Readlinkat => "__syscall_readlinkat": "iippp",
    Recvfrom => "__syscall_recvfrom": "iippipp",
    Renameat => "__syscall_renameat": "iipip",
    Rmdir => "__syscall_rmdir": "ip",
    Sendto => "__syscall_sendto": "iippipp",
    Socket => "__syscall_socket": "iiiiiii",
    Stat64 => "__syscall_stat64": "ipp",
    Statfs64 => "__syscall_statfs64": "ippp",
    Symlinkat => "__syscall_symlinkat": "ipip",
    Truncate64 => "__syscall_truncate64": "ipj",
    Unlinkat => "__syscall_unlinkat": "iipi",
    Utimensat => "__syscall_utimensat": "iippi",
    AbortJs => "_abort_js": "v",
    DlopenJs => "_dlopen_js": "pp",
    DlsymJs => "_dlsym_js": "pppp",
    KeepaliveClear => "_emscripten_runtime_keepalive_clear": "v",
    ThrowLongjmp => "_emscripten_throw_longjmp": "v",
    GmtimeJs => "_gmtime_js": "vjp",
    LocaltimeJs => "_localtime_js": "vjp",
    MktimeJs => "_mktime_js": "jp",
    MmapJs => "_mmap_js": "ipiiijpp",
    MunmapJs => "_munmap_js": "ippiiij",
    SetitimerJs => "_setitimer_js": "iid",
    TzsetJs => "_tzset_js": "vpppp",
    ClockTimeGet => "clock_time_get": "iijp",
    DateNow => "emscripten_date_now": "d",
    ForceExit => "emscripten_force_exit": "vi",
    GetHeapMax => "emscripten_get_heap_max": "p",
    GetNow => "emscripten_get_now": "d",
    ResizeHeap => "emscripten_resize_heap": "ip",
    EnvironGet => "environ_get": "ipp",
    EnvironSizesGet => "environ_sizes_get": "ipp",
    Exit => "exit": "vi",
    FdClose => "fd_close": "ii",
    FdFdstatGet => "fd_fdstat_get": "iip",
    FdPread => "fd_pread": "iippjp",
    FdPwrite => "fd_pwrite": "iippjp",
    FdRead => "fd_read": "iippp",
    FdSeek => "fd_seek": "iijip",
    FdSync => "fd_sync": "ii",
    FdWrite => "fd_write": "iippp",
    Getaddrinfo => "getaddrinfo": "ipppp",
    Getnameinfo => "getnameinfo": "ipipipii",
    ProcExit => "proc_exit": "vi",
    RandomGet => "random_get": "ipp",
    SchedYield => "sched_yield": "i",
    GetTempRet0 => "getTempRet0": "i",
    SetTempRet0 => "setTempRet0": "vi",
}

fn signature_type(engine: &Engine, signature: &str) -> FuncType {
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
    FuncType::new(engine, params, results)
}

fn arg_i32(params: &[Val], index: usize) -> i32 {
    params.get(index).and_then(Val::i32).unwrap_or(0)
}

fn arg_u32(params: &[Val], index: usize) -> u32 {
    arg_i32(params, index).cast_unsigned()
}

fn arg_i64(params: &[Val], index: usize) -> i64 {
    params.get(index).and_then(Val::i64).unwrap_or(0)
}

fn arg_f64(params: &[Val], index: usize) -> f64 {
    params.get(index).and_then(Val::f64).unwrap_or(0.0)
}

/// `bigintToI53Checked`.
fn i53(value: i64) -> Option<i64> {
    const LIMIT: i64 = 9_007_199_254_740_992;
    if (-LIMIT..=LIMIT).contains(&value) {
        Some(value)
    } else {
        None
    }
}

fn set_i32(results: &mut [Val], value: i32) {
    if let Some(slot) = results.first_mut() {
        *slot = Val::I32(value);
    }
}

/// The negated errno of a syscall failure; a non-errno failure aborts.
fn syscall_result(result: FsResult<i32>) -> wasmtime::Result<i32> {
    match result {
        Ok(value) => Ok(value),
        Err(FsError::Errno(code)) => Ok(-code),
        Err(FsError::Fatal(message)) => Err(abort_error(message)),
    }
}

/// The positive errno of a WASI failure.
fn wasi_result(result: FsResult<i32>) -> wasmtime::Result<i32> {
    match result {
        Ok(value) => Ok(value),
        Err(FsError::Errno(code)) => Ok(code),
        Err(FsError::Fatal(message)) => Err(abort_error(message)),
    }
}

struct Context<'a, 'b> {
    caller: &'a mut Caller<'b, Runtime>,
    index: usize,
}

impl Context<'_, '_> {
    fn fs(&self) -> Rc<std::cell::RefCell<Fs>> {
        Rc::clone(&self.caller.data().modules[self.index].fs)
    }

    fn memory(&mut self) -> &mut [u8] {
        let memory = self.caller.data().modules[self.index].memory;
        memory.data_mut(&mut *self.caller)
    }

    fn string(&mut self, address: u32) -> String {
        c_string(self.memory(), address, None)
    }

    /// `SYSCALLS.calculateAt`.
    fn calculate_at(&mut self, dirfd: i32, path: &str, allow_empty: bool) -> FsResult<String> {
        if vfs::path_is_abs(path) {
            return Ok(path.to_owned());
        }
        let fs = self.fs();
        let base = if dirfd == -100 {
            fs.borrow().cwd()
        } else {
            fs.borrow().get_stream_checked(dirfd)?.path.clone()
        };
        if path.is_empty() {
            if !allow_empty {
                return Err(FsError::Errno(vfs::ENOENT));
            }
            return Ok(base);
        }
        Ok(format!("{base}/{path}"))
    }

    /// `SYSCALLS.doStat`.
    fn do_stat(&mut self, stat: vfs::Stat, address: u32) {
        let memory = self.memory();
        write_i32(memory, address, to_int32_i64(stat.dev));
        write_u32(memory, address + 4, stat.mode);
        write_u32(memory, address + 8, stat.nlink);
        write_i32(memory, address + 12, to_int32_i64(stat.uid));
        write_i32(memory, address + 16, to_int32_i64(stat.gid));
        write_i32(memory, address + 20, to_int32_i64(stat.rdev));
        write_i64(memory, address + 24, stat.size);
        write_i32(memory, address + 32, 4096);
        write_i32(memory, address + 36, to_int32_i64(stat.blocks));
        for (offset, time) in [(40, stat.atime), (56, stat.mtime), (72, stat.ctime)] {
            let seconds = (time / 1000.0).floor();
            let remainder = time % 1000.0;
            #[allow(clippy::cast_possible_truncation)]
            write_i64(memory, address + offset, seconds as i64);
            write_i32(
                memory,
                address + offset + 8,
                to_int32(remainder * 1e3 * 1e3),
            );
        }
        write_i64(memory, address + 88, stat.ino.cast_signed());
    }
}

fn to_int32_i64(value: i64) -> i32 {
    // Exact for |value| < 2^53, the range of these JavaScript numbers.
    #[allow(clippy::cast_precision_loss)]
    to_int32(value as f64)
}

/// Entry point of every host import.
pub fn dispatch(
    mut caller: Caller<'_, Runtime>,
    index: usize,
    host: HostFn,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    if host == HostFn::Invoke {
        return invoke(&mut caller, index, params, results);
    }
    let mut context = Context {
        caller: &mut caller,
        index,
    };
    call(&mut context, host, params, results)
}

/// `invoke_*`: call through the table; a longjmp sets `__THREW__`.
fn invoke(
    caller: &mut Caller<'_, Runtime>,
    index: usize,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let saved = runtime::stack_save(caller, index)?;
    let table = caller.data().modules[index].table;
    let slot = u64::from(arg_u32(params, 0));
    let Some(Ref::Func(Some(func))) = table.get(&mut *caller, slot) else {
        runtime::stack_restore(caller, index, saved)?;
        return Err(abort_error(format!("table index {slot} is not a function")));
    };
    match func.call(&mut *caller, &params[1..], results) {
        Ok(()) => Ok(()),
        Err(error) => {
            runtime::stack_restore(caller, index, saved)?;
            if !error.is::<Longjmp>() {
                return Err(error);
            }
            caller.data_mut().count("invoke_caught_longjmp");
            runtime::call_i32(caller, index, "setThrew", &[Val::I32(1), Val::I32(0)])?;
            for slot in results.iter_mut() {
                // `undefined` converts to 0 for integers and NaN for floats;
                // the 'j' variants return 0n explicitly.
                let replacement = match slot {
                    Val::F32(_) => Val::F32(f32::NAN.to_bits()),
                    Val::F64(_) => Val::F64(f64::NAN.to_bits()),
                    Val::I64(_) => Val::I64(0),
                    _ => Val::I32(0),
                };
                *slot = replacement;
            }
            Ok(())
        }
    }
}

#[allow(clippy::too_many_lines)]
fn call(
    context: &mut Context<'_, '_>,
    host: HostFn,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let index = context.index;
    match host {
        HostFn::Invoke => unreachable!("handled by dispatch"),
        HostFn::AssertFail => {
            let condition = context.string(arg_u32(params, 0));
            let file = arg_u32(params, 1);
            let file = if file == 0 {
                "unknown filename".to_owned()
            } else {
                context.string(file)
            };
            let function = arg_u32(params, 3);
            let function = if function == 0 {
                "unknown function".to_owned()
            } else {
                context.string(function)
            };
            Err(abort_error(format!(
                "Aborted(Assertion failed: {condition}, at: {file},{},{function})",
                arg_i32(params, 2)
            )))
        }
        HostFn::CallSighandler => {
            let table = context.caller.data().modules[index].table;
            let slot = u64::from(arg_u32(params, 0));
            let Some(Ref::Func(Some(func))) = table.get(&mut *context.caller, slot) else {
                return Err(abort_error("signal handler is not a function"));
            };
            let ty = func.ty(&mut *context.caller);
            let mut output: Vec<Val> = ty.results().map(|kind| zero_value(&kind)).collect();
            func.call(
                &mut *context.caller,
                &[Val::I32(arg_i32(params, 1))],
                &mut output,
            )
        }
        HostFn::AbortJs => Err(abort_error(
            "Aborted(). Build with -sASSERTIONS for more info.",
        )),
        HostFn::KeepaliveClear => {
            context.caller.data_mut().modules[index].no_exit_runtime = false;
            Ok(())
        }
        HostFn::ThrowLongjmp => {
            context.caller.data_mut().count("emscripten_throw_longjmp");
            Err(wasmtime::Error::new(Longjmp))
        }
        HostFn::Exit => runtime::exit_js(context.caller, index, arg_i32(params, 0)),
        HostFn::ForceExit => {
            context.caller.data_mut().modules[index].no_exit_runtime = false;
            runtime::exit_js(context.caller, index, arg_i32(params, 0))
        }
        HostFn::ProcExit => runtime::proc_exit(context.caller, index, arg_i32(params, 0)),
        HostFn::SchedYield => {
            set_i32(results, 0);
            Ok(())
        }
        HostFn::GetTempRet0 => {
            let value = runtime::call_i32(context.caller, index, "_emscripten_tempret_get", &[])?;
            set_i32(results, value);
            Ok(())
        }
        HostFn::SetTempRet0 => {
            runtime::call_i32(
                context.caller,
                index,
                "_emscripten_tempret_set",
                &[Val::I32(arg_i32(params, 0))],
            )?;
            Ok(())
        }
        HostFn::DateNow => {
            if let Some(slot) = results.first_mut() {
                *slot = Val::F64(vfs::date_now().to_bits());
            }
            Ok(())
        }
        HostFn::GetNow => {
            let elapsed = context.caller.data().origin.elapsed();
            if let Some(slot) = results.first_mut() {
                *slot = Val::F64((elapsed.as_secs_f64() * 1000.0).to_bits());
            }
            Ok(())
        }
        HostFn::ClockTimeGet => {
            let id = arg_i32(params, 0);
            if !(0..=3).contains(&id) {
                set_i32(results, vfs::EINVAL);
                return Ok(());
            }
            let milliseconds = if id == 0 {
                vfs::date_now()
            } else {
                context.caller.data().origin.elapsed().as_secs_f64() * 1000.0
            };
            #[allow(clippy::cast_possible_truncation)]
            let nanoseconds = (milliseconds * 1e3 * 1e3).round() as i64;
            let address = arg_u32(params, 2);
            write_i64(context.memory(), address, nanoseconds);
            set_i32(results, 0);
            Ok(())
        }
        HostFn::GetHeapMax => {
            set_i32(results, to_int32(2_147_483_648.0));
            Ok(())
        }
        HostFn::ResizeHeap => {
            let grown = runtime::resize_heap(context.caller, index, arg_u32(params, 0));
            set_i32(results, i32::from(grown));
            Ok(())
        }
        HostFn::EnvironSizesGet | HostFn::EnvironGet => {
            let strings = env_strings(context.caller.data_mut(), index);
            let memory = context.memory();
            if host == HostFn::EnvironSizesGet {
                write_u32(
                    memory,
                    arg_u32(params, 0),
                    u32::try_from(strings.len()).unwrap_or(0),
                );
                let total: usize = strings.iter().map(|entry| entry.len() + 1).sum();
                write_u32(
                    memory,
                    arg_u32(params, 1),
                    u32::try_from(total).unwrap_or(0),
                );
            } else {
                let mut offset = 0_u32;
                for (position, entry) in strings.iter().enumerate() {
                    let target = arg_u32(params, 1) + offset;
                    write_u32(
                        memory,
                        arg_u32(params, 0) + u32::try_from(position * 4).unwrap_or(0),
                        target,
                    );
                    // stringToAscii writes each UTF-16 unit truncated to a byte.
                    let bytes: Vec<u8> = entry
                        .encode_utf16()
                        .map(|unit| unit.to_le_bytes()[0])
                        .collect();
                    write_bytes(memory, target, &bytes);
                    write_u8(memory, target + u32::try_from(bytes.len()).unwrap_or(0), 0);
                    offset += u32::try_from(entry.encode_utf16().count() + 1).unwrap_or(0);
                }
            }
            set_i32(results, 0);
            Ok(())
        }
        HostFn::RandomGet => {
            let address = arg_u32(params, 0) as usize;
            let length = arg_u32(params, 1) as usize;
            let memory = context.memory();
            if let Some(slot) = memory.get_mut(address..address + length) {
                getrandom::fill(slot).map_err(|error| abort_error(error.to_string()))?;
            }
            set_i32(results, 0);
            Ok(())
        }
        HostFn::SetitimerJs => {
            let which = arg_i32(params, 0);
            let timeout = arg_f64(params, 1);
            let now = context.caller.data().origin.elapsed().as_secs_f64() * 1000.0;
            let module = &mut context.caller.data_mut().modules[index];
            module.timers.remove(&which);
            if timeout != 0.0 && !timeout.is_nan() {
                module.timers.insert(which, now + timeout);
            }
            set_i32(results, 0);
            Ok(())
        }
        HostFn::TzsetJs => {
            tzset(context, params);
            Ok(())
        }
        HostFn::LocaltimeJs => {
            localtime(context, params);
            Ok(())
        }
        HostFn::GmtimeJs => {
            gmtime(context, params);
            Ok(())
        }
        HostFn::MktimeJs => mktime(context, params, results),
        HostFn::DlopenJs => {
            let status = crate::dylink::dlopen(context.caller, index, arg_i32(params, 0))?;
            set_i32(results, status);
            Ok(())
        }
        HostFn::DlsymJs => {
            let value = crate::dylink::dlsym(
                context.caller,
                index,
                arg_i32(params, 0),
                arg_u32(params, 1),
                arg_u32(params, 2),
            )?;
            set_i32(results, value);
            Ok(())
        }
        HostFn::MmapJs => {
            let value = mmap(context, params)?;
            set_i32(results, value);
            Ok(())
        }
        HostFn::MunmapJs => {
            let value = munmap(context, params)?;
            set_i32(results, value);
            Ok(())
        }
        HostFn::Getaddrinfo => {
            let value = crate::netdb::getaddrinfo(context.caller, index, params)?;
            set_i32(results, value);
            Ok(())
        }
        HostFn::Getnameinfo => {
            let value = crate::netdb::getnameinfo(context.caller, index, params);
            set_i32(results, value);
            Ok(())
        }
        _ => {
            let value = filesystem_call(context, host, params)?;
            set_i32(results, value);
            Ok(())
        }
    }
}

/// `getEnvStrings()`.
fn env_strings(runtime: &mut Runtime, index: usize) -> Vec<String> {
    let module = &mut runtime.modules[index];
    if let Some(strings) = &module.env_strings {
        return strings.clone();
    }
    let mut entries: Vec<(String, String)> = vec![
        ("USER".into(), "web_user".into()),
        ("LOGNAME".into(), "web_user".into()),
        ("PATH".into(), "/".into()),
        ("PWD".into(), "/".into()),
        ("HOME".into(), "/home/web_user".into()),
        ("LANG".into(), module.default_lang.clone()),
        ("_".into(), module.this_program.clone()),
    ];
    for (key, value) in &module.env {
        if let Some(entry) = entries.iter_mut().find(|(existing, _)| existing == key) {
            entry.1.clone_from(value);
        } else {
            entries.push((key.clone(), value.clone()));
        }
    }
    let strings: Vec<String> = entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    module.env_strings = Some(strings.clone());
    strings
}

fn tzset(context: &mut Context<'_, '_>, params: &[Val]) {
    let clock = &context.caller.data().clock;
    let now = vfs::date_now();
    let year = clock.local_fields(now).map_or(1970, |fields| fields.year);
    let (january, july) = clock.january_july_offsets(f64::from(year));
    let maximum = january.max(july);
    let name = |offset: f64| {
        let sign = if offset >= 0.0 { '-' } else { '+' };
        let absolute = offset.abs();
        #[allow(clippy::cast_possible_truncation)]
        let hours = (absolute / 60.0).floor() as i64;
        format!("UTC{sign}{hours:02}{:02}", absolute % 60.0)
    };
    let january_name = name(january);
    let july_name = name(july);
    let memory = context.memory();
    write_u32(
        memory,
        arg_u32(params, 0),
        to_int32(maximum * 60.0).cast_unsigned(),
    );
    write_i32(
        memory,
        arg_u32(params, 1),
        i32::from(january.total_cmp(&july).is_ne()),
    );
    let (standard, daylight) = if july < january {
        (january_name, july_name)
    } else {
        (july_name, january_name)
    };
    string_to_utf8(memory, &standard, arg_u32(params, 2), 17);
    string_to_utf8(memory, &daylight, arg_u32(params, 3), 17);
}

fn date_value_from_seconds(seconds: i64) -> f64 {
    match i53(seconds) {
        // Exact below 2^53.
        #[allow(clippy::cast_precision_loss)]
        Some(seconds) => seconds as f64 * 1000.0,
        None => f64::NAN,
    }
}

fn localtime(context: &mut Context<'_, '_>, params: &[Val]) {
    let value = date_value_from_seconds(arg_i64(params, 0));
    let address = arg_u32(params, 1);
    let clock = &context.caller.data().clock;
    let fields = clock.local_fields(value);
    let (offset, dst) = match fields {
        Some(fields) => {
            let offset = clock.timezone_offset_minutes(value);
            let (january, july) = clock.january_july_offsets(f64::from(fields.year));
            let dst = july.total_cmp(&january).is_ne()
                && (offset - january.min(july)).abs() < f64::EPSILON;
            (offset, dst)
        }
        None => (f64::NAN, false),
    };
    let fields = fields.unwrap_or_default();
    let memory = context.memory();
    let values = [
        fields.second,
        fields.minute,
        fields.hour,
        fields.day,
        fields.month,
        fields.year - 1900,
        fields.weekday,
        fields.yearday,
    ];
    for (position, value) in values.iter().enumerate() {
        write_i32(
            memory,
            address + u32::try_from(position * 4).unwrap_or(0),
            *value,
        );
    }
    write_i32(memory, address + 36, to_int32(-(offset * 60.0)));
    write_i32(memory, address + 32, i32::from(dst));
}

fn gmtime(context: &mut Context<'_, '_>, params: &[Val]) {
    let value = date_value_from_seconds(arg_i64(params, 0));
    let address = arg_u32(params, 1);
    let memory = context.memory();
    if !value.is_finite() || value.abs() > 8.64e15 {
        for position in 0..8 {
            write_i32(memory, address + position * 4, 0);
        }
        return;
    }
    let fields = crate::jsdate::utc_fields(value);
    let values = [
        fields.second,
        fields.minute,
        fields.hour,
        fields.day,
        fields.month,
        fields.year - 1900,
        fields.weekday,
        fields.yearday,
    ];
    for (position, value) in values.iter().enumerate() {
        write_i32(
            memory,
            address + u32::try_from(position * 4).unwrap_or(0),
            *value,
        );
    }
}

fn mktime(
    context: &mut Context<'_, '_>,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let address = arg_u32(params, 0);
    let memory = context.memory();
    let field = |offset: u32| f64::from(read_i32(memory, address + offset));
    let (year, month, day, hour, minute, second) = (
        field(20) + 1900.0,
        field(16),
        field(12),
        field(8),
        field(4),
        field(0),
    );
    let isdst = read_i32(memory, address + 32);
    let clock = &context.caller.data().clock;
    let mut time = clock.local_constructor(year, month, day, hour, minute, second, 0.0);
    let offset = clock.timezone_offset_minutes(time);
    let local_year = clock
        .local_fields(time)
        .map_or(f64::NAN, |fields| f64::from(fields.year));
    let (january, july) = clock.january_july_offsets(local_year);
    let minimum = january.min(july);
    let mut write_dst = None;
    if isdst < 0 {
        write_dst = Some(i32::from(
            july.total_cmp(&january).is_ne() && (minimum - offset).abs() < f64::EPSILON,
        ));
    } else if (isdst > 0) != ((minimum - offset).abs() < f64::EPSILON) {
        let maximum = january.max(july);
        let target = if isdst > 0 { minimum } else { maximum };
        time += (target - offset) * 60_000.0;
    }
    let fields = clock.local_fields(time);
    let memory = context.memory();
    if let Some(dst) = write_dst {
        write_i32(memory, address + 32, dst);
    }
    let fields = fields.unwrap_or_default();
    write_i32(memory, address + 24, fields.weekday);
    write_i32(memory, address + 28, fields.yearday);
    write_i32(memory, address, fields.second);
    write_i32(memory, address + 4, fields.minute);
    write_i32(memory, address + 8, fields.hour);
    write_i32(memory, address + 12, fields.day);
    write_i32(memory, address + 16, fields.month);
    write_i32(memory, address + 20, fields.year - 1900);
    let seconds = if time.is_nan() { -1.0 } else { time / 1000.0 };
    if seconds.fract() != 0.0 {
        return Err(abort_error(format!(
            "RangeError: The number {seconds} cannot be converted to a BigInt because it is not an integer"
        )));
    }
    if let Some(slot) = results.first_mut() {
        #[allow(clippy::cast_possible_truncation)]
        let value = seconds as i64;
        *slot = Val::I64(value);
    }
    Ok(())
}

fn mmap(context: &mut Context<'_, '_>, params: &[Val]) -> wasmtime::Result<i32> {
    let length = arg_u32(params, 0);
    let protection = arg_i32(params, 1);
    let flags = arg_i32(params, 2);
    let fd = arg_i32(params, 3);
    let Some(offset) = i53(arg_i64(params, 4)) else {
        return Ok(vfs::EOVERFLOW);
    };
    let allocated = arg_u32(params, 5);
    let address_out = arg_u32(params, 6);
    let fs = context.fs();
    let check = (|| -> FsResult<()> {
        let fs = fs.borrow();
        let stream = fs.get_stream_checked(fd)?;
        let access = stream.flags() & vfs::O_ACCMODE;
        if protection & 2 != 0 && flags & 2 == 0 && access != 2 {
            return Err(FsError::Errno(vfs::EACCES));
        }
        if access == 1 {
            return Err(FsError::Errno(vfs::EACCES));
        }
        if !fs.has_mmap(fd)? {
            return Err(FsError::Errno(vfs::ENODEV));
        }
        if length == 0 {
            return Err(FsError::Errno(vfs::EINVAL));
        }
        Ok(())
    })();
    if let Err(error) = check {
        return syscall_result(Err(error));
    }
    let address = runtime::mmap_alloc(context.caller, context.index, length)?;
    let is_memfs = matches!(
        fs.borrow().get_stream_checked(fd).map(|stream| stream.ops),
        Ok(vfs::StreamOps::MemFile)
    );
    if address == 0 && is_memfs {
        return Ok(-vfs::ENOMEM);
    }
    let bytes = match fs.borrow_mut().mmap_read(fd, length as usize, offset) {
        Ok(bytes) => bytes,
        Err(error) => return syscall_result(Err(error)),
    };
    let memory = context.memory();
    write_bytes(memory, address.cast_unsigned(), &bytes);
    write_i32(memory, allocated, 1);
    write_u32(memory, address_out, address.cast_unsigned());
    Ok(0)
}

fn munmap(context: &mut Context<'_, '_>, params: &[Val]) -> wasmtime::Result<i32> {
    let address = arg_u32(params, 0) as usize;
    let length = arg_u32(params, 1) as usize;
    let protection = arg_i32(params, 2);
    let flags = arg_i32(params, 3);
    let fd = arg_i32(params, 4);
    let offset = arg_i64(params, 5);
    let fs = context.fs();
    if let Err(error) = fs.borrow().get_stream_checked(fd) {
        return syscall_result(Err(error));
    }
    if protection & 2 != 0 {
        let node = fs.borrow().stream_node(fd);
        let is_file = node.is_ok_and(|node| vfs::is_file(fs.borrow().node(node).mode));
        if !is_file {
            return Ok(-vfs::ENODEV);
        }
        if flags & 2 != 0 {
            return Ok(0);
        }
        let bytes = context
            .memory()
            .get(address..address + length)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if let Err(error) = fs.borrow_mut().msync(fd, &bytes, offset) {
            return syscall_result(Err(error));
        }
    }
    Ok(0)
}

#[allow(clippy::too_many_lines)]
fn filesystem_call(
    context: &mut Context<'_, '_>,
    host: HostFn,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let fs_handle = context.fs();
    match host {
        HostFn::Chdir => {
            let path = context.string(arg_u32(params, 0));
            syscall_result(fs_handle.borrow_mut().chdir(&path).map(|()| 0))
        }
        HostFn::Chmod => {
            let path = context.string(arg_u32(params, 0));
            syscall_result(
                fs_handle
                    .borrow_mut()
                    .chmod_path(&path, arg_u32(params, 1), false)
                    .map(|()| 0),
            )
        }
        HostFn::Dup => syscall_result(fs_handle.borrow_mut().dup_stream(arg_i32(params, 0), None)),
        HostFn::Dup3 => {
            let old = arg_i32(params, 0);
            let new = arg_i32(params, 1);
            let mut fs = fs_handle.borrow_mut();
            let result = (|| {
                let stream = fs.get_stream_checked(old)?;
                if stream.fd == Some(new) {
                    return Ok(-vfs::EINVAL);
                }
                if !(0..4096).contains(&new) {
                    return Ok(-vfs::EBADF);
                }
                if fs.get_stream(new).is_some() {
                    fs.close(new)?;
                }
                fs.dup_stream(old, Some(new))
            })();
            syscall_result(result)
        }
        HostFn::Faccessat => {
            let raw = context.string(arg_u32(params, 1));
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, false)
                .and_then(|path| fs_handle.borrow_mut().access(&path, arg_i32(params, 2)));
            syscall_result(result)
        }
        HostFn::Fadvise64 => Ok(0),
        HostFn::Fallocate => {
            let (Some(offset), Some(length)) = (i53(arg_i64(params, 2)), i53(arg_i64(params, 3)))
            else {
                return Ok(vfs::EOVERFLOW);
            };
            syscall_result(
                fs_handle
                    .borrow_mut()
                    .allocate(arg_i32(params, 0), offset, length)
                    .map(|()| 0),
            )
        }
        HostFn::Fchmod => syscall_result(
            fs_handle
                .borrow_mut()
                .fchmod(arg_i32(params, 0), arg_u32(params, 1))
                .map(|()| 0),
        ),
        HostFn::Fchmodat2 => {
            let raw = context.string(arg_u32(params, 1));
            let no_follow = arg_i32(params, 3) & 256 != 0;
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, false)
                .and_then(|path| {
                    fs_handle
                        .borrow_mut()
                        .chmod_path(&path, arg_u32(params, 2), no_follow)
                })
                .map(|()| 0);
            syscall_result(result)
        }
        HostFn::Fchown32 => {
            let mut fs = fs_handle.borrow_mut();
            let result = fs
                .get_stream_checked(arg_i32(params, 0))
                .map(|stream| stream.node)
                .and_then(|node| fs.chown_node(node))
                .map(|()| 0);
            syscall_result(result)
        }
        HostFn::Fchownat => {
            let raw = context.string(arg_u32(params, 1));
            let no_follow = arg_i32(params, 4) & 256 != 0;
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, false)
                .and_then(|path| fs_handle.borrow_mut().chown_path(&path, no_follow))
                .map(|()| 0);
            syscall_result(result)
        }
        HostFn::Fcntl64 => fcntl(context, &fs_handle, params),
        HostFn::Fdatasync => syscall_result(
            fs_handle
                .borrow()
                .get_stream_checked(arg_i32(params, 0))
                .map(|_| 0),
        ),
        HostFn::Fstat64 => {
            let path = match fs_handle.borrow().get_stream_checked(arg_i32(params, 0)) {
                Ok(stream) => stream.path.clone(),
                Err(error) => return syscall_result(Err(error)),
            };
            let stat = fs_handle.borrow_mut().stat(&path, false);
            match stat {
                Ok(stat) => {
                    context.do_stat(stat, arg_u32(params, 1));
                    Ok(0)
                }
                Err(error) => syscall_result(Err(error)),
            }
        }
        HostFn::Ftruncate64 => {
            let Some(length) = i53(arg_i64(params, 1)) else {
                return Ok(vfs::EOVERFLOW);
            };
            syscall_result(
                fs_handle
                    .borrow_mut()
                    .ftruncate(arg_i32(params, 0), length)
                    .map(|()| 0),
            )
        }
        HostFn::Getcwd => {
            let size = arg_u32(params, 1) as usize;
            if size == 0 {
                return Ok(-vfs::EINVAL);
            }
            let cwd = fs_handle.borrow().cwd();
            let needed = cwd.len() + 1;
            if size < needed {
                return Ok(-68);
            }
            string_to_utf8(context.memory(), &cwd, arg_u32(params, 0), size);
            Ok(i32::try_from(needed).unwrap_or(i32::MAX))
        }
        HostFn::Getdents64 => getdents(context, &fs_handle, params),
        HostFn::Ioctl => ioctl(context, &fs_handle, params),
        HostFn::Lstat64 | HostFn::Stat64 => {
            let path = context.string(arg_u32(params, 0));
            let stat = fs_handle.borrow_mut().stat(&path, host == HostFn::Lstat64);
            match stat {
                Ok(stat) => {
                    context.do_stat(stat, arg_u32(params, 1));
                    Ok(0)
                }
                Err(error) => syscall_result(Err(error)),
            }
        }
        HostFn::Newfstatat => {
            let raw = context.string(arg_u32(params, 1));
            let flags = arg_i32(params, 3);
            let no_follow = flags & 256 != 0;
            let allow_empty = flags & 4096 != 0;
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, allow_empty)
                .and_then(|path| fs_handle.borrow_mut().stat(&path, no_follow));
            match result {
                Ok(stat) => {
                    context.do_stat(stat, arg_u32(params, 2));
                    Ok(0)
                }
                Err(error) => syscall_result(Err(error)),
            }
        }
        HostFn::Mkdirat => {
            let raw = context.string(arg_u32(params, 1));
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, false)
                .and_then(|path| fs_handle.borrow_mut().mkdir(&path, arg_u32(params, 2)))
                .map(|_| 0);
            syscall_result(result)
        }
        HostFn::Openat => {
            let raw = context.string(arg_u32(params, 1));
            let varargs = arg_u32(params, 3);
            let mode = if varargs == 0 {
                0
            } else {
                read_u32(context.memory(), varargs)
            };
            let result = context
                .calculate_at(arg_i32(params, 0), &raw, false)
                .and_then(|path| {
                    fs_handle
                        .borrow_mut()
                        .open(&path, arg_i32(params, 2), Some(mode))
                });
            syscall_result(result)
        }
        HostFn::Pipe => {
            let address = arg_u32(params, 0);
            if address == 0 {
                return Ok(-vfs::EFAULT);
            }
            match fs_handle.borrow_mut().create_pipe() {
                Ok((read, write)) => {
                    let memory = context.memory();
                    write_i32(memory, address, read);
                    write_i32(memory, address + 4, write);
                    Ok(0)
                }
                Err(error) => syscall_result(Err(error)),
            }
        }
        HostFn::Readlinkat => {
            let raw = context.string(arg_u32(params, 1));
            let buffer = arg_u32(params, 2);
            let size = arg_i32(params, 3);
            let path = match context.calculate_at(arg_i32(params, 0), &raw, false) {
                Ok(path) => path,
                Err(error) => return syscall_result(Err(error)),
            };
            if size <= 0 {
                return Ok(-vfs::EINVAL);
            }
            let target = match fs_handle.borrow_mut().readlink(&path) {
                Ok(target) => target,
                Err(error) => return syscall_result(Err(error)),
            };
            let size = size.cast_unsigned() as usize;
            let count = size.min(target.len());
            let memory = context.memory();
            let saved = runtime::read_u8(memory, buffer + u32::try_from(count).unwrap_or(0));
            string_to_utf8(memory, &target, buffer, size + 1);
            write_u8(memory, buffer + u32::try_from(count).unwrap_or(0), saved);
            Ok(i32::try_from(count).unwrap_or(i32::MAX))
        }
        HostFn::Renameat => {
            let old_raw = context.string(arg_u32(params, 1));
            let new_raw = context.string(arg_u32(params, 3));
            let result = context
                .calculate_at(arg_i32(params, 0), &old_raw, false)
                .and_then(|old| {
                    context
                        .calculate_at(arg_i32(params, 2), &new_raw, false)
                        .map(|new| (old, new))
                })
                .and_then(|(old, new)| fs_handle.borrow_mut().rename(&old, &new))
                .map(|()| 0);
            syscall_result(result)
        }
        HostFn::Rmdir => {
            let path = context.string(arg_u32(params, 0));
            syscall_result(fs_handle.borrow_mut().rmdir(&path).map(|()| 0))
        }
        HostFn::Statfs64 => {
            let path = context.string(arg_u32(params, 0));
            let values = match fs_handle.borrow_mut().statfs(&path) {
                Ok(values) => values,
                Err(error) => return syscall_result(Err(error)),
            };
            let address = arg_u32(params, 2);
            let memory = context.memory();
            let narrow = |value: i64| to_int32_i64(value);
            write_i32(memory, address + 4, narrow(values[0]));
            write_i32(memory, address + 40, narrow(values[0]));
            write_i32(memory, address + 8, narrow(values[2]));
            write_i32(memory, address + 12, narrow(values[3]));
            write_i32(memory, address + 16, narrow(values[4]));
            write_i32(memory, address + 20, narrow(values[5]));
            write_i32(memory, address + 24, narrow(values[6]));
            write_i32(memory, address + 28, narrow(values[7]));
            write_i32(memory, address + 44, narrow(values[8]));
            write_i32(memory, address + 36, narrow(values[9]));
            Ok(0)
        }
        HostFn::Symlinkat => {
            let target = context.string(arg_u32(params, 0));
            let raw = context.string(arg_u32(params, 2));
            let result = context
                .calculate_at(arg_i32(params, 1), &raw, false)
                .and_then(|path| fs_handle.borrow_mut().symlink(&target, &path))
                .map(|_| 0);
            syscall_result(result)
        }
        HostFn::Truncate64 => {
            let Some(length) = i53(arg_i64(params, 1)) else {
                return Ok(vfs::EOVERFLOW);
            };
            let path = context.string(arg_u32(params, 0));
            syscall_result(
                fs_handle
                    .borrow_mut()
                    .truncate_path(&path, length)
                    .map(|()| 0),
            )
        }
        HostFn::Unlinkat => {
            let raw = context.string(arg_u32(params, 1));
            let flags = arg_i32(params, 2);
            let path = match context.calculate_at(arg_i32(params, 0), &raw, false) {
                Ok(path) => path,
                Err(error) => return syscall_result(Err(error)),
            };
            let result = match flags {
                0 => fs_handle.borrow_mut().unlink(&path),
                512 => fs_handle.borrow_mut().rmdir(&path),
                _ => return Err(abort_error("Aborted(Invalid flags passed to unlinkat)")),
            };
            syscall_result(result.map(|()| 0))
        }
        HostFn::Utimensat => {
            let raw = context.string(arg_u32(params, 1));
            let times = arg_u32(params, 2);
            let path = match context.calculate_at(arg_i32(params, 0), &raw, true) {
                Ok(path) => path,
                Err(error) => return syscall_result(Err(error)),
            };
            let now = vfs::date_now();
            let (atime, mtime) = if times == 0 {
                (Some(now), Some(now))
            } else {
                let memory = context.memory();
                let read_time = |address: u32| {
                    // readI53FromI64: low word unsigned plus high word.
                    let seconds = f64::from(read_u32(memory, address))
                        + f64::from(read_i32(memory, address + 4)) * 4_294_967_296.0;
                    let nanoseconds = read_i32(memory, address + 8);
                    match nanoseconds {
                        1_073_741_823 => Some(now),
                        1_073_741_822 => None,
                        _ => Some(seconds * 1e3 + f64::from(nanoseconds) / 1e6),
                    }
                };
                (read_time(times), read_time(times + 16))
            };
            if mtime.or(atime).is_some() {
                let result = fs_handle.borrow_mut().utime(&path, atime, mtime);
                return syscall_result(result.map(|()| 0));
            }
            Ok(0)
        }
        HostFn::FdClose => {
            let fd = arg_i32(params, 0);
            let result = fs_handle.borrow_mut().close(fd).map(|()| 0);
            wasi_result(result)
        }
        HostFn::FdFdstatGet => {
            let fd = arg_i32(params, 0);
            let kind = match fs_handle.borrow().get_stream_checked(fd) {
                // The glue tests `stream.mode`, which streams do not have, so
                // every non-TTY stream reports a regular file.
                Ok(stream) => {
                    if stream.tty.is_some() {
                        2
                    } else {
                        4
                    }
                }
                Err(error) => return wasi_result(Err(error)),
            };
            let address = arg_u32(params, 1);
            let memory = context.memory();
            write_u8(memory, address, kind);
            write_i16(memory, address + 2, 0);
            write_i64(memory, address + 8, 0);
            write_i64(memory, address + 16, 0);
            Ok(0)
        }
        HostFn::FdRead | HostFn::FdPread => {
            let fd = arg_i32(params, 0);
            let iov = arg_u32(params, 1);
            let count = arg_u32(params, 2);
            let (position, out) = if host == HostFn::FdPread {
                let Some(offset) = i53(arg_i64(params, 3)) else {
                    return Ok(vfs::EOVERFLOW);
                };
                (Some(offset), arg_u32(params, 4))
            } else {
                (None, arg_u32(params, 3))
            };
            let result = do_readv(context, &fs_handle, fd, iov, count, position);
            match result {
                Ok(total) => {
                    write_u32(context.memory(), out, total);
                    Ok(0)
                }
                Err(error) => wasi_result(Err(error)),
            }
        }
        HostFn::FdWrite | HostFn::FdPwrite => {
            let fd = arg_i32(params, 0);
            let iov = arg_u32(params, 1);
            let count = arg_u32(params, 2);
            let (position, out) = if host == HostFn::FdPwrite {
                let Some(offset) = i53(arg_i64(params, 3)) else {
                    return Ok(vfs::EOVERFLOW);
                };
                (Some(offset), arg_u32(params, 4))
            } else {
                (None, arg_u32(params, 3))
            };
            let result = do_writev(context, &fs_handle, fd, iov, count, position);
            match result {
                Ok(total) => {
                    write_u32(context.memory(), out, total);
                    Ok(0)
                }
                Err(error) => wasi_result(Err(error)),
            }
        }
        HostFn::FdSeek => {
            let fd = arg_i32(params, 0);
            let Some(offset) = i53(arg_i64(params, 1)) else {
                return Ok(vfs::EOVERFLOW);
            };
            let whence = arg_i32(params, 2);
            let out = arg_u32(params, 3);
            let result = fs_handle.borrow_mut().llseek(fd, offset, whence);
            match result {
                Ok(position) => {
                    write_i64(context.memory(), out, position);
                    let mut fs = fs_handle.borrow_mut();
                    if let Some(stream) = fs.get_stream_mut(fd)
                        && stream.getdents.is_some()
                        && offset == 0
                        && whence == 0
                    {
                        stream.getdents = None;
                    }
                    Ok(0)
                }
                Err(error) => wasi_result(Err(error)),
            }
        }
        HostFn::FdSync => {
            let result = fs_handle.borrow_mut().fd_sync(arg_i32(params, 0));
            wasi_result(result)
        }
        HostFn::Socket => syscall_result(
            fs_handle
                .borrow_mut()
                .create_socket(arg_i32(params, 1), arg_i32(params, 2)),
        ),
        HostFn::Bind
        | HostFn::Connect
        | HostFn::Listen
        | HostFn::Accept4
        | HostFn::Recvfrom
        | HostFn::Sendto => socket_call(context, &fs_handle, host, params),
        HostFn::Newselect => newselect(context, &fs_handle, params),
        _ => Err(abort_error(format!("unhandled host import {host:?}"))),
    }
}

fn do_readv(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    fd: i32,
    iov: u32,
    count: u32,
    position: Option<i64>,
) -> FsResult<u32> {
    let mut total = 0_u32;
    let mut position = position;
    for item in 0..count {
        let memory = context.memory();
        let pointer = read_u32(memory, iov + item * 8) as usize;
        let length = read_u32(memory, iov + item * 8 + 4) as usize;
        let Some(buffer) = memory.get_mut(pointer..pointer + length) else {
            return Err(FsError::Errno(vfs::EFAULT));
        };
        let read = fs.borrow_mut().read(fd, buffer, position)?;
        total += u32::try_from(read).unwrap_or(0);
        if read < length {
            break;
        }
        if let Some(offset) = position.as_mut() {
            *offset += i64::try_from(read).unwrap_or(0);
        }
    }
    Ok(total)
}

fn do_writev(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    fd: i32,
    iov: u32,
    count: u32,
    position: Option<i64>,
) -> FsResult<u32> {
    let mut total = 0_u32;
    let mut position = position;
    for item in 0..count {
        let memory = context.memory();
        let pointer = read_u32(memory, iov + item * 8) as usize;
        let length = read_u32(memory, iov + item * 8 + 4) as usize;
        let Some(buffer) = memory.get(pointer..pointer + length) else {
            return Err(FsError::Errno(vfs::EFAULT));
        };
        let buffer = buffer.to_vec();
        let written = fs.borrow_mut().write(fd, &buffer, position)?;
        total += u32::try_from(written).unwrap_or(0);
        if written < length {
            break;
        }
        if let Some(offset) = position.as_mut() {
            *offset += i64::try_from(written).unwrap_or(0);
        }
    }
    Ok(total)
}

fn fcntl(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let fd = arg_i32(params, 0);
    let command = arg_i32(params, 1);
    let varargs = arg_u32(params, 2);
    let mut fs = fs.borrow_mut();
    let flags = match fs.get_stream_checked(fd) {
        Ok(stream) => stream.flags(),
        Err(error) => return syscall_result(Err(error)),
    };
    match command {
        0 => {
            let mut target = read_i32(context.memory(), varargs);
            if target < 0 {
                return Ok(-vfs::EINVAL);
            }
            while fs.get_stream(target).is_some() {
                target += 1;
            }
            syscall_result(fs.dup_stream(fd, Some(target)))
        }
        1 | 2 | 13 | 14 => Ok(0),
        3 => Ok(flags),
        4 => {
            let add = read_i32(context.memory(), varargs);
            if let Some(stream) = fs.get_stream(fd) {
                stream.shared.borrow_mut().flags |= add;
            }
            Ok(0)
        }
        12 => {
            let pointer = read_u32(context.memory(), varargs);
            write_i16(context.memory(), pointer, 2);
            Ok(0)
        }
        _ => Ok(-vfs::EINVAL),
    }
}

fn getdents(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    params: &[Val],
) -> wasmtime::Result<i32> {
    const RECORD: u32 = 280;
    let fd = arg_i32(params, 0);
    let buffer = arg_u32(params, 1);
    let size = arg_u32(params, 2);
    let mut fs = fs.borrow_mut();
    let result = (|| -> FsResult<i32> {
        let path = fs.get_stream_checked(fd)?.path.clone();
        if fs.get_stream_checked(fd)?.getdents.is_none() {
            let names = fs.readdir(&path)?;
            if let Some(stream) = fs.get_stream_mut(fd) {
                stream.getdents = Some(names);
            }
        }
        let names = fs
            .get_stream_checked(fd)?
            .getdents
            .clone()
            .unwrap_or_default();
        let position = fs.llseek(fd, 0, 1)?;
        let start = usize::try_from(position / i64::from(RECORD)).unwrap_or(0);
        let end = names.len().min(start + (size / RECORD) as usize);
        let mut written = 0_u32;
        let mut current = start;
        while current < end {
            let name = &names[current];
            if let Some((inode, kind)) = fs.dirent(fd, name)? {
                let memory = context.memory();
                let at = buffer + written;
                write_i64(memory, at, inode.cast_signed());
                write_i64(
                    memory,
                    at + 8,
                    i64::try_from((current + 1) * RECORD as usize).unwrap_or(0),
                );
                write_i16(memory, at + 16, 280);
                write_u8(memory, at + 18, kind);
                string_to_utf8(memory, name, at + 19, 256);
                written += RECORD;
            }
            current += 1;
        }
        fs.llseek(fd, i64::try_from(current * RECORD as usize).unwrap_or(0), 0)?;
        Ok(written.cast_signed())
    })();
    syscall_result(result)
}

fn ioctl(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let fd = arg_i32(params, 0);
    let operation = arg_i32(params, 1);
    let varargs = arg_u32(params, 2);
    let fs = fs.borrow();
    let stream = match fs.get_stream_checked(fd) {
        Ok(stream) => stream,
        Err(error) => return syscall_result(Err(error)),
    };
    let tty = stream.tty;
    let ops = stream.ops;
    match operation {
        21509 | 21510 | 21511 | 21512 | 21524 | 21515 => {
            Ok(if tty.is_some() { 0 } else { -vfs::ENOTTY })
        }
        21505 => {
            if tty.is_none() {
                return Ok(-vfs::ENOTTY);
            }
            if tty == Some(0) {
                return Ok(0);
            }
            // default_tty_ops.ioctl_tcgets (tty1 has none).
            if tty == Some(usize::try_from(vfs::makedev(5, 0)).unwrap_or(0)) {
                let pointer = read_u32(context.memory(), varargs);
                let memory = context.memory();
                write_i32(memory, pointer, 25856);
                write_i32(memory, pointer + 4, 5);
                write_i32(memory, pointer + 8, 191);
                write_i32(memory, pointer + 12, 35387);
                let control = [3_u8, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 0, 18, 15, 23, 22];
                for slot in 0..32_u32 {
                    let value = control.get(slot as usize).copied().unwrap_or(0);
                    write_u8(memory, pointer + slot + 17, value);
                }
            }
            Ok(0)
        }
        21506..=21508 => Ok(if tty.is_some() { 0 } else { -vfs::ENOTTY }),
        21519 => {
            if tty.is_none() {
                return Ok(-vfs::ENOTTY);
            }
            let pointer = read_u32(context.memory(), varargs);
            write_i32(context.memory(), pointer, 0);
            Ok(0)
        }
        21520 => Ok(if tty.is_some() {
            -vfs::EINVAL
        } else {
            -vfs::ENOTTY
        }),
        21531 => match ops {
            vfs::StreamOps::Pipe => Ok(vfs::EINVAL),
            vfs::StreamOps::Socket => {
                let pointer = read_u32(context.memory(), varargs);
                write_i32(context.memory(), pointer, 0);
                Ok(0)
            }
            _ => Ok(-vfs::ENOTTY),
        },
        21523 => {
            if tty.is_none() {
                return Ok(-vfs::ENOTTY);
            }
            if tty == Some(usize::try_from(vfs::makedev(5, 0)).unwrap_or(0)) {
                let pointer = read_u32(context.memory(), varargs);
                write_i16(context.memory(), pointer, 24);
                write_i16(context.memory(), pointer + 2, 80);
            }
            Ok(0)
        }
        _ => Ok(-vfs::EINVAL),
    }
}

/// Socket calls on a SOCKFS socket. No peer can exist because Node has no
/// `ws` module in the pinned package, so connect fails and listen aborts the
/// way the glue does.
fn socket_call(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    host: HostFn,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let fd = arg_i32(params, 0);
    if !fs.borrow().is_socket_fd(fd) {
        if host == HostFn::Sendto && arg_u32(params, 4) == 0 {
            // sendto without an address writes through the stream of a
            // non-socket descriptor only after getSocketFromFD succeeds.
            return Ok(-vfs::EBADF);
        }
        return Ok(-vfs::EBADF);
    }
    let _ = context;
    match host {
        HostFn::Bind => Ok(0),
        HostFn::Connect => Ok(-vfs::EHOSTUNREACH),
        HostFn::Listen => Err(abort_error("Cannot find module 'ws'")),
        HostFn::Recvfrom | HostFn::Sendto => Ok(-vfs::ENOTCONN),
        _ => Ok(-vfs::EINVAL),
    }
}

fn newselect(
    context: &mut Context<'_, '_>,
    fs: &Rc<std::cell::RefCell<Fs>>,
    params: &[Val],
) -> wasmtime::Result<i32> {
    let count = arg_i32(params, 0);
    let read_set = arg_u32(params, 1);
    let write_set = arg_u32(params, 2);
    let except_set = arg_u32(params, 3);
    let memory = context.memory();
    let words = |address: u32| -> (i32, i32) {
        if address == 0 {
            (0, 0)
        } else {
            (read_i32(memory, address), read_i32(memory, address + 4))
        }
    };
    let (read_low, read_high) = words(read_set);
    let (write_low, write_high) = words(write_set);
    let (except_low, except_high) = words(except_set);
    let all_low = read_low | write_low | except_low;
    let all_high = read_high | write_high | except_high;
    let pick = |fd: i32, low: i32, high: i32, mask: i32| {
        if fd < 32 { low & mask } else { high & mask }
    };
    let mut total = 0;
    let (mut out_read_low, mut out_read_high, mut out_write_low, mut out_write_high) = (0, 0, 0, 0);
    let (mut out_except_low, mut out_except_high) = (0, 0);
    let fs = fs.borrow();
    for fd in 0..count {
        let mask = 1_i32.wrapping_shl(u32::try_from(fd % 32).unwrap_or(0));
        if pick(fd, all_low, all_high, mask) == 0 {
            continue;
        }
        let stream = match fs.get_stream_checked(fd) {
            Ok(stream) => stream,
            Err(error) => return syscall_result(Err(error)),
        };
        let flags = match stream.ops {
            vfs::StreamOps::Pipe => {
                if stream.flags() & vfs::O_ACCMODE == 1 {
                    260
                } else {
                    // Readable when any bucket holds unread bytes.
                    5
                }
            }
            _ => 5,
        };
        if flags & 1 != 0 && pick(fd, read_low, read_high, mask) != 0 {
            if fd < 32 {
                out_read_low |= mask;
            } else {
                out_read_high |= mask;
            }
            total += 1;
        }
        if flags & 4 != 0 && pick(fd, write_low, write_high, mask) != 0 {
            if fd < 32 {
                out_write_low |= mask;
            } else {
                out_write_high |= mask;
            }
            total += 1;
        }
        if flags & 2 != 0 && pick(fd, except_low, except_high, mask) != 0 {
            if fd < 32 {
                out_except_low |= mask;
            } else {
                out_except_high |= mask;
            }
            total += 1;
        }
    }
    drop(fs);
    let memory = context.memory();
    if read_set != 0 {
        write_i32(memory, read_set, out_read_low);
        write_i32(memory, read_set + 4, out_read_high);
    }
    if write_set != 0 {
        write_i32(memory, write_set, out_write_low);
        write_i32(memory, write_set + 4, out_write_high);
    }
    if except_set != 0 {
        write_i32(memory, except_set, out_except_low);
        write_i32(memory, except_set + 4, out_except_high);
    }
    Ok(total)
}

#[allow(dead_code)]
fn unused(_: ExitStatus, _: Abort, _: i64) -> i64 {
    read_i64(&[], 0)
}
