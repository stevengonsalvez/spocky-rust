//! Claude Code provider adapter: launches the external Claude Code program and
//! maps its sessions, streaming, permissions, interrupts, and resume onto the
//! provider contracts, following pinned Paseo `5de45e2`.

pub mod actor;
pub mod client;
pub mod context_usage;
pub mod date_parse;
pub mod launch;
pub mod local;
pub mod model_manifest;
pub mod models;
pub mod partial_json;
pub mod process;
pub mod project_dir;
pub mod prompt_attachments;
pub mod provider_image;
pub mod provider_options;
pub mod sdk_options;
pub mod sdk_query;
pub mod session;
pub mod sidechain_tracker;
pub mod subagents;
pub mod task_notification;
pub mod task_state;
pub mod timeline_assembler;
pub mod timestamps;
pub mod tool_call_detail;
pub mod tool_call_mapper;
pub mod transcript;
