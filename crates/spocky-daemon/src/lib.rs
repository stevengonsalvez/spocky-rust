//! Daemon process: WebSocket listener on `/ws`, hello and version
//! negotiation, Host and Origin admission, readiness, home layout
//! (`config.json`, `paseo.pid`, `server-id`), and process lifecycle. Behavior
//! follows pinned Paseo `5de45e2`.

pub mod bearer;
pub mod hostnames;
pub mod iso_time;
pub mod js;
pub mod listen;
pub mod local_credential;
pub mod log;
pub mod origin;
pub mod pid_lock;
pub mod private_files;
pub mod server_id;
pub mod subprotocol;
pub mod upgrade;
