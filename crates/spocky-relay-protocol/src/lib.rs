//! Wire contract of the distributed relay: limits, close codes, upgrade rejections,
//! route validation, handshake-key validation and v2 control messages.
//!
//! Pure functions over bytes. No sockets, no clock, no randomness. Behavior is the
//! pinned relay (`paseo-relay@3fc41c96c8c63f3a7109e832899cc57d473c4531`), including
//! Cowboy 2.17, Cowlib and Jason 1.4 behavior the relay inherits.
//!
//! Authority: the BEAM relay (`relay@3fc41c9`, capability `CLOUD-RELAY-PROTOCOL-027`) wins
//! wherever it differs from the Cloudflare adapter in `packages/relay`. The differences are
//! tabulated in `evidence/phase4/relay-protocol.md`. A Cloudflare fallback lane
//! (`CLOUD-RELAY-LEGACY-009`) can reuse these functions and inherits BEAM behavior.

pub mod close;
pub mod connection;
pub mod control;
pub mod erlang_map;
pub mod handshake;
pub mod json;
pub mod limits;
pub mod query;
pub mod rejection;
