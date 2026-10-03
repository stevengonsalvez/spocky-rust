//! `PaseoRelay.Connection.from_query/1`: route validation of an upgrade request.

use crate::limits::MAXIMUM_ROUTE_ID_BYTES;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Server,
    Client,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Version {
    V1,
    V2,
}

/// A validated route. Identifiers are raw query bytes, not necessarily UTF-8.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Connection {
    pub server_id: Vec<u8>,
    pub role: Role,
    pub version: Version,
    /// `None` on v1. On v2 an empty value marks the daemon control socket.
    pub connection_id: Option<Vec<u8>>,
}

pub const INVALID_ROLE: &str = "Missing or invalid role parameter";
pub const SERVER_ID_TOO_LONG: &str = "serverId is too long";
pub const MISSING_SERVER_ID: &str = "Missing serverId parameter";
pub const INVALID_VERSION: &str = "Invalid v parameter (expected 1 or 2)";
pub const CONNECTION_ID_TOO_LONG: &str = "connectionId is too long";

/// Validates a decoded query map.
///
/// A v2 client without a `connectionId` receives `conn_` followed by the lowercase
/// hexadecimal of `generated_id`, which the caller draws from a strong random source.
///
/// # Errors
///
/// Returns the exact `400` message of the first invalid parameter, in the order
/// role, `serverId`, `v`, `connectionId`.
pub fn from_query(
    query: &BTreeMap<Vec<u8>, Vec<u8>>,
    generated_id: impl FnOnce() -> [u8; 8],
) -> Result<Connection, &'static str> {
    let role = match query.get(b"role".as_slice()).map(Vec::as_slice) {
        Some(b"server") => Role::Server,
        Some(b"client") => Role::Client,
        _ => return Err(INVALID_ROLE),
    };
    let server_id = match query.get(b"serverId".as_slice()) {
        Some(value) if (1..=MAXIMUM_ROUTE_ID_BYTES).contains(&value.len()) => value.clone(),
        Some(value) if value.len() > MAXIMUM_ROUTE_ID_BYTES => return Err(SERVER_ID_TOO_LONG),
        _ => return Err(MISSING_SERVER_ID),
    };
    let version = match query.get(b"v".as_slice()).map(|value| elixir_trim(value)) {
        None | Some(b"" | b"1") => Version::V1,
        Some(b"2") => Version::V2,
        Some(_) => return Err(INVALID_VERSION),
    };
    let connection_id = match version {
        Version::V1 => None,
        Version::V2 => {
            let value = query
                .get(b"connectionId".as_slice())
                .map_or(&[][..], |value| elixir_trim(value));
            if value.len() > MAXIMUM_ROUTE_ID_BYTES {
                return Err(CONNECTION_ID_TOO_LONG);
            }
            if role == Role::Client && value.is_empty() {
                Some(format_generated_id(generated_id()))
            } else {
                Some(value.to_vec())
            }
        }
    };
    Ok(Connection {
        server_id,
        role,
        version,
        connection_id,
    })
}

fn format_generated_id(bytes: [u8; 8]) -> Vec<u8> {
    let mut id = b"conn_".to_vec();
    for byte in bytes {
        id.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    id
}

/// `String.trim/1` on a possibly non-UTF-8 binary: strips Unicode `White_Space`
/// characters from both ends and stops at the first byte that is not one.
#[must_use]
pub fn elixir_trim(value: &[u8]) -> &[u8] {
    let mut start = 0;
    while let Some(width) = leading_white_space(&value[start..]) {
        start += width;
    }
    let mut end = value.len();
    while end > start {
        let Some(width) = trailing_white_space(&value[start..end]) else {
            break;
        };
        end -= width;
    }
    &value[start..end]
}

fn leading_white_space(bytes: &[u8]) -> Option<usize> {
    (1..=4.min(bytes.len())).find_map(|width| white_space_width(&bytes[..width]))
}

fn trailing_white_space(bytes: &[u8]) -> Option<usize> {
    (1..=4.min(bytes.len())).find_map(|width| white_space_width(&bytes[bytes.len() - width..]))
}

fn white_space_width(candidate: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(candidate).ok()?;
    let mut characters = text.chars();
    let character = characters.next()?;
    (characters.next().is_none() && character.is_whitespace()).then_some(candidate.len())
}
