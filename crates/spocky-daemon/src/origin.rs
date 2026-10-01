//! WebSocket Origin admission against the request Host.
//!
//! Source at Paseo `5de45e2`: `isWebSocketSameOrigin` and its helpers in
//! `websocket-server.ts`.

use url::Url;

use crate::js;

struct HostAuthority {
    hostname: String,
    port: Option<String>,
}

fn strip_ipv6_brackets(hostname: &str) -> &str {
    hostname
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(hostname)
}

/// `parseHostAuthority`.
fn parse_host_authority(host: &str) -> Option<HostAuthority> {
    let trimmed = js::trim(host);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('[') {
        let end = trimmed.find(']')?;
        let hostname = strip_ipv6_brackets(&trimmed[..=end]).to_lowercase();
        let rest = &trimmed[end + 1..];
        if rest.is_empty() {
            return Some(HostAuthority {
                hostname,
                port: None,
            });
        }
        let port = rest.strip_prefix(':')?;
        return (!port.is_empty()).then(|| HostAuthority {
            hostname,
            port: Some(port.to_owned()),
        });
    }
    let Some(first_colon) = trimmed.find(':') else {
        return Some(HostAuthority {
            hostname: trimmed.to_lowercase(),
            port: None,
        });
    };
    if trimmed[first_colon + 1..].contains(':') {
        return Some(HostAuthority {
            hostname: trimmed.to_lowercase(),
            port: None,
        });
    }
    let hostname = trimmed[..first_colon].to_lowercase();
    let port = &trimmed[first_colon + 1..];
    (!hostname.is_empty() && !port.is_empty()).then(|| HostAuthority {
        hostname,
        port: Some(port.to_owned()),
    })
}

/// `defaultPortForOriginProtocol`; the protocol includes the trailing colon.
fn default_port_for_origin_protocol(protocol: &str) -> Option<&'static str> {
    match protocol {
        "http:" => Some("80"),
        "https:" => Some("443"),
        _ => None,
    }
}

/// `isLoopbackAlias`.
fn is_loopback_alias(hostname: &str) -> bool {
    let normalized = strip_ipv6_brackets(hostname).to_lowercase();
    if normalized == "localhost" || normalized.ends_with(".localhost") {
        return true;
    }
    if normalized == "::1" || normalized == "0:0:0:0:0:0:0:1" {
        return true;
    }
    let mut parts = normalized.split('.');
    parts.next() == Some("127")
        && parts
            .by_ref()
            .take(3)
            .filter(|part| (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
            .count()
            == 3
        && parts.next().is_none()
}

/// `isWebSocketSameOrigin`.
#[must_use]
pub fn is_web_socket_same_origin(origin: Option<&str>, request_host: Option<&str>) -> bool {
    let (Some(origin), Some(request_host)) = (
        origin.filter(|origin| !origin.is_empty()),
        request_host.filter(|host| !host.is_empty()),
    ) else {
        return false;
    };
    if origin == format!("http://{request_host}") || origin == format!("https://{request_host}") {
        return true;
    }
    let Ok(origin_url) = Url::parse(origin) else {
        return false;
    };
    let protocol = format!("{}:", origin_url.scheme());
    let origin_port = origin_url
        .port()
        .map(|port| port.to_string())
        .or_else(|| default_port_for_origin_protocol(&protocol).map(str::to_owned));
    let Some(origin_port) = origin_port else {
        return false;
    };
    let Some(request_authority) = parse_host_authority(request_host) else {
        return false;
    };
    let request_port = request_authority
        .port
        .or_else(|| default_port_for_origin_protocol(&protocol).map(str::to_owned));
    if request_port.as_deref() != Some(origin_port.as_str()) {
        return false;
    }
    origin_url.host_str().is_some_and(is_loopback_alias)
        && is_loopback_alias(&request_authority.hostname)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(origin: &str, host: &str) -> bool {
        is_web_socket_same_origin(Some(origin), Some(host))
    }

    #[test]
    fn missing_origin_or_host_is_not_same_origin() {
        assert!(!is_web_socket_same_origin(None, Some("localhost:1")));
        assert!(!is_web_socket_same_origin(Some("http://localhost:1"), None));
        assert!(!is_web_socket_same_origin(Some(""), Some("localhost:1")));
    }

    #[test]
    fn an_exact_scheme_and_host_match_is_same_origin() {
        assert!(same("http://example.com:6767", "example.com:6767"));
        assert!(same("https://example.com", "example.com"));
        assert!(!same("http://example.com:6767", "example.com:6768"));
    }

    #[test]
    fn loopback_aliases_with_equal_ports_are_same_origin() {
        assert!(same("http://localhost:6767", "127.0.0.1:6767"));
        assert!(same("http://127.0.0.1:6767", "localhost:6767"));
        assert!(same("http://[::1]:6767", "localhost:6767"));
        assert!(same("http://localhost:6767", "[::1]:6767"));
        assert!(same("http://app.localhost:80", "localhost"));
        assert!(same("https://localhost", "127.0.0.1:443"));
        assert!(same("http://127.1:6767", "127.0.0.1:6767"));
    }

    #[test]
    fn loopback_aliases_with_different_ports_are_not_same_origin() {
        assert!(!same("http://localhost:6767", "127.0.0.1:6768"));
        assert!(!same("http://localhost", "127.0.0.1:6767"));
    }

    #[test]
    fn a_non_loopback_origin_is_not_aliased() {
        assert!(!same("http://evil.example:6767", "127.0.0.1:6767"));
        assert!(!same("http://localhost:6767", "evil.example:6767"));
        assert!(!same("http://127.0.0.1.evil.example:6767", "127.0.0.1:6767"));
    }

    #[test]
    fn schemes_without_a_default_port_need_an_explicit_port() {
        assert!(!same("paseo://app", "localhost"));
        assert!(!same("not a url", "localhost"));
    }

    #[test]
    fn host_authority_parsing_follows_the_pinned_rules() {
        let parse = |host| parse_host_authority(host).map(|a| (a.hostname, a.port));
        assert_eq!(parse("Host:80"), Some(("host".into(), Some("80".into()))));
        assert_eq!(parse("[::1]"), Some(("::1".into(), None)));
        assert_eq!(parse("[::1]:9"), Some(("::1".into(), Some("9".into()))));
        assert_eq!(parse("[::1]x"), None);
        assert_eq!(parse("[::1]:"), None);
        assert_eq!(parse("a:b:c"), Some(("a:b:c".into(), None)));
        assert_eq!(parse("host:"), None);
        assert_eq!(parse(":80"), None);
        assert_eq!(parse(" "), None);
    }
}
