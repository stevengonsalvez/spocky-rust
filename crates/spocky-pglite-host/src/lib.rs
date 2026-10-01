//! Rust host for the pinned `@electric-sql/pglite` 0.5.4 Wasm modules.
//!
//! The host runs the distributed `pglite.wasm`, `initdb.wasm` and the side
//! modules in `pglite.data` unchanged under Wasmtime, replacing the
//! Emscripten JavaScript glue that the retained Node host uses.

mod dylink;
mod jsdate;
mod netdb;
pub mod package;
pub mod pglite;
pub mod protocol;
mod runtime;
mod shell;
mod syscalls;
mod vfs;
