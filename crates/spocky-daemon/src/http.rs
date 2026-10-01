//! Plain HTTP on the daemon port: request parsing and the routes that come
//! before the WebSocket upgrade handler's concerns.
//!
//! Sources at Paseo `5de45e2`: the Express app in `bootstrap.ts` (Host check on
//! TCP listeners, CORS, bearer middleware, `/api/health`, `/api/status`) and
//! `auth.ts` (`createRequireBearerMiddleware`, `shouldBypassBearerAuth`).
//!
//! Not ported: routes outside the vertical slice (`/api/files/download`,
//! `/api/terminal-activity`, `/mcp/agents`, the web UI and service proxy). They
//! answer like any unknown path, with Express' 404 page.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::BuildHasher;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};

use crate::admission::PasswordVerifier;
use crate::bearer::extract_http_bearer_token;
use crate::hostnames::{Hostnames, is_http_host_allowed};
use crate::iso_time::{now_ms, to_iso_string};
use crate::local_credential::matches_local_credential;
use crate::upgrade::UpgradeRequest;

/// Node's `maxHeaderSize` default.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;

/// What reading a request head from a buffer produced.
#[derive(Debug, PartialEq, Eq)]
pub enum ParsedHead {
    /// A full head and the number of bytes it used.
    Complete(UpgradeRequest, usize),
    /// More bytes are needed.
    Partial,
    /// Not HTTP, answered with 400.
    Invalid,
    /// Head larger than [`MAX_HEADER_BYTES`], answered with 431.
    TooLarge,
}

/// Node decodes header values as Latin-1: one byte, one code point. A UTF-8
/// `é` therefore reads as two characters, exactly as in the baseline.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// Parses an HTTP/1.x request head. Header values are decoded as Latin-1.
#[must_use]
pub fn parse_head(buffer: &[u8]) -> ParsedHead {
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(buffer) {
        Ok(httparse::Status::Complete(used)) => {
            if used > MAX_HEADER_BYTES {
                return ParsedHead::TooLarge;
            }
            let (Some(method), Some(url)) = (request.method, request.path) else {
                return ParsedHead::Invalid;
            };
            ParsedHead::Complete(
                UpgradeRequest {
                    method: method.to_owned(),
                    url: url.to_owned(),
                    headers: request
                        .headers
                        .iter()
                        .map(|header| {
                            (
                                header.name.to_owned(),
                                latin1(header.value).trim_matches([' ', '\t']).to_owned(),
                            )
                        })
                        .collect(),
                },
                used,
            )
        }
        Ok(httparse::Status::Partial) => {
            if buffer.len() > MAX_HEADER_BYTES {
                ParsedHead::TooLarge
            } else {
                ParsedHead::Partial
            }
        }
        Err(_) => ParsedHead::Invalid,
    }
}

/// Whether Node would raise `upgrade` for this head: an `Upgrade` header and a
/// `Connection` header listing `upgrade`.
#[must_use]
pub fn is_upgrade_request(request: &UpgradeRequest) -> bool {
    request.header("upgrade").is_some()
        && request.header("connection").is_some_and(|connection| {
            connection
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        })
}

/// An HTTP response ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        431 => "Request Header Fields Too Large",
        _ => "Unknown",
    }
}

impl HttpResponse {
    /// Status line, headers, a blank line, then the body.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }

    /// Node's answer to a head it cannot parse or accept.
    #[must_use]
    pub fn client_error(status: u16) -> Self {
        Self {
            status,
            headers: vec![("Connection".to_owned(), "close".to_owned())],
            body: Vec::new(),
        }
    }
}

/// `Date` header value (`toUTCString` in IMF-fixdate form).
#[must_use]
pub fn http_date(ms: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let iso = to_iso_string(ms);
    let day_index = usize::try_from(ms.div_euclid(86_400_000).rem_euclid(7)).unwrap_or(0);
    let month: usize = iso[5..7].parse().unwrap_or(1);
    format!(
        "{}, {} {} {} {} GMT",
        DAYS[day_index],
        &iso[8..10],
        MONTHS[month - 1],
        &iso[0..4],
        &iso[11..19]
    )
}

/// Express' weak `ETag`: `W/"<length in hex>-<27 base64 chars of sha1>"`.
#[must_use]
pub fn weak_etag(body: &[u8]) -> String {
    if body.is_empty() {
        return "W/\"0-2jmj7l5rSw0yVb/vlWAYkK/YBwk\"".to_owned();
    }
    let digest = STANDARD.encode(Sha1::digest(body));
    format!("W/\"{:x}-{}\"", body.len(), &digest[..27])
}

/// What the routes read from the running daemon.
pub struct HttpContext<'a, S: BuildHasher> {
    pub server_id: &'a str,
    pub hostname: &'a str,
    pub version: &'a str,
    /// `formatListenTarget(boundListenTarget)`.
    pub listen: &'a str,
    /// The listener is TCP; only then is the Host header checked.
    pub tcp_listener: bool,
    pub hostnames: Option<&'a Hostnames>,
    pub allowed_origins: &'a HashSet<String, S>,
    pub password_hash: Option<&'a str>,
    pub local_credential: Option<&'a str>,
    pub verifier: &'a dyn PasswordVerifier,
    pub now_ms: i64,
}

/// `res.json(body)` as Express writes it.
fn json_response(
    status: u16,
    body: &Value,
    ctx_now: i64,
    cors: &[(String, String)],
) -> HttpResponse {
    let bytes = body.to_string().into_bytes();
    let mut headers = vec![("X-Powered-By".to_owned(), "Express".to_owned())];
    headers.extend(cors.iter().cloned());
    headers.push((
        "Content-Type".to_owned(),
        "application/json; charset=utf-8".to_owned(),
    ));
    headers.push(("Content-Length".to_owned(), bytes.len().to_string()));
    headers.push(("ETag".to_owned(), weak_etag(&bytes)));
    headers.push(("Date".to_owned(), http_date(ctx_now)));
    headers.push(("Connection".to_owned(), "close".to_owned()));
    HttpResponse {
        status,
        headers,
        body: bytes,
    }
}

fn not_found(request: &UpgradeRequest, now: i64, cors: &[(String, String)]) -> HttpResponse {
    let path = encode_path(request_path(request));
    let escaped = path
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let body = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot {} {escaped}</pre>\n</body>\n</html>\n",
        request.method
    )
    .into_bytes();
    let mut headers = vec![
        ("X-Powered-By".to_owned(), "Express".to_owned()),
        (
            "Content-Security-Policy".to_owned(),
            "default-src 'none'".to_owned(),
        ),
        ("X-Content-Type-Options".to_owned(), "nosniff".to_owned()),
    ];
    headers.extend(cors.iter().cloned());
    headers.push((
        "Content-Type".to_owned(),
        "text/html; charset=utf-8".to_owned(),
    ));
    headers.push(("Content-Length".to_owned(), body.len().to_string()));
    headers.push(("Date".to_owned(), http_date(now)));
    headers.push(("Connection".to_owned(), "close".to_owned()));
    HttpResponse {
        status: 404,
        headers,
        body,
    }
}

/// The pathname Node's URL parser reports: spaces, quotes, angle brackets,
/// backslash, caret, backtick, braces, pipe, apostrophe and non-ASCII bytes are
/// percent-encoded.
fn encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte <= 0x20 || byte >= 0x7f || b" \"<>\\^`{|}'".contains(&byte) {
            let _ = write!(encoded, "%{byte:02X}");
        } else {
            encoded.push(char::from(byte));
        }
    }
    encoded
}

/// `req.path`: the request target without its query string.
fn request_path(request: &UpgradeRequest) -> &str {
    request.url.split('?').next().unwrap_or_default()
}

/// `shouldBypassBearerAuth`.
fn bypasses_bearer_auth(method: &str, path: &str) -> bool {
    method == "OPTIONS"
        || path == "/api/health"
        || path == "/api/files/download"
        || path == "/mcp/agents"
}

/// The middleware chain and routes, in `bootstrap.ts` order.
#[must_use]
pub fn handle_request<S: BuildHasher>(
    request: &UpgradeRequest,
    ctx: &HttpContext<'_, S>,
) -> HttpResponse {
    let host = request.header("host");
    if ctx.tcp_listener && !is_http_host_allowed(host.as_deref(), ctx.hostnames) {
        return json_response(
            403,
            &Value::Object(Map::from_iter([(
                "error".to_owned(),
                Value::from("Invalid Host header"),
            )])),
            ctx.now_ms,
            &[],
        );
    }

    let mut cors: Vec<(String, String)> = Vec::new();
    if let Some(origin) = request.header("origin").filter(|origin| !origin.is_empty())
        && (ctx.allowed_origins.contains("*") || ctx.allowed_origins.contains(origin.as_str()))
    {
        cors.extend([
            ("Access-Control-Allow-Origin".to_owned(), origin),
            (
                "Access-Control-Allow-Methods".to_owned(),
                "GET, POST, DELETE, OPTIONS".to_owned(),
            ),
            (
                "Access-Control-Allow-Headers".to_owned(),
                "Content-Type, Authorization".to_owned(),
            ),
            (
                "Access-Control-Allow-Credentials".to_owned(),
                "true".to_owned(),
            ),
        ]);
    }
    if request.method == "OPTIONS" {
        let mut headers = vec![("X-Powered-By".to_owned(), "Express".to_owned())];
        headers.extend(cors);
        headers.push(("Date".to_owned(), http_date(ctx.now_ms)));
        headers.push(("Connection".to_owned(), "close".to_owned()));
        return HttpResponse {
            status: 204,
            headers,
            body: Vec::new(),
        };
    }

    let path = request_path(request);
    if let Some(password_hash) = ctx.password_hash.filter(|hash| !hash.is_empty())
        && !bypasses_bearer_auth(&request.method, path)
    {
        let authorization = request.header("authorization");
        let token = extract_http_bearer_token(authorization.as_deref());
        let is_local = path == "/api/status"
            && ctx
                .local_credential
                .zip(token)
                .is_some_and(|(expected, token)| matches_local_credential(expected, token));
        let valid =
            is_local || token.is_some_and(|token| ctx.verifier.verify(token, password_hash));
        if !valid {
            return json_response(
                401,
                &Value::Object(Map::from_iter([(
                    "error".to_owned(),
                    Value::from("Unauthorized"),
                )])),
                ctx.now_ms,
                &cors,
            );
        }
    }

    let is_get = request.method == "GET" || request.method == "HEAD";
    let response = match (is_get, path) {
        (true, "/api/health") => Some(Value::Object(Map::from_iter([
            ("status".to_owned(), Value::from("ok")),
            (
                "timestamp".to_owned(),
                Value::from(to_iso_string(ctx.now_ms)),
            ),
        ]))),
        (true, "/api/status") => Some(Value::Object(Map::from_iter([
            ("status".to_owned(), Value::from("server_info")),
            ("serverId".to_owned(), Value::from(ctx.server_id)),
            ("hostname".to_owned(), Value::from(ctx.hostname)),
            ("version".to_owned(), Value::from(ctx.version)),
            ("listen".to_owned(), Value::from(ctx.listen)),
        ]))),
        _ => None,
    };
    match response {
        Some(body) => {
            let mut response = json_response(200, &body, ctx.now_ms, &cors);
            if request.method == "HEAD" {
                response.body.clear();
            }
            response
        }
        None => not_found(request, ctx.now_ms, &cors),
    }
}

/// The current time for [`HttpContext::now_ms`].
#[must_use]
pub fn current_ms() -> i64 {
    now_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Exact;

    impl PasswordVerifier for Exact {
        fn verify(&self, password: &str, password_hash: &str) -> bool {
            password == "secret" && password_hash == "hash"
        }
    }

    const NOW: i64 = 1_790_867_824_000; // Thu, 01 Oct 2026 15:17:04 GMT

    fn request(method: &str, url: &str, headers: &[(&str, &str)]) -> UpgradeRequest {
        UpgradeRequest {
            method: method.to_owned(),
            url: url.to_owned(),
            headers: headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn respond(
        request: &UpgradeRequest,
        password_hash: Option<&str>,
        origins: &HashSet<String>,
        tcp: bool,
    ) -> HttpResponse {
        handle_request(
            request,
            &HttpContext {
                server_id: "srv_x",
                hostname: "box",
                version: "0.10.0",
                listen: "127.0.0.1:7000",
                tcp_listener: tcp,
                hostnames: None,
                allowed_origins: origins,
                password_hash,
                local_credential: Some("LOCALTOKEN"),
                verifier: &Exact,
                now_ms: NOW,
            },
        )
    }

    fn text(response: &HttpResponse) -> String {
        String::from_utf8(response.to_bytes()).unwrap()
    }

    #[test]
    fn dates_and_etags_match_node_and_express() {
        assert_eq!(http_date(NOW), "Thu, 01 Oct 2026 15:17:04 GMT");
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(86_400_000 * 3), "Sun, 04 Jan 1970 00:00:00 GMT");
        // Values produced by Express 5 `res.json` for these bodies.
        assert_eq!(
            weak_etag(br#"{"status":"server_info","serverId":"srv_x"}"#),
            "W/\"2b-ZGTApX/EkwW4zZcnL5RdjOCrQXY\""
        );
        assert_eq!(
            weak_etag(br#"{"error":"Invalid Host header"}"#),
            "W/\"1f-NqqN66y+wMc2D87EuO28QwDGNws\""
        );
    }

    #[test]
    fn the_forbidden_host_response_matches_express_bytes() {
        let response = respond(
            &request("GET", "/api/status", &[("Host", "evil.example")]),
            None,
            &HashSet::new(),
            true,
        );
        assert_eq!(
            text(&response),
            "HTTP/1.1 403 Forbidden\r\nX-Powered-By: Express\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 31\r\nETag: W/\"1f-NqqN66y+wMc2D87EuO28QwDGNws\"\r\nDate: Thu, 01 Oct 2026 15:17:04 GMT\r\nConnection: close\r\n\r\n{\"error\":\"Invalid Host header\"}"
        );
    }

    #[test]
    fn a_socket_listener_skips_the_host_check() {
        let response = respond(
            &request("GET", "/api/health", &[("Host", "evil.example")]),
            None,
            &HashSet::new(),
            false,
        );
        assert_eq!(response.status, 200);
        let missing = respond(
            &request("GET", "/api/health", &[]),
            None,
            &HashSet::new(),
            true,
        );
        assert_eq!(missing.status, 403);
    }

    #[test]
    fn status_and_health_bodies() {
        let origins = HashSet::new();
        let status = respond(
            &request("GET", "/api/status?x=1", &[("Host", "127.0.0.1:7000")]),
            None,
            &origins,
            true,
        );
        assert_eq!(status.status, 200);
        assert_eq!(
            String::from_utf8(status.body).unwrap(),
            r#"{"status":"server_info","serverId":"srv_x","hostname":"box","version":"0.10.0","listen":"127.0.0.1:7000"}"#
        );
        let health = respond(
            &request("GET", "/api/health", &[("Host", "localhost")]),
            None,
            &origins,
            true,
        );
        assert_eq!(
            String::from_utf8(health.body).unwrap(),
            r#"{"status":"ok","timestamp":"2026-10-01T15:17:04.000Z"}"#
        );
    }

    #[test]
    fn head_keeps_the_headers_and_drops_the_body() {
        let get = respond(
            &request("GET", "/api/status", &[("Host", "localhost")]),
            None,
            &HashSet::new(),
            true,
        );
        let head = respond(
            &request("HEAD", "/api/status", &[("Host", "localhost")]),
            None,
            &HashSet::new(),
            true,
        );
        assert_eq!(head.headers, get.headers);
        assert!(head.body.is_empty());
    }

    #[test]
    fn unknown_routes_and_methods_get_the_express_404_page() {
        let response = respond(
            &request("POST", "/api/status", &[("Host", "localhost")]),
            None,
            &HashSet::new(),
            true,
        );
        assert_eq!(
            text(&response),
            "HTTP/1.1 404 Not Found\r\nX-Powered-By: Express\r\nContent-Security-Policy: default-src 'none'\r\nX-Content-Type-Options: nosniff\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 150\r\nDate: Thu, 01 Oct 2026 15:17:04 GMT\r\nConnection: close\r\n\r\n<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot POST /api/status</pre>\n</body>\n</html>\n"
        );
        let odd = respond(
            &request("GET", "/nope<x", &[("Host", "localhost")]),
            None,
            &HashSet::new(),
            true,
        );
        assert!(
            String::from_utf8(odd.body)
                .unwrap()
                .contains("Cannot GET /nope%3Cx")
        );
    }

    #[test]
    fn cors_headers_follow_the_allowed_origins_and_options_answers_204() {
        let origins: HashSet<String> = ["https://app.example".to_owned()].into();
        let options = respond(
            &request(
                "OPTIONS",
                "/api/status",
                &[("Host", "localhost"), ("Origin", "https://app.example")],
            ),
            Some("hash"),
            &origins,
            true,
        );
        assert_eq!(options.status, 204);
        assert!(options.body.is_empty());
        assert!(options.headers.contains(&(
            "Access-Control-Allow-Origin".to_owned(),
            "https://app.example".to_owned()
        )));
        let foreign = respond(
            &request(
                "GET",
                "/api/health",
                &[("Host", "localhost"), ("Origin", "https://evil.example")],
            ),
            None,
            &origins,
            true,
        );
        assert!(
            !foreign
                .headers
                .iter()
                .any(|(n, _)| n.starts_with("Access-Control"))
        );
        let star: HashSet<String> = ["*".to_owned()].into();
        let any = respond(
            &request(
                "GET",
                "/api/health",
                &[("Host", "localhost"), ("Origin", "https://evil.example")],
            ),
            None,
            &star,
            true,
        );
        assert!(
            any.headers
                .iter()
                .any(|(n, _)| n == "Access-Control-Allow-Origin")
        );
    }

    #[test]
    fn a_password_protects_everything_but_health_and_options() {
        let origins = HashSet::new();
        let call = |method: &str, path: &str, auth: Option<&str>| {
            let mut headers = vec![("Host", "localhost")];
            if let Some(auth) = auth {
                headers.push(("Authorization", auth));
            }
            respond(
                &request(method, path, &headers),
                Some("hash"),
                &origins,
                true,
            )
            .status
        };
        assert_eq!(call("GET", "/api/status", None), 401);
        assert_eq!(call("GET", "/api/status", Some("Bearer wrong")), 401);
        assert_eq!(call("GET", "/api/status", Some("Basic secret")), 401);
        assert_eq!(call("GET", "/api/status", Some("Bearer secret")), 200);
        assert_eq!(call("GET", "/api/health", None), 200);
        assert_eq!(call("OPTIONS", "/api/status", None), 204);
        assert_eq!(call("GET", "/other", None), 401);
        assert_eq!(call("GET", "/api/files/download", None), 404);
    }

    #[test]
    fn the_local_credential_unlocks_only_api_status() {
        let origins = HashSet::new();
        let call = |path: &str| {
            let headers = [
                ("Host", "localhost"),
                ("Authorization", "Bearer LOCALTOKEN"),
            ];
            respond(
                &request("GET", path, &headers),
                Some("hash"),
                &origins,
                true,
            )
            .status
        };
        assert_eq!(call("/api/status"), 200);
        assert_eq!(call("/other"), 401);
    }

    #[test]
    fn unauthorized_is_a_json_401_with_cors_headers() {
        let origins: HashSet<String> = ["*".to_owned()].into();
        let response = respond(
            &request(
                "GET",
                "/api/status",
                &[("Host", "localhost"), ("Origin", "https://a.example")],
            ),
            Some("hash"),
            &origins,
            true,
        );
        assert_eq!(response.status, 401);
        assert_eq!(
            String::from_utf8(response.body.clone()).unwrap(),
            r#"{"error":"Unauthorized"}"#
        );
        assert!(
            response
                .headers
                .iter()
                .any(|(n, _)| n == "Access-Control-Allow-Origin")
        );
    }

    #[test]
    fn parses_complete_partial_and_invalid_heads() {
        let head =
            b"GET /ws HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nX-A:  padded \r\n\r\nEXTRA";
        let ParsedHead::Complete(parsed, used) = parse_head(head) else {
            panic!("expected a complete head");
        };
        assert_eq!(&head[used..], b"EXTRA");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.url, "/ws");
        assert_eq!(parsed.header("x-a").as_deref(), Some("padded"));
        assert_eq!(
            parse_head(b"GET /ws HTTP/1.1\r\nHost:"),
            ParsedHead::Partial
        );
        assert_eq!(
            parse_head(b"\x16\x03\x01garbage\r\n\r\n"),
            ParsedHead::Invalid
        );
        let huge = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEADER_BYTES)
        );
        assert_eq!(parse_head(huge.as_bytes()), ParsedHead::TooLarge);
        let unterminated = vec![b'a'; MAX_HEADER_BYTES + 1];
        assert_eq!(parse_head(&unterminated), ParsedHead::TooLarge);
    }

    #[test]
    fn header_values_are_decoded_as_latin1_like_node() {
        let head = b"GET / HTTP/1.1\r\nX-A: caf\xE9\r\nX-B: \xC3\xA9\r\nX-C: \xA0x\xA0 \r\n\r\n";
        let ParsedHead::Complete(parsed, _) = parse_head(head) else {
            panic!("expected a complete head");
        };
        assert_eq!(parsed.header("x-a").as_deref(), Some("caf\u{e9}"));
        assert_eq!(parsed.header("x-b").as_deref(), Some("\u{c3}\u{a9}"));
        // Only spaces and tabs are trimmed; U+00A0 is data.
        assert_eq!(parsed.header("x-c").as_deref(), Some("\u{a0}x\u{a0}"));
    }

    #[test]
    fn upgrade_needs_both_headers_and_a_connection_token() {
        let up = |headers: &[(&str, &str)]| is_upgrade_request(&request("GET", "/ws", headers));
        assert!(up(&[("Upgrade", "websocket"), ("Connection", "Upgrade")]));
        assert!(up(&[
            ("Upgrade", "websocket"),
            ("Connection", "keep-alive, upgrade")
        ]));
        assert!(!up(&[("Upgrade", "websocket")]));
        assert!(!up(&[("Connection", "Upgrade")]));
        assert!(!up(&[
            ("Upgrade", "websocket"),
            ("Connection", "keep-alive")
        ]));
    }

    #[test]
    fn client_errors_close_the_connection() {
        assert_eq!(
            HttpResponse::client_error(400).to_bytes(),
            b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n"
        );
    }
}
