//! Daemon application: implements the `spocky-daemon` session backend by
//! dispatching protocol requests to the session layer and providers, and owns
//! the production `spocky-daemon` binary. Behavior follows pinned Paseo
//! `5de45e2`.

pub mod agent_control;
pub mod agent_create;
pub mod agent_directory;
pub mod agent_message;
pub mod agent_updates;
pub mod authorization;
pub mod bootstrap;
pub mod codex_agent;
pub mod events;
pub mod inline_task;
pub mod provider;
pub mod reply_window;
pub mod request;
pub mod session;
pub mod shutdown;
pub mod workspace_handlers;
