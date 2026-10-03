//! The daemon's relay client: control and data sockets, reconnect, keepalive and the
//! encrypted socket handed to the application.
//!
//! Sans-IO. Sockets, timers and logging are reached through caller-supplied handles, so
//! the same code runs under a real network runtime and under the differential harness
//! that replays recorded events against the pinned TypeScript.

pub mod endpoint;
pub mod js_json;
