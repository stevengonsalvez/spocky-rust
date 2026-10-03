//! Wire limits of the pinned relay (`PaseoRelay.Protocol` and `PaseoRelay.Connection`).

/// Masked data-frame ceiling on the wire: 32 MiB.
pub const MAXIMUM_FRAME_WIRE_BYTES: usize = 32 * 1024 * 1024;

/// Largest header of a masked client frame.
pub const MAXIMUM_CLIENT_FRAME_HEADER_BYTES: usize = 14;

/// Payload that still fits the wire ceiling after a masked client header.
pub const MAXIMUM_CLIENT_FRAME_PAYLOAD_BYTES: usize =
    MAXIMUM_FRAME_WIRE_BYTES - MAXIMUM_CLIENT_FRAME_HEADER_BYTES;

/// Cowboy applies one payload limit to frames and reassembled messages.
pub const MAXIMUM_MESSAGE_PAYLOAD_BYTES: usize = MAXIMUM_CLIENT_FRAME_PAYLOAD_BYTES;

/// Separate ceiling for inbound v2 control messages: 64 KiB.
pub const MAXIMUM_CONTROL_PAYLOAD_BYTES: usize = 64 * 1024;

/// Longest accepted `serverId` or v2 `connectionId`, in bytes.
pub const MAXIMUM_ROUTE_ID_BYTES: usize = 256;

/// Cowlib's default `max_keys` for `cow_qs:parse_qs/1`.
pub const MAXIMUM_QUERY_KEYS: usize = 100;
