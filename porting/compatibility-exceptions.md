# Compatibility Runtime Exceptions

One compatibility runtime exception is accepted as time-boxed interim: the Hub
retained JavaScript PGlite host. No exception is accepted as permanent.

An exception requires capability, owner, exact runtime and version, boundary, exchanged data, pilot evidence, security and packaging risk, performance and support risk, original-versus-candidate tests, platforms, removal condition, and review date.

## Harness-only tools

`scripts/phase2/relay-ops-tls-proxy.py` is a local differential-test TLS edge
proxy. It is excluded from shipped runtime ownership and is not a compatibility
runtime exception. Replace it with Rust only if test-harness ownership becomes
part of the release artifact.

`scripts/phase2/pglite-rust-host-import-trace.mjs` runs the pinned PGlite
JavaScript glue under Node only to trace which Wasm imports a workload calls.
It is evidence tooling for lane `p2_pglite_rust_host`, excluded from shipped
runtime ownership, and not a compatibility runtime exception.

## Accepted interim

### Hub retained JavaScript PGlite host

- Status: `accepted-interim`, decided by Stevie on 2026-10-01 and relayed by
  the coordinator session. Time-boxed: it stands only until the removal
  condition below is met. It does not close the open gaps listed here.
- Replacement lane: `p2_pglite_rust_host` is open to build a Rust Wasm host in
  new crate `crates/spocky-pglite-host` (worktree
  `/Users/stevengonsalvez/orca/workspaces/paseo-rust/phase2-pglite-rust-host`)
- Capability and owner: `CLOUD-HUB-DATA-015` embedded Hub PostgreSQL-compatible
  storage, Hub storage
- Runtime: `@electric-sql/pglite` `0.5.4` with its distributed JavaScript glue,
  from pinned Hub `28f6c78833065fd282f9064f92a9aa61875dd359`; Node.js
  `v26.7.0` on `darwin/x64` (`/usr/local/Cellar/node/26.7.0/bin/node`, SHA-256
  `9bc2ac1e3fbfe4e9c3e2194283861d3cc7f07daa7a22a59be6ae79229922f540`) is the
  only measured host
- Owned surface: Rust adapter `crates/spocky-hub-pilot/src/retained_pglite.rs`
  and JavaScript host `scripts/phase2/hub-embedded-retained-host.mjs`, last
  changed at `65f5b5cba675f3e689574ce06cf5d8dd36edcbba`
- Boundary and data: Rust Hub process to a Node child over length-prefixed JSON
  IPC; SQL, bind parameters, typed result rows, transaction and migration
  commands, structured PostgreSQL errors, and shutdown state. The database
  directory is held by one shared OS file lock that the Node child inherits.
- Why not Rust yet: the pinned main Wasm module imports 121 custom `env`
  functions, 13 WASI functions, and one `GOT.mem` value; initdb imports 40 more
  `env` functions. Bare WASI does not supply that contract. SQLite matches 50
  tables but reports 209 constraint and index names against 537, so it is rejected as
  the exact engine. Evidence: `evidence/phase2/hub-embedded-engine-selection.json`
  (SHA-256 `b54387b38d49e645d9f4687eaae64a2d449443893e8cf8fa3ca3fff44a269f21`)
  and `evidence/phase2/hub-embedded-storage.md`.
- Original-versus-candidate evidence: 50 catalog tables, 537 constraint and
  index names, and 49 journal rows match
  (`hub-embedded-retained-comparison.json`, SHA-256
  `8d0606ebb86c3f557f61eab523be53988ef45a2ff34c3ac0c47322222217f88f`). 17
  retained-host runtime tests pass (`scripts/phase2/hub-embedded-retained.test.sh`);
  the lead reran it on main `0070832` on 2026-10-01 with Node `v26.7.0`: 17
  passed, 0 failed.
  Same-schema baseline handoff and reverse reopen pass
  (`evidence/phase2/hub-legacy-handoff.md`). Ordered mixed starts exclude the
  second owner (`evidence/phase2/hub-mixed-ownership.md`). Additive
  future-journal handoff and failed-batch rollback pass
  (`evidence/phase2/hub-schema-downgrade.md`).
- Review: Astra review J (`evidence/phase2/review-j.md`) accepts the narrow
  darwin/x64 cooperating-host ownership checkpoint with no P0, P1, or P2
  finding, and keeps this exception `required-not-accepted`.
- Security and packaging risk: ships Node.js, a 301-file 25,403,132-byte npm
  package, subprocess IPC, filesystem permissions, and widens code-signing,
  updater, and supply-chain review scope
- Performance and support risk: process startup, IPC serialization, memory
  duplication, crash supervision, and support for the custom Wasm host contract;
  IPC performance is unmeasured against baseline budgets
- Open gaps: Node packaging and availability on packaged macOS, Windows, Linux,
  iOS, Android, and browser; IPC performance; callback transactions and keyed
  application locks; full schema-definition provenance; relational mutation
  traces; destructive or semantic downgrade; the two simultaneous mixed-owner
  races below
- Platforms: darwin/x64 only
- Removal condition: a Rust-hosted PGlite-compatible engine replays all 49
  historical migrations, reopens old state, survives process crashes, and
  matches embedded and PostgreSQL mutation traces on required platforms
- Review date: 2026-10-01

## Candidates

No candidate below is accepted for release or parity.

### Hub paused incomplete legacy owner

- Status: candidate only
- Capability and owner: mixed Hub data-directory ownership, Hub storage
- Runtime: pinned baseline `28f6c78833065fd282f9064f92a9aa61875dd359` and Rust `spocky-hub-pilot` at `5d29c4a5b1fe68498f43f7e6b3d61d74e76b533f`
- Boundary and data: `.paseo-hub.lock` containing legacy `pid` and `token` JSON or candidate `os-file-lock-v1` JSON
- Evidence: `evidence/phase2/hub-simultaneous-ownership-report.json`, `baselinePausedAfterExclusiveCreate`
- Risk: candidate may replace an incomplete live legacy owner and permit two live owners; corruption and recovery impact remain unqualified
- Performance and support: ten 10 ms legacy reads are bounded, but no safe mixed-start guarantee exists
- Differential coverage: handwritten pinned lock-operation model against the real candidate; pinned database runtime is not executed for this race
- Platforms: macOS x64 evidence only; Windows unqualified
- Removal condition: atomic cross-runtime claim protocol or removal of supported concurrent mixed-version starts
- Review date: 2026-10-01

### Hub stale-unlink live-owner replacement

- Status: candidate only
- Capability and owner: mixed Hub data-directory ownership, Hub storage
- Runtime: pinned baseline `28f6c78833065fd282f9064f92a9aa61875dd359` and Rust `spocky-hub-pilot` at `5d29c4a5b1fe68498f43f7e6b3d61d74e76b533f`
- Boundary and data: `.paseo-hub.lock` inode identity and owner JSON
- Evidence: `evidence/phase2/hub-simultaneous-ownership-report.json`, `completedLiveRecordStaleUnlinkToctou`
- Risk: path unlink may delete a newly completed live legacy record and permit two live owners; corruption and recovery impact remain unqualified
- Performance and support: identity recheck narrows the race but cannot make unlink conditional and atomic
- Differential coverage: deterministic handwritten operation model; pinned database runtime is not executed for this race
- Platforms: macOS x64 evidence only; Windows unqualified
- Removal condition: conditional atomic deletion primitive shared by both runtimes or removal of supported concurrent mixed-version starts
- Review date: 2026-10-01
