//! Codex provider adapter: launches the external `codex` program and speaks
//! its app-server protocol, following pinned Paseo `5de45e2`.
//!
//! Ported: launch and version gates, `initialize`, model catalog, thread
//! start, reuse, and resume (`thread/loaded/list`, `thread/resume`, history
//! replay from `thread/read`), `turn/start`,
//! `turn/interrupt`, close, root-thread streaming of user, assistant,
//! reasoning, plan, todo, compaction, usage, and shell tool events, and
//! command and file change approvals.
//!
//! Not yet ported, and reported through `CodexSession::unported` when hit:
//! file change, MCP, web search, and sub-agent tool items, question and MCP
//! elicitation requests (answered with Paseo's dismiss replies meanwhile),
//! slash commands, non-text prompt blocks, plan mode, and sub-agent history.

pub mod catalog;
pub mod history;
pub mod items;
pub mod launch;
pub mod notification;
pub mod options;
pub mod session;
pub mod tools;
pub mod transport;

pub use launch::{CodexGates, CustomProvider, ProviderCommand, ProviderRuntimeSettings};
pub use session::{
    CodexProvider, CodexSession, NativeArchiveState, Prompt, ResumeHandle, RunOptions,
    SessionConfig, SteerOptions, SteerResult, is_definitive_steer_rejection,
};
