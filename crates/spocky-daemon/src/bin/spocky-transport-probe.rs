//! `spocky-transport-probe`: the daemon transport with no session layer. It
//! accepts a hello, answers `server_info`, ping and HTTP, and nothing else. It
//! exists so the transport can be exercised as a process (signals, exit codes,
//! home files) without the session crates. The production daemon is
//! `spocky-daemon-app`.

use std::process::ExitCode;
use std::sync::Arc;

use spocky_daemon::daemon::NoSessionBackend;
use spocky_daemon::process::run;

fn main() -> ExitCode {
    run(Arc::new(NoSessionBackend))
}
