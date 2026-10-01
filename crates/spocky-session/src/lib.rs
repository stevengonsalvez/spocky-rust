//! Session layer: request dispatch, subscriptions, workspace registry, and
//! agent lifecycle orchestration over providers and persisted state. Behavior
//! follows pinned Paseo `5de45e2`.

pub mod checkout;
pub mod clock;
pub mod git;
pub mod git_remote;
pub mod paths;
pub mod project_key;
pub mod provisioning;
pub mod timeline;
