//! `packages/protocol/src/daemon-endpoints.ts`: relay endpoint parsing and the relay
//! WebSocket URL, including the WHATWG `URL` and `URLSearchParams` behavior it relies on.

use std::{error::Error, fmt};
use url::Url;

/// An error with the exact JavaScript `message`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointError(pub String);

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for EndpointError {}

fn error(message: &str) -> EndpointError {
    EndpointError(message.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPortParts {
    pub host: String,
    pub port: u16,
    pub is_ipv6: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelayRole {
    Server,
    Client,
}

impl RelayRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
        }
    }
}

/// The `unknown` argument of `normalizeRelayProtocolVersion`.
#[derive(Clone, Debug, PartialEq)]
pub enum VersionInput {
    /// `null` or `undefined`.
    Missing,
    String(String),
    Number(f64),
    /// Any other type, which normalizes to the empty string.
    Other,
}

/// `normalizeRelayProtocolVersion` with the default fallback of `"2"`.
///
/// # Errors
///
/// Fails with `Relay version must be "1" or "2"` for any other non-empty value.
pub fn normalize_relay_protocol_version(
    value: &VersionInput,
) -> Result<&'static str, EndpointError> {
    let normalized = match value {
        VersionInput::Missing | VersionInput::Other => return Ok("2"),
        VersionInput::String(text) => js_trim(text).to_owned(),
        VersionInput::Number(number) => js_number_to_string(*number),
    };
    match normalized.as_str() {
        "" | "2" => Ok("2"),
        "1" => Ok("1"),
        _ => Err(error("Relay version must be \"1\" or \"2\"")),
    }
}

/// `String(number)` for the values that can equal `"1"` or `"2"`; any other result is
/// reported as a non-matching, non-empty string, which is all the caller needs.
fn js_number_to_string(number: f64) -> String {
    if number.to_bits() == 1.0_f64.to_bits() {
        "1".to_owned()
    } else if number.to_bits() == 2.0_f64.to_bits() {
        "2".to_owned()
    } else {
        "other".to_owned()
    }
}

fn is_js_space(character: char) -> bool {
    u16::try_from(u32::from(character)).is_ok_and(spocky_crypto::js_string::is_js_whitespace)
}

/// `String.prototype.trim`.
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

fn parse_port(digits: &str, context: &str) -> Result<u16, EndpointError> {
    match digits.parse::<u16>() {
        Ok(port) if port >= 1 => Ok(port),
        _ => Err(EndpointError(format!(
            "{context}: port must be between 1 and 65535"
        ))),
    }
}

/// `parseHostPort`.
///
/// # Errors
///
/// Fails with the original messages for an empty host, a bad shape or a bad port.
pub fn parse_host_port(input: &str) -> Result<HostPortParts, EndpointError> {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return Err(error("Host is required"));
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        let invalid = || error("Invalid host:port (expected [::1]:6767)");
        let close = rest.find(']').ok_or_else(invalid)?;
        let (host, tail) = rest.split_at(close);
        let digits = tail.strip_prefix("]:").ok_or_else(invalid)?;
        if host.is_empty() || !is_port_digits(digits) {
            return Err(invalid());
        }
        let host = js_trim(host);
        if host.is_empty() {
            return Err(error("Host is required"));
        }
        let port = parse_port(digits, "Invalid host:port")?;
        return Ok(HostPortParts {
            host: host.to_owned(),
            port,
            is_ipv6: true,
        });
    }
    let invalid = || error("Invalid host:port (expected localhost:6767)");
    let (host, digits) = trimmed.rsplit_once(':').ok_or_else(invalid)?;
    if host.is_empty() || host.chars().any(is_line_terminator) || !is_port_digits(digits) {
        return Err(invalid());
    }
    let host = js_trim(host);
    if host.is_empty() {
        return Err(error("Host is required"));
    }
    let port = parse_port(digits, "Invalid host:port")?;
    Ok(HostPortParts {
        host: host.to_owned(),
        port,
        is_ipv6: false,
    })
}

fn is_port_digits(text: &str) -> bool {
    (1..=5).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
}

const fn is_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Parameters of `buildRelayWebSocketUrl`.
#[derive(Clone, Debug)]
pub struct RelayUrlParams<'a> {
    pub endpoint: &'a str,
    pub use_tls: bool,
    pub server_id: &'a str,
    pub role: RelayRole,
    /// Ignored when empty, like the original's truthiness check.
    pub connection_id: Option<&'a str>,
    pub version: VersionInput,
}

/// `buildRelayWebSocketUrl`.
///
/// # Errors
///
/// Fails with the endpoint parse errors, the version error, or `Invalid URL`.
pub fn build_relay_websocket_url(params: &RelayUrlParams<'_>) -> Result<String, EndpointError> {
    let parts = parse_host_port(params.endpoint)?;
    let protocol = if params.use_tls { "wss" } else { "ws" };
    let host_part = if parts.is_ipv6 {
        format!("[{}]", parts.host)
    } else {
        parts.host.clone()
    };
    let mut url = Url::parse(&format!("{protocol}://{host_part}:{}/ws", parts.port))
        .map_err(|_| error("Invalid URL"))?;
    let version = normalize_relay_protocol_version(&params.version)?;
    // `url.searchParams.set` re-serializes the whole query, so a query that came from a
    // `?` inside the host is re-encoded as form data before the new pairs are added.
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    search_params_set(&mut pairs, "serverId", params.server_id);
    search_params_set(&mut pairs, "role", params.role.as_str());
    search_params_set(&mut pairs, "v", version);
    if let Some(connection_id) = params.connection_id.filter(|id| !id.is_empty()) {
        search_params_set(&mut pairs, "connectionId", connection_id);
    }
    url.query_pairs_mut().clear().extend_pairs(&pairs);
    Ok(url.to_string())
}

/// `URLSearchParams.prototype.set`: replaces the first pair with the name and removes the
/// rest, or appends.
fn search_params_set(pairs: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(index) = pairs.iter().position(|(candidate, _)| candidate == name) {
        value.clone_into(&mut pairs[index].1);
        let mut position = 0;
        pairs.retain(|(candidate, _)| {
            let keep = candidate != name || position == index;
            position += 1;
            keep
        });
    } else {
        pairs.push((name.to_owned(), value.to_owned()));
    }
}
