//! Wire contract of the distributed relay: limits, close codes, upgrade rejections,
//! route validation, handshake-key validation and v2 control messages.
//!
//! Pure functions over bytes. No sockets, no clock, no randomness. Behavior is the
//! pinned relay (`paseo-relay@3fc41c96c8c63f3a7109e832899cc57d473c4531`), including
//! Cowboy 2.17, Cowlib and Jason 1.4 behavior the relay inherits.

pub mod limits;
