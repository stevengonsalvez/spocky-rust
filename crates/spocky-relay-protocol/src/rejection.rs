//! HTTP answers of `PaseoRelay.Socket.init/2` for requests that never become a WebSocket.

/// A relay HTTP rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rejection {
    pub status: u16,
    pub body: &'static str,
}

pub const EXPECTED_WEBSOCKET_UPGRADE: Rejection = Rejection {
    status: 426,
    body: "Expected WebSocket upgrade",
};
pub const CONNECTION_CAPACITY: Rejection = Rejection {
    status: 503,
    body: "Relay connection capacity",
};
pub const MEMORY_PRESSURE: Rejection = Rejection {
    status: 503,
    body: "Relay memory pressure",
};
pub const CAPACITY_CONFIGURATION: Rejection = Rejection {
    status: 503,
    body: "Relay capacity configuration",
};
pub const CAPACITY_UNAVAILABLE: Rejection = Rejection {
    status: 503,
    body: "Relay capacity unavailable",
};

/// Cowboy answers a malformed or oversized query string with an empty `400`.
pub const MALFORMED_QUERY: Rejection = Rejection {
    status: 400,
    body: "",
};

/// Status of a reroute answer; the owner target travels in the configured header.
pub const REROUTE_STATUS: u16 = 409;

/// An invalid connection query is answered with `400` and the validation message.
#[must_use]
pub const fn invalid_connection(message: &'static str) -> Rejection {
    Rejection {
        status: 400,
        body: message,
    }
}
