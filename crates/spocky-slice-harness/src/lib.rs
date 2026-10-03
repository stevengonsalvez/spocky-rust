//! Phase 3 unchanged-client differential harness: drives the pinned Paseo
//! CLI against original and Spocky daemons on disposable homes and ports,
//! never 6767, and enforces exact gate comparisons.

pub mod compare;
pub mod gates;
pub mod normalize;
pub mod persistence;
pub mod side;
pub mod stub;
