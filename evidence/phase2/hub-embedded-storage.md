# Hub embedded storage closure checkpoint

Pinned Hub baseline `28f6c78833065fd282f9064f92a9aa61875dd359` uses PGlite
`0.5.4`. The differential queries both installed databases. It does not derive
expected tables or constraints from the Drizzle snapshot.

Run:

```sh
sh scripts/phase2/hub-embedded-differential.test.sh
scripts/phase2/hub-embedded-differential.sh
cargo test --locked -p spocky-hub-pilot --test embedded_sql_runtime
```

The installed table inventories match at 50 tables. Installed constraint and
index inventories do not match: PGlite reports 537 names and SQLite reports
209. SQLite has 17 names absent from PGlite; PGlite has 345 names absent from
SQLite. The raw comparison records both lists without snapshot filtering.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `pglite.json` | 51,030 | `92a873c2b3fedec61aa0fcf499d4cbe5d1964b90d47eb81e3a82a770ae97f508` |
| `rust.json` | 24,384 | `a8a451668adad104bb67118cec5110e420fe5ecc8ce761a398537a58b4e943fb` |
| `comparison.json` | 112,741 | `8ffb8e2135b14f0db481e2850e6894ad730d116a42fb2eb0c2fc31748f7d93aa` |

Thirteen targeted runtime tests pass. They include legacy whole-state reopen,
ordered migration-prefix resume, rejection without journal rewriting, and
transaction rollback after an injected migration interruption.

This checkpoint does not select SQLite as the exact engine. Observable pilot
operations match, but exact database parity remains false. Engine, schema,
dialect, migrations, relational state mutation, and PGlite crash-process
qualification remain blockers.

## Exact-engine selection result

Run:

```sh
gtimeout 900 sh scripts/phase2/hub-embedded-engine-selection.test.sh
gtimeout 900 scripts/phase2/hub-embedded-engine-selection.sh
```

The pinned package contains 301 files totaling 25,403,132 bytes. Its main Wasm
module is 10,087,563 bytes and imports 121 custom `env` functions, 13 WASI
functions, and one `GOT.mem` value. The initdb module imports another 40 custom
`env` functions. The public package exports JavaScript and CommonJS entry
points. Bare WASI does not supply the distributed runtime contract.

The executable probe runs the pinned JavaScript runtime and replays all 49
historical migrations. Restart, cross-process directory rejection,
transaction rollback, keyed-lock ordering, and stale-owner recovery pass.
Raw selection evidence is
`evidence/phase2/hub-embedded-engine-selection.json` (5,448 bytes, SHA-256
`8bb09943502b07dae40e71fe1848027f5d11b213af2a45d686092785d86901d3`).

Selection remains blocked. Shortest exact path retains the distributed
JavaScript glue and sends that runtime exception for review. A native host
requires root Rust dependencies plus implementation of the missing host
contract. Both exceed this checkpoint. SQLite remains rejected as the exact
engine. The probe emits the required compatibility-exception record with status
`required-not-accepted`, including capability, owner, exact runtime, boundary,
exchanged data, evidence, risks, tests, platforms, removal condition, and
review date. No compatibility exception is accepted by this result.

## Retained PGlite host candidate

Run:

```sh
gtimeout 600 scripts/phase2/hub-embedded-retained.test.sh
gtimeout 900 scripts/phase2/hub-embedded-retained-evidence.sh
```

The Rust adapter launches pinned PGlite `0.5.4` through length-prefixed JSON
IPC. It serializes requests, owns the database directory exclusively, kills the
child after timeout or lost reply, and never retries a write. Typed values keep
null, binary, timestamp, numeric, JSON, boolean, column order, and structured
PostgreSQL errors.

Nine targeted tests pass. They cover all 49 migrations, no-op restart, a real
one-migration historical database reopening into the remaining 48 migrations,
future and partial journal outcomes, transaction rollback, concurrent callers,
exclusive ownership, child crash, parent death, bounded frames, timeout, and
lost-reply ambiguity.

Original and retained-host captures match 50 installed schema tables, 537
constraint and index names, and all 49 migration journal rows. Candidate raw
catalog evidence also records 52 public and Drizzle tables before probe
filtering, 458 non-primary constraints, and 94 non-primary indexes. The retained
package inventory records 301 files. The retained migration inventory records
98 files.

Measured runtime is Node `v26.7.0` on `darwin/x64`, executable
`/usr/local/Cellar/node/26.7.0/bin/node`, SHA-256
`9bc2ac1e3fbfe4e9c3e2194283861d3cc7f07daa7a22a59be6ae79229922f540`.
The dependency graph and every retained package and migration file digest are
preserved beside the raw captures.

This is a candidate, not an accepted compatibility exception. Node packaging
and platform availability, IPC performance, and retained JavaScript delivery
and support ownership remain unqualified. The exception record remains
`required-not-accepted`.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `hub-embedded-retained-original.json` | 51,030 | `92a873c2b3fedec61aa0fcf499d4cbe5d1964b90d47eb81e3a82a770ae97f508` |
| `hub-embedded-retained-candidate.json` | 120,323 | `78192d280645147613180cc991c98311e8a0f57190ae4be6ca28e16f40fbea44` |
| `hub-embedded-retained-comparison.json` | 3,032 | `1566a46e335aefa76460384941b5cef4391c7313321ac755fbfa8a68bae8258c` |
| `hub-embedded-retained-dependency-graph.json` | 526 | `f55c0095950994ea6ecb8358b879d5e44297c1dd9a2b1cf5b982255fdcc623b2` |
| `hub-embedded-retained-package-sha256.txt` | 28,332 | `ba5a5bbbd4e82f994a3503066ce69a6a970f416108041ad427e314aeeb337850` |
| `hub-embedded-retained-migrations-sha256.txt` | 9,131 | `a345dc1c2f48e67b921eb84cbe25f24bdb5bfdd68b39c71d170334ba9337bf38` |
| `hub-embedded-retained-evidence.log` | 4,388 | `acd251d05301cabd99ef224d9c1073d5170e9d199f50eea21bffb45480b364ae` |
| `hub-embedded-retained-tests.log` | 1,076 | `24e983418d8b0926d8158b2f12e9adcb1fc5d15c51ab720c8777b14cfc829915` |
