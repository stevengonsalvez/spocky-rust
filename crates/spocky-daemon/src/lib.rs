//! Daemon process: WebSocket listener on `/ws`, hello and version
//! negotiation, Host and Origin admission, readiness, home layout
//! (`config.json`, `paseo.pid`, `server-id`), and process lifecycle. Behavior
//! follows pinned Paseo `5de45e2`.
//!
//! This crate is a library plus a transport probe binary. The production
//! `spocky-daemon` binary is built by `spocky-daemon-app`, which supplies a
//! [`session_api::SessionBackend`] and calls [`process::run`].

pub mod admission;
pub mod bearer;
pub mod config_file;
pub mod daemon;
pub mod daemon_keypair;
pub mod hostnames;
pub mod http;
pub mod iso_time;
pub mod js;
pub mod listen;
pub mod local_credential;
pub mod log;
pub mod origin;
pub mod pid_lock;
pub mod private_files;
pub mod process;
pub mod server;
pub mod server_id;
pub mod server_info;
pub mod session_api;
pub mod subprotocol;
pub mod upgrade;
