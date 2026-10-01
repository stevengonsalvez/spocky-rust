# PGlite Rust host end-to-end spike

Task `P2-PGLITE-RUST-HOST-01`, lead review item B. The Rust host
(`crates/spocky-pglite-host`) runs the pinned `pglite.wasm`, `initdb.wasm`
and the side modules in `pglite.data` unchanged under Wasmtime `47.0.4`, with
no Node process. This spike compares it with the original JavaScript host on
the same pinned package (Hub `28f6c78833065fd282f9064f92a9aa61875dd359`,
`@electric-sql/pglite` `0.5.4`, digests as in
`pglite-rust-host-contract.md`).

## Run

```sh
PASEO_HUB_BASELINE_ROOT=/path/to/.baselines/hub \
  sh scripts/phase2/pglite-rust-host-spike.sh
```

The runner makes a fresh `npm ci` fixture from the pinned commit, runs
`scripts/phase2/pglite-rust-host-spike.mjs` on Node `v26.7.0`
(`/usr/local/Cellar/node/26.7.0/bin/node`), copies the Node data directory,
builds and runs `crates/spocky-pglite-host/examples/spike.rs` through the
shared build gate, and compares the reports with
`scripts/phase2/pglite-rust-host-spike-compare.mjs`. Host: darwin/x64,
`Europe/London` zone, heavily loaded shared machine.

Both hosts run the same workload on a fresh directory: open (initdb and
start), `select 1 as one`, a `DO` block whose `BEGIN ... EXCEPTION WHEN
division_by_zero` catches `1 / 0`, `select * from missing_table`, `create table
spike_marker`, a parameterized insert of `(1, 'spike marker')`, a settings
query, close. The Rust host then reopens its own directory and the copy of the
Node-made directory.

## Results (measured, 2026-10-01, rerun after the lint commits)

| Check | Node host | Rust host |
| --- | --- | --- |
| initdb | exit 0 | exit 0, empty stderr, 996 entries handed to the data directory |
| Open to ready | not timed | 7,677 ms (includes initdb) |
| Compile `pglite.wasm` and `initdb.wasm` | not applicable | 9,316 ms, release build, loaded host, no compilation cache |
| `select 1 as one` | `[{"one":1}]` | one row, text `1` |
| PL/pgSQL `EXCEPTION` block | 1 `_emscripten_throw_longjmp` call | 1 `_emscripten_throw_longjmp` call, caught by an `invoke_*` wrapper (`setThrew`), `NOTICE caught 22012` |
| `missing_table` error | `42P01`, `ERROR`, `relation "missing_table" does not exist` | same code, severity and message; reached through `exit(100)` and `PostgresMainLongJmp` |
| Settings | `TimeZone = Etc/GMT0`, `server_version = 18.3` | same |
| Side modules | not counted | 3 `dlopen` and 18 `dlsym`, equal to the counts in the Node import trace |
| Close | ok | ok |
| Reopen own directory | not run | ok, `select 1` returns `1` |
| Reopen Node-made directory | not applicable | ok, reads `(1, 'spike marker')` written by Node |

Typed values are the raw PostgreSQL text here; PGlite's JavaScript value
mapping is not ported yet.

## Data directory tree (fresh directory after the workload)

Compared fields: path, type, size, permission bits, file SHA-256, and mtime
class. Mtime values are wall-clock and are compared only by class: files the
tar handoff writes get the seconds-as-milliseconds 1970 dates, everything
else a real date.

| Field | Result |
| --- | --- |
| Entries | 1,001 Node, 1,001 Rust, no path on one side only |
| Type, size, mode | 0 differences |
| Mtime class | 0 differences |
| File bytes | 973 files identical; `global/pg_control` and `pg_wal/000000010000000000000001` differ |

Control run: two Node runs of the same workload differ in exactly the same two
files and nothing else (fact, same comparison script). So the Rust-versus-Node
difference equals Node's own run-to-run variance. Inference: the variance is
the system identifier and checkpoint timestamps PostgreSQL derives from the
clock; this was not decoded byte by byte.

## Stack setting

The store runs on a dedicated thread with a 256 MiB native stack.
`max_wasm_stack` is 64 MiB; Wasmtime requires `async_stack_size` to be at
least that, so it is set to 65 MiB although no asynchronous Wasm runs. No
stack overflow occurred in this workload. Deep-recursion limits were not
probed.

## Defects found and fixed during the spike

| Defect | Effect | Fix |
| --- | --- | --- |
| `getTempRet0` and `setTempRet0` missing from the host namespace | every `plpgsql` call aborted with `TypeError: getTempRet0 is not a function`, the wire loop swallowed it, and later statements failed (`ERRORDATA_STACK_SIZE exceeded`) | the two glue functions call `_emscripten_tempret_get` and `_emscripten_tempret_set`, as in the glue |
| `FS.analyzePath("")` treated as missing | the tar handoff tried to `mkdir("")` | `exists` is true whenever path lookup does not throw, as in the glue |

## Known differences and gaps (not hidden)

- `pgB.close()` (the memory-filesystem instance used for initdb) is not run;
  the glue starts it without awaiting it and its state is discarded.
- The wire loop swallows exceptions other than `exit(100)` as the glue does,
  but stops after 10,000 consecutive swallowed exceptions without input
  progress instead of looping forever.
- `statfs` returns the Emscripten defaults, not Node's host values (0 calls
  in the import trace).
- Socket `connect` returns `EHOSTUNREACH` and `listen` aborts, matching Node
  without the `ws` module; none is reached by PostgreSQL in single-user mode.
- Not yet run: the 49 migrations, catalog parity (50 tables, 537 names, 49
  journal rows), the 17 retained-host cases, JavaScript typed-value mapping,
  crash survival.

## Checks

`cargo clippy --locked -p spocky-pglite-host --all-targets -- -D warnings`
passes with the workspace `all` and `pedantic` lints denied; remaining
allowances are item-level with a stated reason (decimal glue constants,
Date-range casts, single-port functions over 100 lines).
`cargo test --locked -p spocky-pglite-host --release --lib`: 6 passed, 0
failed. `cargo fmt -p spocky-pglite-host --check` passes.

## Raw artifacts (untracked, `evidence/raw/pglite-rust-host-spike/`)

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `node.json` | 216,896 | `c792aee1ad343dccbd9b89c4a0cb70f823be046d732c091a493ebb8a56cf62dc` |
| `rust.json` | 433,800 | `13e10a98dad96fb51487d8192b00cac94ba0f044b69bf1938ac53bfaabc6eb48` |
| `comparison.json` | 2,475 | `e88a60bb11a4faf0931484816d9c2e96dfe92f153924cca7a971267615159f9f` |
