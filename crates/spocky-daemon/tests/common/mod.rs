//! Shared by the process and lifecycle tests: nothing started by a test may
//! listen on the production daemon ports.

/// Ports of the production daemon.
const PRODUCTION_PORTS: [u16; 2] = [6767, 6768];

/// Panics when `listen` (`host:port`, a bare port, or a path) names a
/// production port. Call it before any launch.
#[allow(dead_code)]
pub fn assert_disposable_listen(listen: &str) {
    let port = listen.rsplit(':').next().unwrap_or(listen);
    if let Ok(port) = port.trim().parse::<u16>() {
        assert!(
            !PRODUCTION_PORTS.contains(&port),
            "refusing to start a test daemon on production port {port} ({listen})"
        );
    }
}
