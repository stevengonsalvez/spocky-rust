//! Bearer credentials carried in HTTP headers and WebSocket subprotocols.
//!
//! Sources at Paseo `5de45e2`: `extractHttpBearerToken`,
//! `extractWsBearerProtocol` and `extractWsBearerToken` in `auth.ts`, and
//! `selectWebSocketProtocol` in `websocket-server.ts`.

use crate::js;

/// `extractHttpBearerToken`: exactly `Bearer <token>` after trimming, with the
/// token and scheme separated by whitespace.
#[must_use]
pub fn extract_http_bearer_token(value: Option<&str>) -> Option<&str> {
    let value = value.filter(|value| !value.is_empty())?;
    let mut parts = js::trim(value)
        .split(js::is_whitespace)
        .filter(|part| !part.is_empty());
    let scheme = parts.next()?;
    let token = parts.next()?;
    (scheme == "Bearer" && parts.next().is_none()).then_some(token)
}

/// The token of a `paseo.bearer.<token>` protocol: everything after the
/// prefix. The pinned code splits on dots and rejoins segments from the third,
/// which is the same text; an empty token is still a token.
fn bearer_token(protocol: &str) -> Option<&str> {
    protocol.strip_prefix("paseo.bearer.")
}

/// `extractWsBearerProtocol`: the first listed protocol shaped like a bearer
/// token.
#[must_use]
pub fn extract_ws_bearer_protocol(value: Option<&str>) -> Option<&str> {
    value
        .filter(|value| !value.is_empty())?
        .split(',')
        .map(js::trim)
        .find(|protocol| bearer_token(protocol).is_some())
}

/// `extractWsBearerToken`.
#[must_use]
pub fn extract_ws_bearer_token(protocol: Option<&str>) -> Option<&str> {
    bearer_token(protocol.filter(|protocol| !protocol.is_empty())?)
}

/// `selectWebSocketProtocol`: with no password the first offered protocol is
/// echoed. With a password only a bearer protocol is accepted. `None` is the
/// `false` that makes the handshake answer without a protocol.
///
/// Caller contract: `password_set` is true exactly when the daemon holds a
/// password hash (`auth.password` in `createWebSocketServer`). It is not the
/// per-connection credential.
#[must_use]
pub fn select_web_socket_protocol<'a>(
    protocols: &[&'a str],
    password_set: bool,
) -> Option<&'a str> {
    if !password_set {
        return protocols.first().copied();
    }
    protocols
        .iter()
        .copied()
        .find(|protocol| extract_ws_bearer_token(Some(protocol)).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_bearer_needs_exactly_scheme_and_token() {
        assert_eq!(extract_http_bearer_token(Some("Bearer abc")), Some("abc"));
        assert_eq!(
            extract_http_bearer_token(Some("  Bearer \t abc  ")),
            Some("abc")
        );
        assert_eq!(extract_http_bearer_token(Some("bearer abc")), None);
        assert_eq!(extract_http_bearer_token(Some("Bearer")), None);
        assert_eq!(extract_http_bearer_token(Some("Bearer a b")), None);
        assert_eq!(extract_http_bearer_token(Some("Basic abc")), None);
        assert_eq!(extract_http_bearer_token(Some("")), None);
        assert_eq!(extract_http_bearer_token(Some("   ")), None);
        assert_eq!(extract_http_bearer_token(None), None);
    }

    #[test]
    fn ws_bearer_protocol_is_the_first_matching_entry() {
        assert_eq!(
            extract_ws_bearer_protocol(Some("chat, paseo.bearer.tok , paseo.bearer.other")),
            Some("paseo.bearer.tok")
        );
        assert_eq!(extract_ws_bearer_protocol(Some("paseo.bearer")), None);
        assert_eq!(extract_ws_bearer_protocol(Some("paseo.basic.tok")), None);
        assert_eq!(extract_ws_bearer_protocol(Some("")), None);
        assert_eq!(extract_ws_bearer_protocol(None), None);
    }

    #[test]
    fn ws_bearer_token_rejoins_the_remaining_segments() {
        assert_eq!(
            extract_ws_bearer_token(Some("paseo.bearer.a.b")),
            Some("a.b")
        );
        assert_eq!(extract_ws_bearer_token(Some("paseo.bearer.")), Some(""));
        assert_eq!(extract_ws_bearer_token(Some("paseo.bearer")), None);
        assert_eq!(extract_ws_bearer_token(Some("x.bearer.t")), None);
        assert_eq!(extract_ws_bearer_token(None), None);
    }

    #[test]
    fn protocol_selection_depends_on_the_password() {
        assert_eq!(select_web_socket_protocol(&["a", "b"], false), Some("a"));
        assert_eq!(select_web_socket_protocol(&[], false), None);
        assert_eq!(
            select_web_socket_protocol(&["a", "paseo.bearer.t"], true),
            Some("paseo.bearer.t")
        );
        assert_eq!(select_web_socket_protocol(&["a"], true), None);
    }
}
