# PGlite Rust host: original-versus-Rust differential

Task `P2-PGLITE-RUST-HOST-01`, step 2. The Rust host runs behind the
retained adapter in place of the Node child, and is compared with the
original Hub runtime on the pinned baseline (Hub
`28f6c78833065fd282f9064f92a9aa61875dd359`, `@electric-sql/pglite` `0.5.4`,
PostgreSQL `18.3`).

## Run

```sh
PASEO_HUB_BASELINE_ROOT=/path/to/.baselines/hub \
  sh scripts/phase2/pglite-rust-host-differential.sh
```

Measured 2026-10-01 at commit `c591ee9`, darwin/x64, on a heavily loaded
shared machine.

The runner builds `spocky-pglite-host-child` (release), a Rust binary that
speaks the framed protocol of `scripts/phase2/hub-embedded-retained-host.mjs`.
`SPOCKY_NODE` names that binary, so the unchanged adapter
(`crates/spocky-hub-pilot/src/retained_pglite.rs`) starts it where it would
start Node. No file in `spocky-hub-pilot` was changed. Every child shares one
Wasmtime compilation cache directory.

| Side | What runs |
| --- | --- |
| Original | the Hub's own in-process capture (`hub-embedded-pglite.mjs capture` on the baseline runtime), not the Node retained child |
| Candidate | `hub-retained-pglite-evidence capture` with the Rust child |
| Runtime tests | `cargo test -p spocky-hub-pilot --test retained_pglite_runtime -- --test-threads=1` with the Rust child; the Node run of the same 17 tests is `hub-embedded-retained-tests.log` |

The runner ends with `scripts/phase2/pglite-rust-host-differential-check.sh`.
It exits 1 when any check fails. `pglite-rust-host-differential-check.test.sh`
shows that each single mismatch, count drop, failed test or missing input
fails the gate.

## Results (measured)

| Check | Result |
| --- | --- |
| Catalog tables | 50 and 50, equal lists |
| Constraint and index names | 537 and 537, equal lists |
| Migration journal rows | 49 and 49, equal hash and `createdAt` values |
| Restart, cross-process rejection, transaction rollback, stale-owner recovery | true on both sides |
| Historical resume (Rust only) | 1 journal row before, 48 applied, 49 after, user row kept |
| Committed write survives a child crash with a lost reply (Rust only) | true |
| Injected failure after a durable close, data kept (Rust only) | true |
| Partial migration rollback (Rust only) | 0 journal rows, 0 public tables, 0 probe rows; error `42601` |
| Error payloads (`failures`, `migrationError`) | equal to the Node retained candidate, `hub-embedded-retained-candidate.json` |
| 17 retained runtime tests with the Rust child | 17 passed, 0 failed, 142.41 s |
| Gate | `differential checks passed` |

Crash survival (tests 5, 6 and 15 and the capture's
`committedWriteCrashRecovery`) is measured for process crashes only. As with
the Node host, nothing is flushed to stable storage (see the durability
section of `pglite-rust-host-contract.md`).

## Identity

The adapter's identity fields are named for Node. The Rust child fills them
as follows: `nodeVersion` is `spocky-pglite-host 0.1.0 (wasmtime 47.0.4)`,
`nodeExecutable` and `nodeExecutableSha256` describe the child binary, and
`adapterDependencies` is `["spocky-pglite-host"]`. `os` and `arch` use Node's
names (`darwin`, `x64`), as the Node child does. The package, version and
dependency fields come from the pinned package, the same as Node.

## Known differences and gaps (not hidden)

- The lock tests (5, 7, 8, 9 and 17) exercise the adapter's
  `DataDirectoryLock` in `spocky-hub-pilot`, not code in the new host.
- The Node child closes the database on `SIGINT`, `SIGTERM` and `SIGHUP` and
  exits with 128. The Rust child has no signal handler (the crate forbids
  `unsafe` and adds no signal crate), so a signal ends it without `close`.
  The next open then runs PostgreSQL crash recovery. No measured test covers
  this case.
- The `INVALID_JSON` reply carries serde's error text, not V8's
  `JSON.parse` message. The code and reply shape are the same.
- A parameter whose tag is known but whose value has the wrong JSON type is
  rejected with `INVALID_VALUE`; Node passes such a value to PGlite unchanged.
  The adapter never sends one.
- The original side is the in-process Hub capture. The Node retained child
  and the Rust child are compared only through the committed Node candidate's
  error payloads and the Node test log.

## Evidence

Files have the temporary fixture path replaced by `<fixture>`.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `pglite-rust-host-differential-original.json` | 51,030 | `92a873c2b3fedec61aa0fcf499d4cbe5d1964b90d47eb81e3a82a770ae97f508` |
| `pglite-rust-host-differential-candidate.json` | 122,150 | `f323a08765ddaedd8c384259b944e8ba3f4dbc757e2693510e14087fd7c9f6c4` |
| `pglite-rust-host-differential-comparison.json` | 2,415 | `87c60d7427b7fa1d20492213da6bb53decfd524db7a26916827a3333a09a0caf` |
| `pglite-rust-host-differential-tests.log` | 1,537 | `a83a528e9bde5b7817df36a31b3e832c5ad9c8b4bb180d69e5b9da05dbd7e53e` |
