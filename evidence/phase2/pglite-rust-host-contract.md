# PGlite Rust host contract and runtime selection

Task `P2-PGLITE-RUST-HOST-01`, step 1. This records the full import contract
of the pinned PGlite Wasm modules, what the distributed JavaScript glue does
for each import, and the Rust runtime selection under `unsafe_code = "forbid"`.
No host code exists at this checkpoint.

## Inputs

Pinned Hub `28f6c78833065fd282f9064f92a9aa61875dd359`, package
`@electric-sql/pglite` `0.5.4`, installed with `npm ci --ignore-scripts` from a
`git archive` of the pinned commit. Digests equal the retained inventory
`evidence/phase2/hub-embedded-retained-package-sha256.txt`.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `dist/pglite.wasm` | 10,087,563 | `a20a37e2eb30553ae44f728001f0ead1b32cdcd53ab21f81fa2117b2947ef599` |
| `dist/initdb.wasm` | 395,059 | `aa134fde5c96733ff9ab9644f409d9337ae7b3f42a66914a4dad2beec03824df` |
| `dist/pglite.data` | 6,293,225 | `e39943c245ec32c36ed89bd5000229c8f3874983db93b7081d949155aaefaebf` |
| `dist/index.js` (glue) | 463,196 | `d346708dbb8a67e6b1e27ae7187b3172c3c51c23fdfe820c9a1e3f5c6f8e170f` |

Probe host: Node `v26.7.0`, `/usr/local/Cellar/node/26.7.0/bin/node`, SHA-256
`9bc2ac1e3fbfe4e9c3e2194283861d3cc7f07daa7a22a59be6ae79229922f540`, darwin/x64.

Reproduce (prints the JSON report on stdout):

```sh
node scripts/phase2/pglite-rust-host-import-trace.mjs \
  "$FIXTURE/node_modules/@electric-sql/pglite" "$FIXTURE/drizzle"
```

`$FIXTURE` is the `npm ci` checkout made exactly as in
`scripts/phase2/hub-embedded-retained.test.sh`. The probe runs the unchanged
glue and wraps every imported function with a call counter before
instantiation. Workload: fresh `initdb` and open, all 49 historical migrations
in one transaction (`migrate:49:49`), a typed parameter query, a rolled-back
transaction, a structured error, insert, close, reopen of the same directory,
read back (`reopen:durable`), close.

## Import totals

Facts from `WebAssembly.Module.imports`:

| Module | `env` functions | `env` globals | `env` memory | `env` table | WASI functions | `GOT.mem` | Total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `pglite.wasm` | 116 | 3 | 1 | 1 | 13 | 1 | 135 |
| `initdb.wasm` | 35 | 3 | 1 | 1 | 9 | 1 | 50 |

Correction to earlier wording: the "121 custom `env` functions" in
`hub-embedded-engine-selection.json` and the compatibility-exception record
count all 121 `env` imports, of which 116 are functions. The initdb "40 more
`env` functions" are 40 `env` imports, 35 of them functions, and 39 of the 40
are also imported by the main module. The only initdb-only import is
`env._mktime_js`. The union is 136 distinct imports.

Workload calls 62 of the 130 distinct imported functions. The other 68 are
unreached by this workload but stay in the contract because the glue
provides them and later SQL paths may reach them.

## Classification summary

| Class | Imports |
| --- | ---: |
| Dynamic linking | 4 |
| Dynamic linking / memory | 1 |
| Entropy | 1 |
| Environment | 2 |
| Exceptions (SjLj dispatch) | 57 |
| Filesystem | 40 |
| Memory | 5 |
| Memory + filesystem | 2 |
| Network (Emscripten SOCKFS / DNS) | 10 |
| Process exit and abort | 5 |
| Timers and clock | 3 |
| Timers and clock (time zone) | 4 |
| Timers and signals | 2 |

## Full import table

`main calls` and `initdb calls` are counts from the probe workload; `absent`
means the module does not import it; globals, memory and table show `null`.

| Import | Kind | main calls | initdb calls | Class | Rust host obligation |
| --- | --- | ---: | ---: | --- | --- |
| `GOT.mem.__heap_base` | global | absent | absent | Dynamic linking / memory | Mutable i32 global, value 11373728 (glue constant) |
| `env.__assert_fail` | function | 0 | absent | Process exit and abort | Typed exit trap carrying status; abort trap |
| `env.__call_sighandler` | function | 1 | 0 | Timers and signals | Deadline checked at host re-entry, then call the handler through the table |
| `env.__indirect_function_table` | table | absent | absent | Dynamic linking | Host-created table (initial 7367), table base 1 |
| `env.__memory_base` | global | absent | absent | Memory | Immutable i32 global 1024 |
| `env.__stack_pointer` | global | absent | absent | Memory | Mutable i32 global 11373728 |
| `env.__syscall__newselect` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_accept4` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_bind` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_chdir` | function | 6 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_chmod` | function | 0 | 4 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_connect` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_dup` | function | 6000 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_dup3` | function | 2 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_faccessat` | function | 565 | 6 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fadvise64` | function | 0 | 998 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fallocate` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fchmod` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fchmodat2` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fchown32` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fchownat` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fcntl64` | function | 0 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fdatasync` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_fstat64` | function | 1 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_ftruncate64` | function | 10 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_getcwd` | function | 0 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_getdents64` | function | 232 | 515 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_ioctl` | function | 9 | 8 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_listen` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_lstat64` | function | 2 | 1 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_mkdirat` | function | 2 | 25 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_newfstatat` | function | 0 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_openat` | function | 6287 | 2694 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_pipe` | function | 6 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_readlinkat` | function | 39 | 7 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_recvfrom` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_renameat` | function | 19 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_rmdir` | function | 0 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_sendto` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_socket` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.__syscall_stat64` | function | 777 | 633 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_statfs64` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_symlinkat` | function | 0 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_truncate64` | function | 400 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_unlinkat` | function | 652 | 1 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__syscall_utimensat` | function | 0 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `env.__table_base` | global | absent | absent | Dynamic linking | Host-created table (initial 7367), table base 1 |
| `env._abort_js` | function | 0 | 0 | Process exit and abort | Typed exit trap carrying status; abort trap |
| `env._dlopen_js` | function | 3 | absent | Dynamic linking | Load side module from the virtual filesystem, parse `dylink.0`, relocate, resolve GOT |
| `env._dlsym_js` | function | 18 | absent | Dynamic linking | Load side module from the virtual filesystem, parse `dylink.0`, relocate, resolve GOT |
| `env._emscripten_runtime_keepalive_clear` | function | 0 | 0 | Process exit and abort | Typed exit trap carrying status; abort trap |
| `env._emscripten_throw_longjmp` | function | 0 | 0 | Exceptions (SjLj dispatch) | Raise the typed longjmp trap |
| `env._gmtime_js` | function | 0 | absent | Timers and clock (time zone) | JavaScript `Date` local-time semantics from the host zone |
| `env._localtime_js` | function | 0 | 604 | Timers and clock (time zone) | JavaScript `Date` local-time semantics from the host zone |
| `env._mktime_js` | function | absent | 2 | Timers and clock (time zone) | JavaScript `Date` local-time semantics from the host zone |
| `env._mmap_js` | function | 11 | 0 | Memory + filesystem | Allocate with exported `emscripten_builtin_memalign`, read or msync through the filesystem |
| `env._munmap_js` | function | 0 | 0 | Memory + filesystem | Allocate with exported `emscripten_builtin_memalign`, read or msync through the filesystem |
| `env._setitimer_js` | function | 6 | 0 | Timers and signals | Deadline checked at host re-entry, then call the handler through the table |
| `env._tzset_js` | function | 0 | 1 | Timers and clock (time zone) | JavaScript `Date` local-time semantics from the host zone |
| `env.emscripten_date_now` | function | 5884 | 3 | Timers and clock | Host wall clock and monotonic clock |
| `env.emscripten_get_heap_max` | function | 0 | absent | Memory | `memory.grow` up to 2 GiB, same growth formula |
| `env.emscripten_get_now` | function | 6 | 0 | Timers and clock | Host wall clock and monotonic clock |
| `env.emscripten_resize_heap` | function | 6 | 0 | Memory | `memory.grow` up to 2 GiB, same growth formula |
| `env.exit` | function | 8 | 0 | Process exit and abort | Typed exit trap carrying status; abort trap |
| `env.getaddrinfo` | function | 8 | 1 | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.getnameinfo` | function | 0 | absent | Network (Emscripten SOCKFS / DNS) | Same errno or result codes as the glue; no host socket is opened |
| `env.invoke_di` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_i` | function | 1318 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_id` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ii` | function | 31022 | 19663 | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iii` | function | 9172 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiii` | function | 12757 | 19663 | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiii` | function | 15920 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiii` | function | 1296 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiii` | function | 589 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiiii` | function | 3 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiiiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiiiiiiiiiiiiii` | function | 2 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiiiji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiiij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iiji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_iijj` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ijiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ijiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ijji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_j` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_ji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jii` | function | 2 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jiiii` | function | 8 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jiiiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_jij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_v` | function | 1956 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vi` | function | 16724 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vid` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vii` | function | 3589 | 0 | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viii` | function | 6709 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiii` | function | 29 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiii` | function | 156 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiiii` | function | 1541 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiiiiii` | function | 104 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiiiiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiiiiiiiiii` | function | 185 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiiji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viiji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viijii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viijiiii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vij` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_viji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vijiji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vijjii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vj` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vji` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.invoke_vjii` | function | 0 | absent | Exceptions (SjLj dispatch) | Host re-enters the indirect table; a longjmp unwinds as a typed trap and sets `setThrew(1,0)` |
| `env.memory` | memory | absent | absent | Memory | Host-created memory, 2048 initial and 32768 maximum pages |
| `wasi_snapshot_preview1.clock_time_get` | function | 0 | absent | Timers and clock | Host wall clock and monotonic clock |
| `wasi_snapshot_preview1.environ_get` | function | 3 | 1 | Environment | Emscripten ENV defaults plus PGlite preRun values |
| `wasi_snapshot_preview1.environ_sizes_get` | function | 3 | 1 | Environment | Emscripten ENV defaults plus PGlite preRun values |
| `wasi_snapshot_preview1.fd_close` | function | 10688 | 2688 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_fdstat_get` | function | 11204 | 1 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_pread` | function | 1063 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_pwrite` | function | 3459 | absent | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_read` | function | 1926 | 1844 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_seek` | function | 7673 | 0 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_sync` | function | 496 | 998 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.fd_write` | function | 1879 | 18678 | Filesystem | Virtual filesystem: memory tree plus host passthrough for the data directory |
| `wasi_snapshot_preview1.proc_exit` | function | 0 | 0 | Process exit and abort | Typed exit trap carrying status; abort trap |
| `wasi_snapshot_preview1.random_get` | function | 0 | absent | Entropy | Host CSPRNG |

## Linking constants supplied by the glue

Facts read from the pinned glue and the `dylink.0` custom sections. Both
modules are Emscripten 3.1.74 position-independent modules; the host creates
memory and table and supplies base globals.

| Module | `dylink.0` memory size / align | `dylink.0` table size | Memory | Table initial | `__memory_base` | `__stack_pointer` and `__heap_base` | `__table_base` |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: |
| `pglite.wasm` | 2,984,084 / 2^12 | 7,366 | 2048 initial, 32768 maximum pages (created by `PGlite`) | 7,367 | 1024 | 11,373,728 | 1 |
| `initdb.wasm` | 139,320 / 2^4 | 143 | 1024 initial (64 MiB), 32768 maximum pages | 144 | 1024 | 205,888 | 1 |

After instantiation the glue runs `__wasm_apply_data_relocs` and then
`__wasm_call_ctors`; exported data addresses are relocated by 1024.

## Side modules (dynamic linking is in the contract)

The workload calls `_dlopen_js` three times and `_dlsym_js` 18 times. Loaded
libraries, read from the `struct dso` path at offset 36:

| Library | When (inferred from call order) | Bytes | `env` functions | `GOT.mem` | `GOT.func` | `dylink.0` memory / table |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| `/pglite/lib/postgresql/dict_snowball.so` | initdb text-search setup | 581,992 | 22 | 1 | 1 | 268,708 / 159 |
| `/pglite/lib/postgresql/plpgsql.so` | initdb, then the first `DO` block in a server instance (the migrations) | 155,470 | 255 | 18 | 33 | 31,384 / 38 |

Both libraries are bytes inside `pglite.data`, not separate package files.
The host must implement the glue's `loadWebAssemblyModule`: parse `dylink.0`,
allocate data with exported `calloc` after runtime start, grow the shared
table, resolve `env` functions to main-module exports or host functions,
resolve `GOT.mem` to relocated data addresses and `GOT.func` to table slots,
then run relocations and constructors. `GOT.mem` includes the SjLj state
(`__THREW__`, `__threwValue`, `PG_exception_stack`), so the side module and
main module share one exception protocol through the host `invoke_*` imports.
Resolved symbols: `Pg_magic_func`, `_PG_init`, `dsnowball_init`,
`dsnowball_lexize`, `plpgsql_call_handler`, `plpgsql_inline_handler`,
`plpgsql_validator`, and their `pg_finfo_*` records.

## Glue duties outside the import list

These behaviors live in JavaScript and are part of the exact contract.

| Duty | Observed glue behavior | Rust host obligation |
| --- | --- | --- |
| File package | `pglite.data` holds 699 files with offsets listed in the glue (`loadPackage` metadata) and is unpacked into memory under `/pglite` | Read the same bytes and the same offset table; no re-packaging |
| Filesystem layers | MEMFS root; NODEFS mounted at `/pglite/data` with `root` = host data directory; `/dev/blob` device; PROXYFS mounts `/pglite` of the main module into the initdb module | One virtual filesystem with per-module descriptor tables; host passthrough only under the data directory |
| Durability | NODEFS has no `fsync` stream operation, so `fd_sync` is a no-op; the server runs with `-F` | Same: no host `fsync`; crash survival is process-crash only, as in the original |
| Environment | `HOME=/home/postgres`, `USER`/`LOGNAME`/`PGUSER=postgres`, `PGDATA=/pglite/data`, `PGDATABASE=postgres`, `LANG`/`LC_COLLATE`/`LC_CTYPE=en_US.UTF-8`, `TZ`/`PGTZ=UTC`, `PGCLIENTENCODING=UTF8`, `ICU_DATA=/pglite/icu`; initdb gets `PGDATA`, `HOME`, `USER`, `LOGNAME`, `ICU_DATA` only | Same ordered environment per module |
| initdb | A second main-module instance on a memory filesystem; `initdb.wasm` runs `--allow-group-access --encoding UTF8 --locale=C.UTF-8 --locale-provider=libc --auth=trust`; its `system`/`popen`/`pclose` callbacks run the main module `main` after restoring a snapshot of main memory; stdin and stdout pass through `/pglite/pgstdin` and `/pglite/pgstdout` | Two instances in one store, same callback protocol and memory snapshot restore |
| Handoff to disk | The initdb data directory is dumped to an uncompressed tar and unpacked into `/pglite/data`; regular files get `utime` from the tar mtime in seconds, which the glue then treats as milliseconds | Reproduce the same files, modes and the same 1970-epoch file mtimes (observed: `pg_hba.conf` dated 21 Jan 1970) |
| Server start | `callMain(["--single","-F","-O","-j","-c","search_path=public","-c","exit_on_error=false","-c","log_checkpoints=false","-c","max_worker_processes=0","-c","max_parallel_workers=0","-c","max_parallel_workers_per_gather=0","-c","io_method=sync","-c","max_parallel_maintenance_workers=0","-D","/pglite/data","postgres"])` must return 99, then `_pgl_startPGlite`; a client message whose first byte is 0 is a startup packet and goes to `_ProcessStartupPacket` and `_pgl_sendConnData` | Same argument vector and return checks |
| Wire loop | Read and write callbacks registered with `addFunction` and `_pgl_set_rw_cbs`; each message batch loops `_PostgresMainLoopOnce`; an error exits with status 100 and the glue calls `_PostgresMainLongJmp`; then `_PostgresSendReadyForQueryIfNecessary` and `_pgl_pq_flush` | Host functions placed in the table; typed exit trap for status 100 |
| Query client | `query` sends Parse, Describe S, Bind, Describe P, Execute, then Sync, each as a separate batch; parameters use the described type serializers; rows use the PGlite text parsers; the retained host then maps values to `IpcValue` | Same message sequence and the combined parser plus `encodeValue` mapping, including JavaScript number and `Date` formatting |
| Close | `_pgl_setPGliteActive(0)`, Terminate message, `_pgl_run_atexit_funcs`, filesystem quit, `_emscripten_force_exit(0)` | Same order |
| Time zone | `_localtime_js` uses the host local zone through JavaScript `Date`; initdb probes it 604 times to choose `timezone` and `log_timezone` | Same local-time results from the host zone database |

Measured time-zone dependency (fact): on this host (`/etc/localtime` =
`Europe/London`) a fresh original data directory gets
`timezone = 'Etc/GMT0'`; with `TZ=America/New_York` it gets
`timezone = 'Etc/GMT+5'`. A Rust host that ignored the host zone would write
a different `postgresql.conf` and change `now()` rendering.

## Runtime selection

Selected: **Wasmtime `47.0.4`** (Cranelift, `runtime`, `std`,
`parallel-compilation`), with `wasmparser` for `dylink.0`. Cargo resolves
`47.0.4` as the newest release compatible with the pinned Rust `1.94.0`.

| Requirement | How Wasmtime meets it under `unsafe_code = "forbid"` |
| --- | --- |
| Host-created imported memory, table, mutable globals | `Memory::new`, `Table::new`, `Global::new` are safe |
| 116 + 13 host functions, 56 `invoke_*` signatures | `Linker::func_new` with a dynamic `FuncType` per import; one generic `invoke` body |
| SjLj: catch longjmp, rethrow exit | Host functions return a typed error; it unwinds through Wasm frames; the outer `invoke_*` downcasts it, restores the stack, calls `setThrew(1,0)`; exit errors propagate |
| Re-entrant host to guest calls | `Caller` gives the table; `Func::call` from inside a host function is supported |
| `addFunction` callbacks | `Table::grow` with a host `Func` |
| Side modules | Instantiate further modules in the same `Store` against the same memory and table; cross-instance function imports |
| Memory growth to 2 GiB | `memory.grow` from guest and `Memory::grow` from host |
| No unsafe in Spocky code | All APIs above are safe. Compiled code is reused through the built-in `cache` feature (`Config::cache`), a safe API; the unsafe code it needs stays inside the dependency, so `unsafe_code = "forbid"` holds in Spocky crates. The host does not call `Module::deserialize` directly |

Rejected alternatives: plain WASI hosts (for example `wasmtime-wasi`) do not
supply the 116 custom `env` functions; an interpreter such as `wasmi` would
meet the API needs but adds a large CPU cost to every `invoke_*` and query;
the Node child is the exception being removed.

Spike measurement (fact, scratch probe outside the repository, built through
the shared build gate with `CARGO_BUILD_JOBS=2` on a heavily loaded host):
Wasmtime `47.0.4` release build compiles `pglite.wasm` in 16.16 s,
`initdb.wasm` in 0.35 s and `plpgsql.so` in 0.42 s, all with
`#![forbid(unsafe_code)]`. Import types it reports match the glue constants:
`env.memory` minimum 2048 and maximum 32768 pages, table minimum 7366,
`__memory_base` and `__table_base` immutable `i32`, `__stack_pointer` and
`GOT.mem.__heap_base` mutable `i32`. The `dylink.0` reader is `wasmparser`
`0.252.0`, the version Wasmtime itself uses.

Inference: compile time is the main startup cost. The host compiles each
module once per process, shares it across opens, and enables the Wasmtime
compilation cache so later processes load compiled code instead of
recompiling. Startup and per-query cost against the retained Node host are
measured in step 2, not assumed.

## Platform matrix

Lead review of this checkpoint: GO with conditions, desktop only.

| Platform | Call | Reason |
| --- | --- | --- |
| macOS (x64, arm64) | GO | Cranelift targets both; measured on darwin/x64 only so far |
| Linux (x64, arm64) | GO | Cranelift targets both; not yet measured |
| Windows (x64) | GO with risks | NODEFS stat emulation reads POSIX mode bits that Windows does not keep; PostgreSQL `checkDataDir` expects `0700` or `0750` on the data directory; both need measured parity |
| iOS | Not covered | iOS forbids writable executable memory, so Wasmtime would need its Pulley interpreter. Pulley performance is unmeasured, which conflicts with the reason `wasmi` was rejected above; the 2 GiB linear-memory maximum also exceeds iOS memory limits |
| Browser | Not covered | Wasmtime cannot run in a browser. The browser keeps PGlite's own JavaScript host, which must be recorded as its own compatibility exception |

## Durability

Fact: NODEFS has no `fsync` stream operation, so `fd_sync` returns 0 without
syncing, and the server runs with `-F` (`fsync = off`). The Node host
therefore never flushes data to stable storage. The Rust host matches this,
so durability parity covers process crashes only (data in the operating
system page cache survives). Power loss and kernel crash durability are not
provided by either host and are not claimed.

## Decision

**GO with conditions, desktop only** (see the platform matrix). Every one of the 136 imports maps to a host obligation that the safe
Wasmtime API can provide, and the two side modules use the same linking
contract. Nothing requires rebuilding Postgres or modifying the pinned bytes.

Reasons:

1. Imports are ordinary Emscripten runtime services (filesystem, time,
   environment, memory growth, SjLj dispatch, dynamic linking); none needs a
   JavaScript engine.
2. Every Wasmtime capability needed is available without `unsafe` in Spocky
   code.
3. The pinned glue is readable and fixes every constant the host must
   reproduce (bases, table and memory sizes, file package offsets, argument
   vectors, environment).

Risks carried into step 2 (each becomes a differential case, not an
assumption):

| Risk | Why it matters | How step 2 checks it |
| --- | --- | --- |
| Filesystem emulation breadth | 40 filesystem imports plus MEMFS, NODEFS and PROXYFS semantics decide the bytes and metadata on disk | Compare the data directory tree (names, sizes, modes) and catalog output with the original |
| Host-zone local time | Decides `timezone` in `postgresql.conf` | Compare `postgresql.conf` and `now()` rendering on the same host zone |
| JavaScript value formatting | Typed rows go through JavaScript numbers and `Date` | Compare typed-row output of the same queries |
| Side-module linking | `plpgsql` runs every `DO` block in the migrations | Replay all 49 migrations and compare catalogs |
| Unreached imports | 68 of 130 imported functions are not called by the workload, including all socket calls | Implement them from the glue; list any that are not exercised as untested |
| Startup cost | 16 s compile on this loaded host | Measure open-to-ready and query latency for both hosts |
| SjLj longjmp path | `_emscripten_throw_longjmp` had 0 calls in the probe workload, so no `PG_TRY` and `PG_CATCH` longjmp ran | Force an error inside a PL/pgSQL `BEGIN ... EXCEPTION` block and count the longjmp calls on both hosts |
| Native stack depth | Every `invoke_*` re-enters Wasm from the host and grows the native stack | Run the store on a dedicated large-stack thread and set `max_wasm_stack` |
