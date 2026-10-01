//! `spocky-daemon`: the daemon process. It takes no arguments; the home comes
//! from `PASEO_HOME`, the listen address from `PASEO_LISTEN`, `config.json`
//! (`daemon.listen`) or `PORT`, in the pinned order.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGINT, SIGTERM};
use spocky_daemon::daemon::{DaemonEnv, FORCE_EXIT_AFTER, NoSessionBackend, start};
use spocky_daemon::log::{JsonLineLogger, Logger};

fn main() -> ExitCode {
    let logger: Arc<dyn Logger> = Arc::new(JsonLineLogger::new(io::stderr(), Vec::new()));
    let daemon = match start(
        &DaemonEnv::from_process(),
        Arc::new(NoSessionBackend),
        &logger,
    ) {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    let signalled = Arc::new(AtomicBool::new(false));
    for signal in [SIGTERM, SIGINT] {
        if signal_hook::flag::register(signal, Arc::clone(&signalled)).is_err() {
            eprintln!("Failed to install a signal handler");
            daemon.stop();
            return ExitCode::from(1);
        }
    }
    while !signalled.load(Ordering::SeqCst) && !daemon.shutdown_requested() {
        thread::sleep(Duration::from_millis(50));
    }

    // "Forcing shutdown - HTTP server didn't close in time"
    thread::spawn(|| {
        thread::sleep(FORCE_EXIT_AFTER);
        eprintln!("Forcing shutdown - HTTP server didn't close in time");
        std::process::exit(1);
    });
    daemon.stop();
    ExitCode::SUCCESS
}
