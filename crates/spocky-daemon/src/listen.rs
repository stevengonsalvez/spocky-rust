//! Listen address precedence and parsing.
//!
//! Sources at Paseo `5de45e2`: `resolveListenAddress` in `config.ts` and
//! `parseListenString` and `formatListenTarget` in `bootstrap.ts`.

use std::fmt;

/// `DEFAULT_PORT` in `config.ts`.
const DEFAULT_PORT: &str = "6767";

/// `ListenTarget` in `bootstrap.ts`. The TCP port keeps the `parseInt` value
/// unchecked: Node rejects an out-of-range port when it binds, not when it
/// parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenTarget {
    Tcp { host: String, port: i64 },
    Socket { path: String },
    Pipe { path: String },
}

/// Message of the `Error` that `parseListenString` throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenError(pub String);

impl fmt::Display for ListenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ListenError {}

/// `resolveListenAddress`: command line, then `PASEO_LISTEN`, then the
/// persisted `daemon.listen`, then `127.0.0.1:${PORT ?? 6767}`. Each `??`
/// skips only an absent value, so an empty string wins and fails to parse.
#[must_use]
pub fn resolve_listen_address(
    cli: Option<&str>,
    env_listen: Option<&str>,
    persisted: Option<&str>,
    env_port: Option<&str>,
) -> String {
    cli.or(env_listen).or(persisted).map_or_else(
        || format!("127.0.0.1:{}", env_port.unwrap_or(DEFAULT_PORT)),
        str::to_owned,
    )
}

/// `parseListenString`.
///
/// # Errors
///
/// Returns the pinned error message for a Windows drive path, a TCP string
/// whose port is not a number, or a string with no recognised form.
pub fn parse_listen_string(listen: &str) -> Result<ListenTarget, ListenError> {
    if listen.starts_with("\\\\.\\pipe\\") || listen.starts_with("pipe://") {
        return Ok(ListenTarget::Pipe {
            path: listen.strip_prefix("pipe://").unwrap_or(listen).to_owned(),
        });
    }
    if let Some(path) = listen.strip_prefix("unix://") {
        return Ok(ListenTarget::Socket {
            path: path.to_owned(),
        });
    }
    if is_windows_drive_path(listen) {
        return Err(ListenError(format!(
            "Invalid listen string (Windows path is not a valid listen target): {listen}"
        )));
    }
    if listen.starts_with('/') || listen.starts_with('~') {
        return Ok(ListenTarget::Socket {
            path: listen.to_owned(),
        });
    }
    let trimmed = trim_js(listen);
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(ListenTarget::Tcp {
            host: "127.0.0.1".to_owned(),
            port: parse_int_radix10(trimmed).unwrap_or(i64::MAX),
        });
    }
    if let Some(last_colon) = listen.rfind(':') {
        let (host, port_text) = (&listen[..last_colon], &listen[last_colon + 1..]);
        let Some(port) = parse_int_radix10(port_text) else {
            return Err(ListenError(format!(
                "Invalid port in listen string: {listen}"
            )));
        };
        let clean_host = host
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(host);
        return Ok(ListenTarget::Tcp {
            host: if clean_host.is_empty() {
                "127.0.0.1".to_owned()
            } else {
                clean_host.to_owned()
            },
            port,
        });
    }
    Err(ListenError(format!("Invalid listen string: {listen}")))
}

/// `formatListenTarget` for a present target: `formatHostForHttpUrl` brackets
/// an IPv6 host, a socket or pipe is its path.
#[must_use]
pub fn format_listen_target(target: &ListenTarget) -> String {
    match target {
        ListenTarget::Tcp { host, port } => {
            if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            }
        }
        ListenTarget::Socket { path } | ListenTarget::Pipe { path } => path.clone(),
    }
}

/// `/^[A-Za-z]:\\/`.
fn is_windows_drive_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// `String.prototype.trim` whitespace: `WhiteSpace` and `LineTerminator`.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{a}'
            | '\u{b}'
            | '\u{c}'
            | '\u{d}'
            | '\u{20}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn trim_js(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// `parseInt(text, 10)` as a finite number: skip leading whitespace, take an
/// optional sign, then the longest run of ASCII digits. `None` is `NaN`. A run
/// too long for `i64` saturates, and the bind check rejects it as out of range.
fn parse_int_radix10(text: &str) -> Option<i64> {
    let rest = text.trim_start_matches(is_js_whitespace);
    let (negative, rest) = match rest.as_bytes().first() {
        Some(b'-') => (true, &rest[1..]),
        Some(b'+') => (false, &rest[1..]),
        _ => (false, rest),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let magnitude = rest[..digits]
        .bytes()
        .try_fold(0_i64, |acc, b| {
            acc.checked_mul(10)?.checked_add(i64::from(b - b'0'))
        })
        .unwrap_or(i64::MAX);
    Some(if negative { -magnitude } else { magnitude })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp(host: &str, port: i64) -> ListenTarget {
        ListenTarget::Tcp {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn precedence_takes_the_first_present_source() {
        assert_eq!(
            resolve_listen_address(
                Some("1.1.1.1:1"),
                Some("2.2.2.2:2"),
                Some("3.3.3.3:3"),
                Some("9")
            ),
            "1.1.1.1:1"
        );
        assert_eq!(
            resolve_listen_address(None, Some("2.2.2.2:2"), Some("3.3.3.3:3"), Some("9")),
            "2.2.2.2:2"
        );
        assert_eq!(
            resolve_listen_address(None, None, Some("3.3.3.3:3"), Some("9")),
            "3.3.3.3:3"
        );
        assert_eq!(
            resolve_listen_address(None, None, None, Some("9")),
            "127.0.0.1:9"
        );
        assert_eq!(
            resolve_listen_address(None, None, None, None),
            "127.0.0.1:6767"
        );
    }

    #[test]
    fn an_empty_string_is_present_and_wins() {
        let resolved = resolve_listen_address(None, Some(""), Some("3.3.3.3:3"), None);
        assert_eq!(resolved, "");
        assert_eq!(
            parse_listen_string(&resolved),
            Err(ListenError("Invalid listen string: ".to_owned()))
        );
    }

    #[test]
    fn parses_tcp_forms() {
        assert_eq!(
            parse_listen_string("127.0.0.1:6767"),
            Ok(tcp("127.0.0.1", 6767))
        );
        assert_eq!(parse_listen_string("8080"), Ok(tcp("127.0.0.1", 8080)));
        assert_eq!(parse_listen_string(" 8080 "), Ok(tcp("127.0.0.1", 8080)));
        assert_eq!(parse_listen_string("[::1]:7000"), Ok(tcp("::1", 7000)));
        assert_eq!(parse_listen_string(":7000"), Ok(tcp("127.0.0.1", 7000)));
        assert_eq!(
            parse_listen_string("localhost:7000x"),
            Ok(tcp("localhost", 7000))
        );
        assert_eq!(parse_listen_string("0.0.0.0:0"), Ok(tcp("0.0.0.0", 0)));
        assert_eq!(parse_listen_string("host:-5"), Ok(tcp("host", -5)));
        assert_eq!(parse_listen_string("host: 12"), Ok(tcp("host", 12)));
    }

    #[test]
    fn an_unbracketed_ipv6_string_splits_at_the_last_colon() {
        assert_eq!(parse_listen_string("::1"), Ok(tcp(":", 1)));
    }

    #[test]
    fn parses_socket_and_pipe_forms() {
        let socket = |path: &str| ListenTarget::Socket {
            path: path.to_owned(),
        };
        let pipe = |path: &str| ListenTarget::Pipe {
            path: path.to_owned(),
        };
        assert_eq!(
            parse_listen_string("/tmp/p.sock"),
            Ok(socket("/tmp/p.sock"))
        );
        assert_eq!(parse_listen_string("~/p.sock"), Ok(socket("~/p.sock")));
        assert_eq!(
            parse_listen_string("unix:///tmp/p.sock"),
            Ok(socket("/tmp/p.sock"))
        );
        assert_eq!(parse_listen_string("pipe://paseo"), Ok(pipe("paseo")));
        assert_eq!(
            parse_listen_string("\\\\.\\pipe\\paseo"),
            Ok(pipe("\\\\.\\pipe\\paseo"))
        );
    }

    #[test]
    fn rejects_bad_strings_with_the_pinned_messages() {
        assert_eq!(
            parse_listen_string("C:\\x\\y"),
            Err(ListenError(
                "Invalid listen string (Windows path is not a valid listen target): C:\\x\\y"
                    .to_owned()
            ))
        );
        assert_eq!(
            parse_listen_string("host:abc"),
            Err(ListenError(
                "Invalid port in listen string: host:abc".to_owned()
            ))
        );
        assert_eq!(
            parse_listen_string("host:"),
            Err(ListenError(
                "Invalid port in listen string: host:".to_owned()
            ))
        );
        assert_eq!(
            parse_listen_string("nonsense"),
            Err(ListenError("Invalid listen string: nonsense".to_owned()))
        );
    }

    #[test]
    fn a_huge_port_saturates_instead_of_wrapping() {
        assert_eq!(
            parse_listen_string("99999999999999999999999"),
            Ok(tcp("127.0.0.1", i64::MAX))
        );
    }

    #[test]
    fn formats_targets_like_format_listen_target() {
        assert_eq!(
            format_listen_target(&tcp("127.0.0.1", 6767)),
            "127.0.0.1:6767"
        );
        assert_eq!(format_listen_target(&tcp("::1", 7000)), "[::1]:7000");
        assert_eq!(
            format_listen_target(&ListenTarget::Socket {
                path: "/tmp/p.sock".to_owned()
            }),
            "/tmp/p.sock"
        );
    }
}
