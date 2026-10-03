//! Every WebSocket close the pinned relay emits, with its exact code and reason.
//!
//! Sources: `PaseoRelay.Socket`, `PaseoRelay.Ownership`, `PaseoRelay.Delivery.Writer`
//! and Cowboy's own frame-size close.

/// A WebSocket close code with its reason text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloseFrame {
    pub code: u16,
    pub reason: &'static str,
}

const fn close(code: u16, reason: &'static str) -> CloseFrame {
    CloseFrame { code, reason }
}

pub const SESSION_EXPIRED: CloseFrame = close(1012, "Session expired");
pub const SESSION_OWNER_MOVED: CloseFrame = close(1012, "Session owner moved");
pub const SERVER_DISCONNECTED: CloseFrame = close(1012, "Server disconnected");
pub const RELAY_INGRESS_CAPACITY: CloseFrame = close(1013, "Relay ingress capacity");
pub const DATA_ROUTE_UNAVAILABLE: CloseFrame = close(1013, "Data route unavailable");
pub const DELIVERY_UNAVAILABLE: CloseFrame = close(1013, "Delivery unavailable");
pub const RELAY_MEMORY_PRESSURE: CloseFrame = close(1013, "Relay memory pressure");
pub const RELAY_CAPACITY_UNAVAILABLE: CloseFrame = close(1013, "Relay capacity unavailable");
pub const SLOW_CONSUMER: CloseFrame = close(1013, "Slow consumer");
pub const INVALID_HANDSHAKE_KEY: CloseFrame = close(1008, "Invalid handshake key");
pub const REPLACED_BY_NEW_CONNECTION: CloseFrame = close(1008, "Replaced by new connection");
pub const CONTROL_UNRESPONSIVE: CloseFrame = close(1011, "Control unresponsive");
pub const CLIENT_DISCONNECTED: CloseFrame = close(1001, "Client disconnected");
/// Cowboy closes an oversized frame or reassembled message with an empty reason.
pub const MESSAGE_TOO_LARGE: CloseFrame = close(1009, "");

/// The complete set, in declaration order.
pub const ALL: [CloseFrame; 14] = [
    SESSION_EXPIRED,
    SESSION_OWNER_MOVED,
    SERVER_DISCONNECTED,
    RELAY_INGRESS_CAPACITY,
    DATA_ROUTE_UNAVAILABLE,
    DELIVERY_UNAVAILABLE,
    RELAY_MEMORY_PRESSURE,
    RELAY_CAPACITY_UNAVAILABLE,
    SLOW_CONSUMER,
    INVALID_HANDSHAKE_KEY,
    REPLACED_BY_NEW_CONNECTION,
    CONTROL_UNRESPONSIVE,
    CLIENT_DISCONNECTED,
    MESSAGE_TOO_LARGE,
];
