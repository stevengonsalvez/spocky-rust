//! Claude Code provider adapter: launches the external Claude Code program and
//! maps its sessions, streaming, permissions, interrupts, and resume onto the
//! provider contracts, following pinned Paseo `5de45e2`.

pub mod model_manifest;
pub mod models;
pub mod partial_json;
pub mod tool_call_detail;
pub mod tool_call_mapper;
