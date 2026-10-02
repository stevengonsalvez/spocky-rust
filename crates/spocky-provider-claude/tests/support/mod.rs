//! Shared support for the differential tests: the pinned-node runner that
//! lives in `spocky-contracts`' tests, included by path so there is one copy.

// Each test binary uses a different subset of this module.
#![allow(dead_code)]

#[path = "../../../spocky-contracts/tests/support/pinned_node.rs"]
mod pinned_node;

pub use pinned_node::*;
