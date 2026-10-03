//! The daemon as a process: logging to stdout, signal handling, and the forced
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

use crate::config_file::configured_log_level;
use crate::daemon::{
    DAEMON_VERSION, DaemonEnv, FORCE_EXIT_AFTER, StartupError, resolve_paseo_home, start,
};
use crate::log::{JsonLineLogger, Logger};
use crate::session_api::SessionBackend;

/// The worker's root logger: `createRootLogger` writes JSON lines and
/// `.child({ daemonVersion })` binds the version into every record.
#[must_use]
pub fn daemon_logger<W: io::Write + Send>(sink: W) -> JsonLineLogger<W> {
    JsonLineLogger::new(
        sink,
        vec![("daemonVersion".to_owned(), DAEMON_VERSION.to_owned())],
    )
}

/// What the process writes to stderr for a failed start. A start that
/// `daemon.start()` rejected with an Error reaches Node's top level, which
/// prints the error's stack; any other failure prints its message.
#[must_use]
pub fn failure_text(error: &StartupError) -> String {
    error
        .1
        .as_ref()
        .map_or_else(|| error.0.clone(), |err| err.stack.clone())
}

/// Starts the daemon from the process environment and runs it until SIGTERM,
/// SIGINT, or loss of the PID lock, then stops it.
///
/// Exit code 0 after a graceful stop. Exit code 1 when startup fails (the
/// message goes to stderr) or when stopping outlives [`FORCE_EXIT_AFTER`], as
/// the baseline's force-exit timer does.
#[must_use]
pub fn run(backend: Arc<dyn SessionBackend>) -> ExitCode {
    // `createRootLogger({ log: config.log }, { paseoHome, file: false })`: the
    // level comes from the config file, which the start reads again.
    let env = DaemonEnv::from_process();
    let level = configured_log_level(&resolve_paseo_home(&env), false);
    let logger: Arc<dyn Logger> = Arc::new(daemon_logger(io::stdout()).with_level(level));
    // Registered before startup, so a signal that arrives while starting is
    // held for the wait loop instead of killing the process mid-startup.
    let signalled = Arc::new(AtomicBool::new(false));
    for signal in [SIGTERM, SIGINT] {
        if signal_hook::flag::register(signal, Arc::clone(&signalled)).is_err() {
            eprintln!("Failed to install a signal handler");
            return ExitCode::from(1);
        }
    }
    let daemon = match start(&env, backend, &logger) {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!("{}", failure_text(&error));
            return ExitCode::from(1);
        }
    };

    while !signalled.load(Ordering::SeqCst) && !daemon.shutdown_requested() {
        thread::sleep(Duration::from_millis(50));
    }

    // The timer starts when shutdown begins and covers the whole stop.
    let force_logger = Arc::clone(&logger);
    let timer = thread::Builder::new()
        .name("force-exit".to_owned())
        .spawn(move || {
            thread::sleep(FORCE_EXIT_AFTER);
            force_logger.warn(&[], "Forcing shutdown - HTTP server didn't close in time");
            std::process::exit(1);
        });
    if let Err(error) = timer {
        eprintln!("Failed to start the force-exit timer: {error}");
    }
    daemon.stop();
    ExitCode::SUCCESS
}
