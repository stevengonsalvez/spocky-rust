//! Slice wire contracts: JSON message envelopes, session request and event
//! schemas, and the provider boundary types shared by the daemon, session,
//! and provider crates. Field names, optional versus null versus missing
//! semantics, and ordering follow pinned Paseo `5de45e2`.
//!
//! Inbound types (client to daemon) mirror zod output: shape key order,
//! unknown keys stripped. Outbound types (daemon to client) mirror the
//! object construction order in the pinned daemon, because the client's
//! zod-aot validator passes the daemon's object through unchanged.

pub mod agent;
pub mod agent_config;
pub mod attachment;
pub mod config;
#[rustfmt::skip]
pub mod config_schema;
pub mod creation;
pub mod field;
pub mod frame;
pub mod id;
pub mod js;
pub mod js_value;
pub mod json;
pub mod literal;
pub mod number;
pub mod permission;
pub mod request;
pub mod response;
pub mod session;
pub mod snapshot;
pub mod text;
pub mod timeline;
pub mod workspace;
pub mod ws;
pub mod zod;
#[rustfmt::skip]
pub mod zod_schemas;
