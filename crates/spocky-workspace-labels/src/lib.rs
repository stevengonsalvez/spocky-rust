//! Workspace labels: a host-wide catalog of coloured labels, their assignment
//! to workspaces, and a journal that lets clients catch up after a gap.
//!
//! Mirrors pinned Paseo `server/workspace-labels`.

pub mod catalog_store;
pub mod clock;
pub mod error;
pub mod names;
pub mod sequence;
pub mod service;
