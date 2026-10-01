//! Session layer: request dispatch, subscriptions, workspace registry, and
//! agent lifecycle orchestration over providers and persisted state. Behavior
//! follows pinned Paseo `5de45e2`.

pub mod agent_identity;
pub mod agent_labels;
pub mod agent_projection;
pub mod agent_prompt;
pub mod agent_sdk;
pub mod agent_storage;
pub mod checkout;
pub mod clock;
pub mod external_state;
pub mod git;
pub mod git_remote;
pub mod js;
pub mod paths;
pub mod project_key;
pub mod provisioning;
pub mod runtime_mcp_config;
pub mod stream_coalescer;
pub mod text;
pub mod timeline;
pub mod timeline_content;
