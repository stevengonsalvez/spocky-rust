//! Codex provider adapter: launches the external `codex` program, speaks its
//! app-server protocol, and maps turns, streaming, approvals, interrupts, and
//! resume onto the slice contracts. Behavior follows pinned Paseo `5de45e2`.

pub mod catalog;
pub mod items;
pub mod launch;
pub mod notification;
pub mod session;
pub mod tools;
pub mod transport;

pub use launch::{CodexGates, CustomProvider, ProviderCommand, ProviderRuntimeSettings};
pub use session::{CodexProvider, CodexSession, Prompt, RunOptions, SessionConfig};
