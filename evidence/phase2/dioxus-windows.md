# Dioxus Windows compile pilot

Dioxus `0.7.0` passed a locked `x86_64-pc-windows-msvc` compile check on
2026-09-30. This is candidate compile evidence, not Windows runtime evidence,
renderer selection, or parity evidence.

## Boundary

`scripts/phase2/dioxus-windows-check.sh` uses repository-local
`cargo-xwin 0.23.1`, cache, and Cargo target output. The script installs the
pinned `cargo-xwin` version when absent and ensures the Rust 1.94 toolchain has
the Windows standard library target. It does not change the default toolchain.
The compile command has a 1,200-second bound.

The captured environment used Rust `1.94.0`, Cargo `1.94.0`, and
`cargo-xwin 0.23.1`. The first complete locked cross-check finished in 2 minutes
30 seconds. The durable warm-cache run finished in 0.77 seconds.

Reproduce with:

```text
sh scripts/phase2/dioxus-windows-check.test.sh
scripts/phase2/dioxus-windows-check.sh
```

The raw durable log is 292 bytes with SHA-256
`ad8a637ee1c003fdf1dbce55c4010f1b0ff3f4eebbb37fccfdf452509e4ad9a3`.

This proves Windows x86_64 compilation only. Windows launch, visual,
accessibility, interaction, packaging, installation, update, rollback, and
pinned-original comparison remain open.
