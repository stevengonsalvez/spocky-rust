//! Daemon application: implements the `spocky-daemon` session backend by
//! dispatching protocol requests to the session layer and providers, and owns
//! the production `spocky-daemon` binary. Behavior follows pinned Paseo
//! `5de45e2`.

pub mod authorization;
pub mod request;
