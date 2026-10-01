# PGlite Rust host end-to-end spike

Task `P2-PGLITE-RUST-HOST-01`, lead review item B. The Rust host
(`crates/spocky-pglite-host`) runs the pinned `pglite.wasm`, `initdb.wasm`
and the side modules in `pglite.data` unchanged under Wasmtime `47.0.4`, with
no Node process. This spike compares it with the original JavaScript host on
the same pinned package (Hub `28f6c78833065fd282f9064f92a9aa61875dd359`,
`@electric-sql/pglite` `0.5.4`, PostgreSQL `18.3`, digests as in
`pglite-rust-host-contract.md`).

## Run

```sh
PASEO_HUB_BASELINE_ROOT=/path/to/.baselines/hub \
  sh scripts/phase2/pglite-rust-host-spike.sh
```

The runner makes a fresh `npm ci` fixture from the pinned commit and runs
`scripts/phase2/pglite-rust-host-spike.mjs` twice on Node `v26.7.0`
(`/usr/local/Cellar/node/26.7.0/bin/node`), each on its own fresh directory
(`node` and the control run `node2`). It copies the first Node data
directory, builds `crates/spocky-pglite-host/examples/spike.rs` through the
shared build gate and runs it twice with one compilation cache directory:
first with the cache empty (`rust`), then warm (`rust-warm`). It then
compares the reports (`pglite-rust-host-spike-compare.mjs`), decodes
`global/pg_control` and the first WAL segment of `node`, `node2` and the
Rust snapshot (`pglite-rust-host-spike-control.mjs`), and names the struct
field of each WAL byte that differs only between Node and Rust
(`pglite-rust-host-spike-wal-decode.mjs`). Host: darwin/x64, `Europe/London`
zone, heavily loaded shared machine.

Both hosts run the same workload on a fresh directory: open (initdb and
start), `select 1 as one`, a `DO` block whose `BEGIN ... EXCEPTION WHEN
division_by_zero` catches `1 / 0`, `select * from missing_table`, `create table
spike_marker`, a parameterized insert of `(1, 'spike marker')`, a settings
query, close. The Rust host then reopens its own directory and the copy of the
Node-made directory.

## Results (measured, 2026-10-01 18:31 to 18:36 local)

| Check | Node host | Rust host |
| --- | --- | --- |
| initdb | exit 0 | exit 0, empty stderr, 996 entries handed to the data directory |
| Compile `pglite.wasm` and `initdb.wasm`, cache empty | not applicable | 24,867 ms, release build |
| Compile, cache warm (second process, same cache directory) | not applicable | 580 ms |
| Open to ready (includes initdb) | not timed | 10,625 ms cold run, 5,807 ms warm run |
| `select 1 as one` | `[{"one":1}]` | one row, text `1` |
| PL/pgSQL `EXCEPTION` block | 1 `_emscripten_throw_longjmp` call | 1 `_emscripten_throw_longjmp` call, 1 caught by an `invoke_*` wrapper (`setThrew`), `NOTICE caught 22012` |
| `missing_table` error | `42P01`, `ERROR`, `relation "missing_table" does not exist` | same code, severity and message; reached through `exit(100)` and `PostgresMainLongJmp` (1) |
| Settings | `TimeZone = Etc/GMT0`, `server_version = 18.3` | same |
| Side modules | not counted in this workload | 3 `dlopen` and 18 `dlsym`, equal to the counts in the Node import trace (a different workload) |
| Close | ok | ok |
| Reopen own directory | not run | ok, `select 1` returns `1` |
| Reopen Node-made directory | not applicable | ok, reads `(1, 'spike marker')` written by Node |

The Node report has an `invokeCaughtLongjmp` field that the Node script never
increments, so its value 0 is not a measurement. Invoke catches and side
module loads are counted on the Rust side only.

The spike example prints raw PostgreSQL text. Typed value mapping runs in
`PgliteHost::query` and is covered by
`tests/host_runtime.rs::typed_query_round_trips_basic_values`; it is not part
of this comparison.

The cold and warm compile times come from one run each on a loaded machine.
An earlier run on the same day measured 30,267 ms cold and 1,055 ms warm.

## Data directory tree (fresh directory after the workload)

Compared fields: path, type, size, permission bits, file SHA-256, and mtime
class. Mtime values are wall-clock and are compared only by class: files the
tar handoff writes get the seconds-as-milliseconds 1970 dates, everything
else a real date.

| Field | Node versus Rust | Node versus Node (control) |
| --- | --- | --- |
| Entries | 1,001 and 1,001, no path on one side only | 1,001 and 1,001, no path on one side only |
| Type, size, mode | 0 differences | 0 differences |
| Mtime class | 0 differences | 0 differences |
| File bytes | 973 files byte-identical; `global/pg_control` and `pg_wal/000000010000000000000001` differ | 973 files byte-identical; the same two files differ |

At file level the Rust-versus-Node result equals the Node control: the same
two files differ and every other file is byte-identical. Inside those two
files the differing bytes are not the same set as in the control. The next two
sections decode them.

## `global/pg_control` (8,192 bytes)

Measured: 42 bytes differ Node versus Node, 43 Node versus Rust; 1 Node
versus Rust byte offset is outside the control set.

The control script reads `system_identifier` (offset 0) and
`checkPointCopy.time` (offset 104) at their PostgreSQL 18 `ControlFileData`
offsets and checks them against the values the original host decodes through
`pg_control_system()` and `pg_control_checkpoint()`. Both match for `node`,
`node2` and `rust`, and the CRC-32C stored at offset 292 matches the first 292
bytes in all three files. Every differing byte falls in a named field:

| Field (offset, size) | Node versus Node | Node versus Rust |
| --- | ---: | ---: |
| `system_identifier` (0, 8) | 4 | 5 |
| `time` (24, 8) | 1 | 1 |
| `checkPointCopy.time` (104, 8) | 1 | 1 |
| `mock_authentication_nonce` (257, 32) | 32 | 32 |
| `crc` (292, 4) | 4 | 4 |

The one offset outside the control set is byte 1, inside
`system_identifier` (Node versus Node differs at bytes 2 to 5 of it, Node
versus Rust at bytes 1 to 5). Decoded through SQL, the fields that differ are
`system_identifier`, `pg_control_last_modified` and `checkpoint_time`, in
both pairs; checkpoint LSN, redo LSN, next XID, next OID and timelines are
equal. The offsets of `time` and `mock_authentication_nonce` come from the
PostgreSQL 18 struct layout; they are consistent with the verified offsets
around them but were not checked against a decoded value.

## WAL segment `000000010000000000000001` (16 MiB)

Measured: 6,053 bytes differ Node versus Node, 6,650 Node versus Rust; 651
Node versus Rust byte offsets are outside the control set. The control script
walks the records of the `node` segment and classes each byte by the field it
belongs to:

| Class | Node versus Node | Node versus Rust |
| --- | ---: | ---: |
| `page.sysid` (page header system identifier) | 4 | 5 |
| `record.crc` | 3,043 | 3,095 |
| `checkpoint.time` | 13 | 13 |
| `xact.time` (commit timestamp) | 2,989 | 2,962 |
| `record.blockdata` | 1 | 2 |
| Standby `RUNNING_XACTS` main data | 3 | 3 |
| Transaction commit with `XLOG_XACT_HAS_INFO`, main data after the timestamp | 0 | 468 |
| Heap `INPLACE` main data | 0 | 57 |
| Standby `INVALIDATIONS` main data | 0 | 45 |

Of the 651 offsets outside the control set, 570 are in the last three
classes and 81 are in classes that also differ between the two Node runs.

The decoder lays out the 210 records of those three classes (196 commits, 8
heap in-place updates, 6 standby invalidation records) with the PostgreSQL 18
structs `xl_xact_commit` and `XactLogCommitRecord` parts, `xl_heap_inplace`,
`xl_invalidations` and the 16-byte `SharedInvalidationMessage` union. For
every record the fields tile the main data exactly and every invalidation
message has a known id. Every byte that differs in those records is one of:

| Record: field | Bytes |
| --- | ---: |
| commit: `xact_time` | 766 |
| commit: snapshot message padding after the 1-byte id | 434 |
| commit: catcache message, union bytes 12 to 15 not in the message | 34 |
| heap in-place: catcache message, unused union bytes | 42 |
| heap in-place: relcache message, unused union bytes | 15 |
| standby invalidations: catcache message, unused union bytes | 29 |
| standby invalidations: relcache message, unused union bytes | 16 |

No differing byte falls in a database, relation, hash, count, XID or
locator field. The same record classes changed between runs: in an earlier
run Node versus Node also differed in standby `INVALIDATIONS` (199 bytes) and
heap `INPLACE` (54 bytes), and Node versus Rust differed in standby
`RUNNING_XACTS` with no Node versus Node bytes there.

Inference, not traced: PostgreSQL fills only the named members of an
invalidation message, so padding and unused union bytes keep whatever the
stack held, which depends on earlier host results such as times and random
values. This would explain why these bytes differ between any two runs. It
was not confirmed by tracing memory in either host.

Not decoded: the 2 `record.blockdata` bytes (1 in the control) and the 81
offsets in control classes that differ at other positions.

## Stack setting

`PgliteHost::open` runs the store on a library-owned thread with a 256 MiB
native stack (`host::run_on_store_thread`, test
`store_work_runs_on_the_library_thread`); the spike example uses the same
function. `max_wasm_stack` is 64 MiB; Wasmtime requires `async_stack_size` to
be at least that, so it is set to 65 MiB although no asynchronous Wasm runs.
No stack overflow occurred in this workload. Deep-recursion limits were not
probed.

## Request deadline

Wasm runs with Wasmtime epoch interruption, ticked every 10 ms. With
`request_timeout` set to 2 s, `select pg_sleep(30)` returns `Timeout` and the
next request returns `Closed`
(`tests/host_runtime.rs::request_deadline_stops_runaway_wasm_and_closes_the_host`).
The interrupted store is stopped without `close`, like killing a timed-out
child.

## Defects found and fixed during the spike

| Defect | Effect | Fix |
| --- | --- | --- |
| `getTempRet0` and `setTempRet0` missing from the host namespace | every `plpgsql` call aborted with `TypeError: getTempRet0 is not a function`, the wire loop swallowed it, and later statements failed (`ERRORDATA_STACK_SIZE exceeded`) | the two glue functions call `_emscripten_tempret_get` and `_emscripten_tempret_set`, as in the glue |
| `FS.analyzePath("")` treated as missing | the tar handoff tried to `mkdir("")` | `exists` is true whenever path lookup does not throw, as in the glue |

## Known differences and gaps (not hidden)

- `pgB.close()` (the memory-filesystem instance used for initdb) is not run;
  the glue starts it without awaiting it and its state is discarded.
- The wire loop swallows exceptions other than `exit(100)` as the glue does,
  keeps the newest 32 in its log, and stops after 10,000 consecutive
  swallowed exceptions without input progress instead of looping forever.
- `statfs` returns the Emscripten defaults, not Node's host values (0 calls
  in the import trace).
- Socket `connect` returns `EHOSTUNREACH` and `listen` aborts, matching Node
  without the `ws` module; none is reached by PostgreSQL in single-user mode.
- Node-side counts of invoke catches and side module loads for this workload
  are not instrumented.
- Not yet run: the 49 migrations, catalog parity (50 tables, 537 names, 49
  journal rows), the 17 retained-host cases, crash survival.

## Raw artifacts

Committed under `evidence/phase2/pglite-rust-host-spike-raw/`:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `comparison.json` | 2,505 | `99e6f8478b82ce2a84cdc6febfa08271a9e1ab55ac22ee1b8bc01e6a7add0350` |
| `control-node-vs-node.json` | 1,866 | `02e4b8882983ce635bf49b17bd061f3478cb8844f29e4cdce4093fceb5995cf7` |
| `control-fields.json` | 809,587 | `47284b75052ded3d4b7d20b2d59ffe464f2fb39e3326c8ce46753e9c146117de` |
| `wal-fields.json` | 509 | `6a49f3ec7a28a521a8da6b63b2b778422d035256062cf6e44438bb2a70f99f77` |

Untracked, `evidence/raw/pglite-rust-host-spike/` (same run):

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `node.json` | 216,896 | `f1ab447cfee564be88e4ae90699760844739c584c99cd9634d88c141521854c2` |
| `node2.json` | 216,896 | `55a5654f583ad540717f4abab94f49e566972f4385fc974702ef2bca7806a21c` |
| `rust.json` | 433,832 | `407e8488a163e26a0bc7ccd4ce193a13284a714e3a9cc93b535e426da51215e9` |
| `rust-warm.json` | 433,829 | `d7d7f7e01e20bbcafc29d176159a9b245b0f6a0774723f71f06c2184f65770a3` |
