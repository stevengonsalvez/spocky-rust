//! The `/ws` HTTP upgrade decision.
//!
//! Sources at Paseo `5de45e2`: `createWebSocketServer` and `verifyWsUpgrade` in
//! `websocket-server.ts`, and `handleUpgrade`, `completeUpgrade` and
//! `abortHandshake` of the `ws` library (8.20.0) that runs the handshake.
//!
//! This module only decides. It reads a parsed request and returns either the
//! bytes of the `101` response or the bytes of the rejection, in the order and
//! with the status lines, bodies and headers the library produces.

use std::collections::HashSet;
use std::fmt::Write;

use tungstenite::handshake::derive_accept_key;

use crate::bearer::select_web_socket_protocol;
use crate::hostnames::{Hostnames, is_ws_upgrade_host_allowed};
use crate::js;
use crate::origin::is_origin_allowed;
use crate::subprotocol::parse_subprotocols;

/// `connectionLifecycle` in `websocket-server.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionLifecycle {
    Starting,
    Accepting,
    Stopping,
}

/// What the upgrade decision reads from the daemon.
#[derive(Debug, Clone, Copy)]
pub struct UpgradePolicy<'a> {
    pub lifecycle: ConnectionLifecycle,
    pub hostnames: Option<&'a Hostnames>,
    pub allowed_origins: &'a HashSet<String>,
    /// The daemon holds a password hash; see [`select_web_socket_protocol`].
    pub password_set: bool,
}

/// A parsed request head. Header names are as received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeRequest {
    pub method: String,
    pub url: String,
    /// The minor number of `HTTP/1.x` from the request line.
    pub http_minor: u8,
    pub headers: Vec<(String, String)>,
}

/// Headers that keep the first value when a request repeats them, from
/// Node's `IncomingMessage` header handling.
const FIRST_WINS: [&str; 18] = [
    "age",
    "authorization",
    "content-length",
    "content-type",
    "etag",
    "expires",
    "from",
    "host",
    "if-modified-since",
    "if-unmodified-since",
    "last-modified",
    "location",
    "max-forwards",
    "proxy-authorization",
    "referer",
    "retry-after",
    "server",
    "user-agent",
];

impl UpgradeRequest {
    /// `req.headers[name]`: names lowercased, repeated lines of the same header
    /// joined with `, ` (`; ` for `cookie`), except for the headers where the
    /// first line wins.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        let mut found: Option<String> = None;
        for (candidate, value) in &self.headers {
            if !candidate.eq_ignore_ascii_case(name) {
                continue;
            }
            match &mut found {
                None => found = Some(value.clone()),
                Some(_) if FIRST_WINS.contains(&name) => {}
                Some(existing) => {
                    existing.push_str(if name == "cookie" { "; " } else { ", " });
                    existing.push_str(value);
                }
            }
        }
        found
    }
}

/// A rejected upgrade: `abortHandshake(socket, code, message, headers)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abort {
    pub code: u16,
    pub message: String,
    pub headers: Vec<(String, String)>,
}

/// The result of the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeDecision {
    /// Send these bytes, then speak WebSocket.
    Accept(Vec<u8>),
    /// Send these bytes, then close the connection.
    Reject { response: Vec<u8>, abort: Abort },
}

/// `http.STATUS_CODES` for the codes this path can produce.
fn status_text(code: u16) -> &'static str {
    match code {
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// `abortHandshake`: status line, `Connection: close`, `Content-Type:
/// text/html`, `Content-Length` of the body, any extra headers, the message as
/// body. An absent message is the status text.
#[must_use]
pub fn abort_response(abort: &Abort) -> Vec<u8> {
    let mut text = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Type: text/html\r\nContent-Length: {}",
        abort.code,
        status_text(abort.code),
        abort.message.len()
    );
    for (name, value) in &abort.headers {
        let _ = write!(text, "\r\n{name}: {value}");
    }
    text.push_str("\r\n\r\n");
    text.push_str(&abort.message);
    text.into_bytes()
}

/// `completeUpgrade`: the `101` head, with `Sec-WebSocket-Protocol` last when a
/// protocol was selected.
#[must_use]
pub fn accept_response(key: &str, protocol: Option<&str>) -> Vec<u8> {
    let mut text = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}",
        derive_accept_key(key.as_bytes())
    );
    if let Some(protocol) = protocol {
        let _ = write!(text, "\r\nSec-WebSocket-Protocol: {protocol}");
    }
    text.push_str("\r\n\r\n");
    text.into_bytes()
}

fn reject(code: u16, message: &str, headers: Vec<(String, String)>) -> UpgradeDecision {
    let abort = Abort {
        code,
        message: message.to_owned(),
        headers,
    };
    UpgradeDecision::Reject {
        response: abort_response(&abort),
        abort,
    }
}

/// `keyRegex`: `/^[+/0-9A-Za-z]{22}==$/`.
fn is_valid_key(key: &str) -> bool {
    key.len() == 24
        && key.ends_with("==")
        && key[..22]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// `+value` for a header value: `undefined` is `NaN`, blank is 0, a
/// decimal, hex, octal or binary literal is its number.
fn to_number(value: Option<&str>) -> f64 {
    let Some(value) = value else {
        return f64::NAN;
    };
    let text = js::trim(value);
    if text.is_empty() {
        return 0.0;
    }
    let radix = |digits: &str, base: u32| {
        u64::from_str_radix(digits, base)
            .ok()
            .filter(|_| !digits.starts_with(['+', '-']))
    };
    #[allow(clippy::cast_precision_loss)]
    let parsed = match text.get(..2) {
        Some("0x" | "0X") => radix(&text[2..], 16).map(|n| n as f64),
        Some("0o" | "0O") => radix(&text[2..], 8).map(|n| n as f64),
        Some("0b" | "0B") => radix(&text[2..], 2).map(|n| n as f64),
        _ => text.parse::<f64>().ok().filter(|_| {
            !text
                .bytes()
                .any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E')
                || text == "Infinity"
                || text == "+Infinity"
                || text == "-Infinity"
        }),
    };
    parsed.unwrap_or(f64::NAN)
}

/// `handleUpgrade` then `verifyWsUpgrade` then `completeUpgrade`, in the
/// library's order: method, `Upgrade`, key, version, path, subprotocols, the
/// daemon's lifecycle, Host, Origin, then the accepted response.
#[must_use]
pub fn evaluate_upgrade(request: &UpgradeRequest, policy: &UpgradePolicy<'_>) -> UpgradeDecision {
    let key = request.header("sec-websocket-key");
    let version = to_number(request.header("sec-websocket-version").as_deref());

    if request.method != "GET" {
        return reject(405, "Invalid HTTP method", vec![]);
    }
    if request
        .header("upgrade")
        .is_none_or(|upgrade| upgrade.to_lowercase() != "websocket")
    {
        return reject(400, "Invalid Upgrade header", vec![]);
    }
    let Some(key) = key.filter(|key| is_valid_key(key)) else {
        return reject(400, "Missing or invalid Sec-WebSocket-Key header", vec![]);
    };
    #[allow(clippy::float_cmp)]
    if version != 13.0 && version != 8.0 {
        return reject(
            400,
            "Missing or invalid Sec-WebSocket-Version header",
            vec![("Sec-WebSocket-Version".to_owned(), "13, 8".to_owned())],
        );
    }
    let pathname = request.url.split('?').next().unwrap_or_default();
    if pathname != "/ws" {
        return reject(400, "Bad Request", vec![]);
    }
    let protocols = match request.header("sec-websocket-protocol") {
        Some(header) => match parse_subprotocols(&header) {
            Ok(protocols) => protocols,
            Err(_) => return reject(400, "Invalid Sec-WebSocket-Protocol header", vec![]),
        },
        None => Vec::new(),
    };

    // verifyWsUpgrade
    if policy.lifecycle != ConnectionLifecycle::Accepting {
        return reject(503, "Server not ready", vec![]);
    }
    let host = request.header("host");
    if !is_ws_upgrade_host_allowed(host.as_deref(), policy.hostnames) {
        return reject(403, "Host not allowed", vec![]);
    }
    let origin = request.header("origin");
    if !is_origin_allowed(origin.as_deref(), policy.allowed_origins, host.as_deref()) {
        return reject(403, "Origin not allowed", vec![]);
    }

    let offered: Vec<&str> = protocols.iter().map(String::as_str).collect();
    let selected = select_web_socket_protocol(&offered, policy.password_set);
    UpgradeDecision::Accept(accept_response(&key, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    fn request(extra: &[(&str, &str)]) -> UpgradeRequest {
        let mut headers: Vec<(String, String)> = [
            ("Host", "127.0.0.1:7000"),
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Key", KEY),
            ("Sec-WebSocket-Version", "13"),
        ]
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect();
        headers.extend(
            extra
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned())),
        );
        UpgradeRequest {
            method: "GET".to_owned(),
            url: "/ws".to_owned(),
            http_minor: 1,
            headers,
        }
    }

    fn without(mut req: UpgradeRequest, name: &str) -> UpgradeRequest {
        req.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        req
    }

    fn decide(req: &UpgradeRequest) -> UpgradeDecision {
        let origins = HashSet::new();
        evaluate_upgrade(
            req,
            &UpgradePolicy {
                lifecycle: ConnectionLifecycle::Accepting,
                hostnames: None,
                allowed_origins: &origins,
                password_set: false,
            },
        )
    }

    fn rejected(decision: UpgradeDecision) -> (u16, String, Vec<(String, String)>) {
        match decision {
            UpgradeDecision::Reject { abort, .. } => (abort.code, abort.message, abort.headers),
            UpgradeDecision::Accept(_) => panic!("expected a rejection"),
        }
    }

    #[test]
    fn accepts_a_valid_upgrade_with_the_rfc_accept_value() {
        let UpgradeDecision::Accept(bytes) = decide(&request(&[])) else {
            panic!("expected accept");
        };
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n"
        );
    }

    #[test]
    fn echoes_the_first_offered_protocol_when_no_password_is_set() {
        let UpgradeDecision::Accept(bytes) =
            decide(&request(&[("Sec-WebSocket-Protocol", "chat, other")]))
        else {
            panic!("expected accept");
        };
        assert!(String::from_utf8(bytes).unwrap().ends_with(
            "Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Protocol: chat\r\n\r\n"
        ));
    }

    #[test]
    fn a_password_accepts_only_a_bearer_protocol() {
        let origins = HashSet::new();
        let policy = UpgradePolicy {
            lifecycle: ConnectionLifecycle::Accepting,
            hostnames: None,
            allowed_origins: &origins,
            password_set: true,
        };
        let UpgradeDecision::Accept(with) = evaluate_upgrade(
            &request(&[("Sec-WebSocket-Protocol", "chat, paseo.bearer.t")]),
            &policy,
        ) else {
            panic!("expected accept");
        };
        assert!(
            String::from_utf8(with)
                .unwrap()
                .contains("Sec-WebSocket-Protocol: paseo.bearer.t\r\n")
        );
        let UpgradeDecision::Accept(without_bearer) =
            evaluate_upgrade(&request(&[("Sec-WebSocket-Protocol", "chat")]), &policy)
        else {
            panic!("expected accept");
        };
        assert!(
            !String::from_utf8(without_bearer)
                .unwrap()
                .contains("Sec-WebSocket-Protocol")
        );
    }

    #[test]
    fn rejection_bytes_follow_abort_handshake() {
        let mut req = request(&[]);
        req.method = "POST".to_owned();
        let UpgradeDecision::Reject { response, .. } = decide(&req) else {
            panic!("expected rejection");
        };
        assert_eq!(
            String::from_utf8(response).unwrap(),
            "HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\nContent-Type: text/html\r\nContent-Length: 19\r\n\r\nInvalid HTTP method"
        );
    }

    #[test]
    fn checks_run_in_the_library_order() {
        let mut req = request(&[]);
        req.method = "PUT".to_owned();
        req = without(without(req, "Upgrade"), "Sec-WebSocket-Key");
        assert_eq!(rejected(decide(&req)).0, 405);

        let req = without(without(request(&[]), "Upgrade"), "Sec-WebSocket-Key");
        assert_eq!(rejected(decide(&req)).1, "Invalid Upgrade header");

        let req = without(request(&[]), "Sec-WebSocket-Key");
        assert_eq!(
            rejected(decide(&req)).1,
            "Missing or invalid Sec-WebSocket-Key header"
        );

        let req = without(request(&[]), "Sec-WebSocket-Version");
        let (code, message, headers) = rejected(decide(&req));
        assert_eq!(
            (code, message.as_str()),
            (400, "Missing or invalid Sec-WebSocket-Version header")
        );
        assert_eq!(
            headers,
            [("Sec-WebSocket-Version".to_owned(), "13, 8".to_owned())]
        );
    }

    #[test]
    fn upgrade_header_is_case_insensitive_and_key_is_strict() {
        let mut req = request(&[]);
        req.headers.retain(|(n, _)| n != "Upgrade");
        req.headers
            .push(("upgrade".to_owned(), "WebSocket".to_owned()));
        assert!(matches!(decide(&req), UpgradeDecision::Accept(_)));
        for bad in [
            "",
            "short==",
            "dGhlIHNhbXBsZSBub25jZQ=A",
            "dGhlIHNhbXBsZSBub25j!Q==",
        ] {
            let mut req = without(request(&[]), "Sec-WebSocket-Key");
            req.headers
                .push(("Sec-WebSocket-Key".to_owned(), bad.to_owned()));
            assert_eq!(
                rejected(decide(&req)).1,
                "Missing or invalid Sec-WebSocket-Key header",
                "{bad}"
            );
        }
    }

    #[test]
    fn version_is_coerced_like_unary_plus() {
        // Expected values computed with `[13, 8].includes(+value)` in Node.
        for (value, ok) in [
            ("13", true),
            ("8", true),
            (" 13 ", true),
            ("0x0d", true),
            ("0X0D", true),
            ("13.0", true),
            ("1.3e1", true),
            ("130e-1", true),
            (".13e2", true),
            ("+13", true),
            ("-13", false),
            ("13.", true),
            ("12", false),
            ("", false),
            ("  ", false),
            ("abc", false),
            ("0b1101", true),
            ("0o15", true),
            ("0B1101", true),
            ("NaN", false),
            ("Infinity", false),
            ("inf", false),
            ("1_3", false),
            ("13px", false),
            ("0x", false),
            ("0x 1", false),
            ("0xd ", true),
            ("\t13\n", true),
            ("013", true),
            ("0o", false),
            ("0b2", false),
            ("1e", false),
            ("8.0", true),
            ("0.8e1", true),
            ("+0x0d", false),
            ("-0x0d", false),
            ("0x+d", false),
            ("\u{ff11}\u{ff13}", false),
            ("13\u{00a0}", true),
            ("\u{feff}13", true),
        ] {
            let mut req = without(request(&[]), "Sec-WebSocket-Version");
            req.headers
                .push(("Sec-WebSocket-Version".to_owned(), value.to_owned()));
            assert_eq!(
                matches!(decide(&req), UpgradeDecision::Accept(_)),
                ok,
                "{value:?}"
            );
        }
    }

    #[test]
    fn only_the_ws_path_is_handled_and_a_query_is_ignored() {
        let mut req = request(&[]);
        req.url = "/ws?x=1".to_owned();
        assert!(matches!(decide(&req), UpgradeDecision::Accept(_)));
        for url in ["/", "/ws/", "/WS", "/wss", "/ws2?x"] {
            req.url = url.to_owned();
            let (code, message, _) = rejected(decide(&req));
            assert_eq!((code, message.as_str()), (400, "Bad Request"), "{url}");
        }
    }

    #[test]
    fn bad_subprotocol_headers_are_400() {
        for bad in ["a;b", "a, a", "a,", ""] {
            let req = request(&[("Sec-WebSocket-Protocol", bad)]);
            let (code, message, _) = rejected(decide(&req));
            assert_eq!(
                (code, message.as_str()),
                (400, "Invalid Sec-WebSocket-Protocol header"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn repeated_protocol_header_lines_are_joined_before_parsing() {
        let req = request(&[
            ("Sec-WebSocket-Protocol", "a"),
            ("Sec-WebSocket-Protocol", "b"),
        ]);
        assert!(matches!(decide(&req), UpgradeDecision::Accept(_)));
        let dup = request(&[
            ("Sec-WebSocket-Protocol", "a"),
            ("sec-websocket-protocol", "a"),
        ]);
        assert_eq!(rejected(decide(&dup)).0, 400);
    }

    #[test]
    fn repeated_host_lines_keep_the_first() {
        let req = request(&[("Host", "evil.example")]);
        assert_eq!(req.header("host").as_deref(), Some("127.0.0.1:7000"));
        assert_eq!(
            request(&[("X-A", "1"), ("x-a", "2")])
                .header("x-a")
                .as_deref(),
            Some("1, 2")
        );
        assert_eq!(
            request(&[("Cookie", "a=1"), ("Cookie", "b=2")])
                .header("cookie")
                .as_deref(),
            Some("a=1; b=2")
        );
    }

    #[test]
    fn a_daemon_that_is_not_accepting_answers_503() {
        let origins = HashSet::new();
        for lifecycle in [ConnectionLifecycle::Starting, ConnectionLifecycle::Stopping] {
            let decision = evaluate_upgrade(
                &request(&[]),
                &UpgradePolicy {
                    lifecycle,
                    hostnames: None,
                    allowed_origins: &origins,
                    password_set: false,
                },
            );
            let (code, message, _) = rejected(decision);
            assert_eq!((code, message.as_str()), (503, "Server not ready"));
        }
    }

    #[test]
    fn a_not_ready_daemon_still_reports_malformed_requests_first() {
        let origins = HashSet::new();
        let policy = UpgradePolicy {
            lifecycle: ConnectionLifecycle::Starting,
            hostnames: None,
            allowed_origins: &origins,
            password_set: false,
        };
        let req = without(request(&[]), "Sec-WebSocket-Key");
        assert_eq!(rejected(evaluate_upgrade(&req, &policy)).0, 400);
    }

    #[test]
    fn host_is_checked_before_origin_and_a_missing_host_is_admitted() {
        let req = request(&[("Origin", "https://evil.example")]);
        let mut bad_host = without(req.clone(), "Host");
        bad_host
            .headers
            .push(("Host".to_owned(), "evil.example".to_owned()));
        let (code, message, _) = rejected(decide(&bad_host));
        assert_eq!((code, message.as_str()), (403, "Host not allowed"));

        let (code, message, _) = rejected(decide(&req));
        assert_eq!((code, message.as_str()), (403, "Origin not allowed"));

        let no_host = without(request(&[]), "Host");
        assert!(matches!(decide(&no_host), UpgradeDecision::Accept(_)));
    }

    #[test]
    fn configured_origins_hostnames_and_same_origin_are_honoured() {
        let origins: HashSet<String> = ["https://app.example".to_owned()].into();
        let hostnames = Hostnames::Patterns(vec![".example.com".to_owned()]);
        let policy = UpgradePolicy {
            lifecycle: ConnectionLifecycle::Accepting,
            hostnames: Some(&hostnames),
            allowed_origins: &origins,
            password_set: false,
        };
        let listed = without(request(&[("Origin", "https://app.example")]), "Host");
        assert!(matches!(
            evaluate_upgrade(&listed, &policy),
            UpgradeDecision::Accept(_)
        ));
        let mut named = without(request(&[]), "Host");
        named
            .headers
            .push(("Host".to_owned(), "box.example.com:7000".to_owned()));
        assert!(matches!(
            evaluate_upgrade(&named, &policy),
            UpgradeDecision::Accept(_)
        ));
        let same = request(&[("Origin", "http://localhost:7000")]);
        assert!(matches!(
            evaluate_upgrade(&same, &policy),
            UpgradeDecision::Accept(_)
        ));
        let wildcard_origins: HashSet<String> = ["*".to_owned()].into();
        let wildcard = UpgradePolicy {
            allowed_origins: &wildcard_origins,
            ..policy
        };
        let any = request(&[("Origin", "https://anything.example")]);
        assert!(matches!(
            evaluate_upgrade(&any, &wildcard),
            UpgradeDecision::Accept(_)
        ));
    }
}
