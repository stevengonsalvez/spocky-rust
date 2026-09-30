# Plugin runtime pilot

Status: passing expanded local macOS runtime evidence. This does not select a
plugin runtime, close `P2-PLUGIN-01`, or establish complete `DPLUGIN-001` parity.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Import baseline: `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5`
- Rust runtime-capture base: `ca190e4111ff7df6976629c5923d06fe05509068`
- Rust settings checkpoint: `057d7a9`
- Host: macOS, `rustc 1.94.0`, Cargo `1.94.0`, Node `v26.7.0`, npm `11.19.0`, Git `2.52.0`
- Both pinned checkouts were clean before and after capture. The source alias in
  `plugin-vitest.config.mts` avoids generating build output in the baseline.

## Scenarios

The capture first emits initialize, ready, invoke, result, shutdown, catalog,
client RPC, and settings-change objects parsed by the pinned Paseo schemas. It
then runs 11 pinned Paseo tests. Five exercise local Git acquisition, reviewed
revisions, nested source identity, and updates. Six exercise settings validation,
atomic writes, revision conflicts, migration failure, reset, and persistence.

The Rust pilot runs three lifecycle cases, two manifest/client-protocol cases,
six real local acquisition/runtime cases, and five settings lifecycle cases:

1. Clone a nested Git plugin at an exact 40-character reviewed commit. Launch its
   Node process, exchange source-shaped initialize/ready/invoke/result/shutdown
   messages, and retain the contributed RPC.
2. Pack an npm plugin, install the tarball offline with lifecycle scripts disabled,
   verify package name and version, then launch it through the same bounded process
   exchange.
3. Activate a Git revision, persist installation-scoped settings, review and clone
   a later broken revision, observe process startup failure, reopen the host from
   disk, and verify the prior revision, contribution, and settings remain active.
4. Normalize a Windows-style nested plugin path on macOS, reject a missing nested
   path, and remove the failed Git staging checkout.
5. Reject a mismatched npm package identity and remove the failed npm staging tree.
6. Transport RPC, provider, usage, event-hook, and before-hook contributions from
   a real ready frame. Surface a fatal invocation as a typed error, terminate the
   exact child, and start a fresh process successfully from the same package.
7. Default and validate typed settings, hash exact raw revisions, reject stale
   writes, migrate once, reject newer schemas, preserve corrupt data until reset,
   isolate notification values, persist atomically with mode `0600`, and keep
   installation directories separate from definition IDs.

The manifest case rejects unknown keys and invalid IDs, validates optional
description, Paseo requirement, and build argv, and infers the pinned
`index.client.ts[x]` and `index.server.ts[x]` entry names. The client protocol
case compares Rust catalog, RPC, and settings notification JSON against a
committed fixture emitted through the pinned TypeScript schemas.

The process-envelope case covers every outer request and message variant in the
pinned `plugin-process-protocol.ts` union. It roundtrips 15 host request shapes
and 14 plugin message shapes, rejects unknown fields, and enforces the pinned
nonempty identifier and positive version boundaries. Provider input and event
payloads remain opaque JSON pending their separate schema port.

Every Git, npm, and subprocess command has a deadline. The npm case cannot contact
the network and cannot run package lifecycle scripts. Test fixtures and acquired
sources live in disposable temporary directories.

## Result

- Pinned Paseo: 11 passed, 0 failed
- Rust lifecycle: 3 passed, 0 failed
- Rust manifest/client protocol: 2 passed, 0 failed
- Rust acquisition/runtime: 6 passed, 0 failed
- Rust process envelope: 2 passed, 0 failed
- Rust settings lifecycle: 5 passed, 0 failed
- Successful process exchanges: Git 5 frames, npm 3 frames
- Failed reviewed update: prior revision, `rpc:review.v1`, and `tone=terse` survived restart
- Pinned protocol fixture SHA-256: `14f39a3541bc79a13e82354b5d91619f6bc4dcada91dfd095bbf0a191faef252`
- Raw log SHA-256: `06ff3fc87898fa2f71d36cd6df368cebf045a3cea16854eb734ca709ce24a0be`
- Raw traffic SHA-256: `8365f9a4ee81b94f8e02f34c0b2bd4891c3c63a00dcde8d9cb91de1163056bd7`
- Raw artifacts: ignored `evidence/raw/phase2/plugin-runtime.log` and
  `evidence/raw/phase2/plugin-runtime-traffic.json`

## Remaining blockers

- The Rust exchange executes initialize, ready, invoke, result, and shutdown over
  newline stdio instead of Node fork IPC. The full outer envelope is modeled but
  provider, usage, hook, daemon-session, cancellation, and reconnect sequences
  are not executed. Fatal invocation and fresh-process restart now pass.
- Provider input and event payload schemas remain opaque JSON values.
- Client source is transported but not compiled with Paseo's esbuild boundary or
  evaluated through the iOS, Android, browser, or desktop contribution runtime.
- The Rust settings pilot covers the pinned boolean and integer schema used by
  baseline tests. Arbitrary JSON schemas, async refinements, callback error
  reporting, and daemon RPC integration remain open.
- Windows and Linux acquisition, path, process, failure, and restart runs are absent.
- Managed checkout deletion and recovery after host termination during staging are
  not exercised against real acquired sources.
- No production daemon, port `6767`, deploy, publish, paid service, or remote source
  was used.

## Reproduction

```sh
scripts/phase2/plugin-runtime-capture.sh
cargo test -p paseo-plugin-pilot --test settings_lifecycle
cargo test -p paseo-plugin-pilot --test process_protocol
cargo fmt --package paseo-plugin-pilot -- --check
cargo clippy -p paseo-plugin-pilot --all-targets -- -D warnings
```
