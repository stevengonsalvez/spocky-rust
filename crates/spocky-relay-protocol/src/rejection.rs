//! HTTP answers of `PaseoRelay.Socket.init/2` for requests that never become a WebSocket.
//!
//! Status and body only. The response headers (`connection: close` on the `426`, Cowboy's
//! `server: Cowboy` and `content-length`, the reroute header on the `409`) are added by the
//! network layer, which the `spocky-relay` network slice owns.

use crate::connection::{Connection, from_query};
use crate::query::{into_query_map, parse_qs};

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

/// A reroute answer: the owner target travels in the configured header, the body is empty.
pub const REROUTE: Rejection = Rejection {
    status: 409,
    body: "",
};

/// `{:unavailable, :owner}`: the Owner closed while the claim attached.
pub const OWNER_UNAVAILABLE: Rejection = Rejection {
    status: 503,
    body: "owner",
};

/// `{:unavailable, :draining}`: the node is draining.
pub const DRAINING: Rejection = Rejection {
    status: 503,
    body: "draining",
};

/// `{:unavailable, :cluster}`: the cluster is below its minimum size.
pub const CLUSTER_UNAVAILABLE: Rejection = Rejection {
    status: 503,
    body: "cluster",
};

/// The order `Socket.init/2` applies before it routes: a request that is not a WebSocket
/// upgrade gets `426`, a query Cowlib rejects gets an empty `400`, and a route that
/// `Connection.from_query/1` rejects gets `400` with its message.
///
/// # Errors
///
/// Returns the rejection the relay answers with.
pub fn classify(
    is_upgrade: bool,
    query: &[u8],
    generated_id: impl FnOnce() -> [u8; 8],
) -> Result<Connection, Rejection> {
    if !is_upgrade {
        return Err(EXPECTED_WEBSOCKET_UPGRADE);
    }
    let pairs = parse_qs(query).map_err(|_| MALFORMED_QUERY)?;
    from_query(&into_query_map(pairs), generated_id).map_err(invalid_connection)
}

/// An invalid connection query is answered with `400` and the validation message.
#[must_use]
pub const fn invalid_connection(message: &'static str) -> Rejection {
    Rejection {
        status: 400,
        body: message,
    }
}
