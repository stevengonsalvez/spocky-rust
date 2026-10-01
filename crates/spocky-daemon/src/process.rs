//! The daemon as a process: logging to stderr, signal handling, and the forced
//! exit when a graceful stop takes too long.
//!
//! Sources at Paseo `5de45e2`: `daemon-worker.ts` (`beginShutdown`, SIGTERM and
//! SIGINT, the 10 s `forceExit` timer) and `scripts/supervisor-entrypoint.ts`
//! (`failStartup`). The production binary lives in `spocky-daemon-app`, which
//! supplies the session backend; this is the part every binary shares.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGINT, SIGTERM};

use crate::daemon::{DaemonEnv, FORCE_EXIT_AFTER, start};
use crate::log::{JsonLineLogger, Logger};
use crate::session_api::SessionBackend;

/// Starts the daemon from the process environment and runs it until SIGTERM,
/// SIGINT, or loss of the PID lock, then stops it.
///
/// Exit code 0 after a graceful stop. Exit code 1 when startup fails (the
/// message goes to stderr) or when stopping outlives [`FORCE_EXIT_AFTER`], as
/// the baseline's force-exit timer does.
#[must_use]
pub fn run(backend: Arc<dyn SessionBackend>) -> ExitCode {
    let logger: Arc<dyn Logger> = Arc::new(JsonLineLogger::new(io::stderr(), Vec::new()));
    let daemon = match start(&DaemonEnv::from_process(), backend, &logger) {
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

    // The timer starts when shutdown begins and covers the whole stop.
    thread::spawn(|| {
        thread::sleep(FORCE_EXIT_AFTER);
        eprintln!("Forcing shutdown - HTTP server didn't close in time");
        std::process::exit(1);
    });
    daemon.stop();
    ExitCode::SUCCESS
}
