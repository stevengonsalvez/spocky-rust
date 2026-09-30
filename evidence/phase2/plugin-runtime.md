# Plugin runtime pilot

Status: passing expanded local macOS runtime evidence. This does not select a
plugin runtime, close `P2-PLUGIN-01`, or establish complete `DPLUGIN-001` parity.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Import baseline: `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5`
- Rust candidate base: `ca190e4111ff7df6976629c5923d06fe05509068`
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
and five real local acquisition/runtime cases:

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

The manifest case rejects unknown keys and invalid IDs, validates optional
description, Paseo requirement, and build argv, and infers the pinned
`index.client.ts[x]` and `index.server.ts[x]` entry names. The client protocol
case compares Rust catalog, RPC, and settings notification JSON against a
committed fixture emitted through the pinned TypeScript schemas.

Every Git, npm, and subprocess command has a deadline. The npm case cannot contact
the network and cannot run package lifecycle scripts. Test fixtures and acquired
sources live in disposable temporary directories.

## Result

- Pinned Paseo: 11 passed, 0 failed
- Rust lifecycle: 3 passed, 0 failed
- Rust manifest/client protocol: 2 passed, 0 failed
- Rust acquisition/runtime: 5 passed, 0 failed
- Successful process exchanges: Git 5 frames, npm 3 frames
- Failed reviewed update: prior revision, `rpc:review.v1`, and `tone=terse` survived restart
- Pinned protocol fixture SHA-256: `14f39a3541bc79a13e82354b5d91619f6bc4dcada91dfd095bbf0a191faef252`
- Raw log SHA-256: `06ff3fc87898fa2f71d36cd6df368cebf045a3cea16854eb734ca709ce24a0be`
- Raw traffic SHA-256: `8365f9a4ee81b94f8e02f34c0b2bd4891c3c63a00dcde8d9cb91de1163056bd7`
- Raw artifacts: ignored `evidence/raw/phase2/plugin-runtime.log` and
  `evidence/raw/phase2/plugin-runtime-traffic.json`

## Remaining blockers

- The Rust exchange covers initialize, ready, invoke, result, and shutdown shapes,
  but uses newline stdio instead of Node fork IPC and omits providers, usage,
  hooks, daemon-session frames, cancellation, fatal messages, and reconnect.
- Client source is transported but not compiled with Paseo's esbuild boundary or
  evaluated through the iOS, Android, browser, or desktop contribution runtime.
- Rust settings persist installation-scoped string maps. Schema defaults,
  validation, revisions, migration, reset, notifications, and corrupt-data
  recovery are proven only by pinned Paseo tests.
- Windows and Linux acquisition, path, process, failure, and restart runs are absent.
- Managed checkout deletion and recovery after host termination during staging are
  not exercised against real acquired sources.
- No production daemon, port `6767`, deploy, publish, paid service, or remote source
  was used.

## Reproduction

```sh
scripts/phase2/plugin-runtime-capture.sh
cargo fmt --package paseo-plugin-pilot -- --check
cargo clippy -p paseo-plugin-pilot --all-targets -- -D warnings
```
