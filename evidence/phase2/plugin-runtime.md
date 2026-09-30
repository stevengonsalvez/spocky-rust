# Plugin runtime pilot

Status: passing local macOS runtime evidence. This does not select a plugin
runtime, close `P2-PLUGIN-01`, or establish complete `DPLUGIN-001` parity.

## Provenance

- Paseo baseline: `5de45e208690b0efc51c59a585ae9729325a9204`
- Import baseline: `8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5`
- Rust candidate base: `52fbf03aa15068fcdf3b98ae1dca491396f66810`
- Host: macOS, `rustc 1.94.0`, Cargo `1.94.0`, Node `v26.7.0`, npm `11.19.0`, Git `2.52.0`
- Both pinned checkouts were clean before and after capture. The source alias in
  `plugin-vitest.config.mts` avoids generating build output in the baseline.

## Scenarios

The capture runs 11 pinned Paseo tests first. Five exercise local Git acquisition,
reviewed revisions, nested source identity, and updates. Six exercise settings
validation, atomic writes, revision conflicts, migration failure, reset, and
restart persistence.

The Rust pilot then runs three existing lifecycle cases and three real local
runtime cases:

1. Clone a nested Git plugin at an exact 40-character reviewed commit. Launch its
   Node process, exchange initialize/ready/shutdown/stopped messages, and retain
   the contributed RPC.
2. Pack an npm plugin, install the tarball offline with lifecycle scripts disabled,
   verify package name and version, then launch it through the same bounded process
   exchange.
3. Activate a Git revision, persist installation-scoped settings, review and clone
   a later broken revision, observe process startup failure, reopen the host from
   disk, and verify the prior revision, contribution, and settings remain active.

Every Git, npm, and subprocess command has a deadline. The npm case cannot contact
the network and cannot run package lifecycle scripts. Test fixtures and acquired
sources live in disposable temporary directories.

## Result

- Pinned Paseo: 11 passed, 0 failed
- Rust lifecycle: 3 passed, 0 failed
- Rust acquisition/runtime: 3 passed, 0 failed
- Successful process exchanges: 2, with 4 captured frames each
- Failed reviewed update: prior revision, `rpc:review.v1`, and `tone=terse` survived restart
- Raw log SHA-256: `99a30373336177701afb5ccf0dc3eceb6ce8ad5e8ffed6d95c85210d64d16b2b`
- Raw traffic SHA-256: `62d6602c57526ed75bc3507c043e5550541e93f667c3758327843d45fcbb9d2d`
- Raw artifacts: ignored `evidence/raw/phase2/plugin-runtime.log` and
  `evidence/raw/phase2/plugin-runtime-traffic.json`

## Remaining blockers

- The Rust process exchange is a reduced pilot protocol, not a byte-level replay
  of Paseo's full plugin subprocess protocol.
- Windows and Linux acquisition, path, process, failure, and restart runs are absent.
- Client contribution transport and behavior on iOS, Android, browser, and desktop
  remain untested.
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
