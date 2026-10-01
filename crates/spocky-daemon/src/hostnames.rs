//! Host header allowlist.
//!
//! Source at Paseo `5de45e2`: `hostnames.ts`.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::js;

/// `HostnamesConfig` other than `undefined`: `true` allows any host, a list
/// adds patterns to the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hostnames {
    Any,
    Patterns(Vec<String>),
}

fn normalize_hostname(hostname: &str) -> String {
    js::trim(hostname).to_lowercase()
}

/// `parseHostnameFromHostHeader`.
fn parse_hostname_from_host_header(host_header: &str) -> Option<String> {
    let trimmed = js::trim(host_header);
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']')?;
        return Some(normalize_hostname(&rest[..end]));
    }
    Some(normalize_hostname(
        trimmed.find(':').map_or(trimmed, |colon| &trimmed[..colon]),
    ))
}

/// `matchesHostnamePattern`.
fn matches_hostname_pattern(hostname: &str, pattern: &str) -> bool {
    let normalized = normalize_hostname(pattern);
    if normalized.is_empty() {
        return false;
    }
    if let Some(base) = normalized.strip_prefix('.') {
        return !base.is_empty()
            && (hostname == base
                || hostname
                    .strip_suffix(base)
                    .is_some_and(|head| head.ends_with('.')));
    }
    hostname == normalized
}

/// `net.isIP(hostname) !== 0`. Node accepts an IPv6 zone id after `%`.
fn is_ip(hostname: &str) -> bool {
    if hostname.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    let (address, zone) = hostname
        .split_once('%')
        .map_or((hostname, None), |(address, zone)| (address, Some(zone)));
    if let Some(zone) = zone
        && (zone.is_empty()
            || !zone
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b':' | b'_')))
    {
        return false;
    }
    address.parse::<Ipv6Addr>().is_ok()
}

/// `isDefaultAllowedHostname`: `localhost`, `*.localhost`, and every IP.
fn is_default_allowed_hostname(hostname: &str) -> bool {
    hostname == "localhost" || hostname.ends_with(".localhost") || is_ip(hostname)
}

/// `isHostnameAllowed`.
#[must_use]
pub fn is_hostname_allowed(host_header: Option<&str>, hostnames: Option<&Hostnames>) -> bool {
    let Some(hostname) = host_header
        .filter(|header| !header.is_empty())
        .and_then(parse_hostname_from_host_header)
        .filter(|hostname| !hostname.is_empty())
    else {
        return false;
    };
    if matches!(hostnames, Some(Hostnames::Any)) {
        return true;
    }
    if is_default_allowed_hostname(&hostname) {
        return true;
    }
    match hostnames {
        Some(Hostnames::Patterns(patterns)) => patterns
            .iter()
            .any(|pattern| matches_hostname_pattern(&hostname, pattern)),
        _ => false,
    }
}

/// `mergeHostnames`: `true` wins, an absent value is skipped, lists are
/// concatenated then trimmed, emptied, and deduplicated in first-seen order.
#[must_use]
pub fn merge_hostnames(values: &[Option<Hostnames>]) -> Hostnames {
    let mut merged: Vec<String> = Vec::new();
    for value in values {
        match value {
            Some(Hostnames::Any) => return Hostnames::Any,
            Some(Hostnames::Patterns(patterns)) => {
                for pattern in patterns {
                    let trimmed = js::trim(pattern);
                    if !trimmed.is_empty() && !merged.iter().any(|seen| seen == trimmed) {
                        merged.push(trimmed.to_owned());
                    }
                }
            }
            None => {}
        }
    }
    Hostnames::Patterns(merged)
}

/// `parseHostnamesEnv`.
#[must_use]
pub fn parse_hostnames_env(raw: Option<&str>) -> Option<Hostnames> {
    let trimmed = js::trim(raw?);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.to_lowercase() == "true" {
        return Some(Hostnames::Any);
    }
    Some(Hostnames::Patterns(
        trimmed
            .split(',')
            .map(|part| js::trim(part).to_owned())
            .filter(|part| !part.is_empty())
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Hostnames {
        Hostnames::Patterns(list.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn defaults_allow_localhost_subdomains_and_every_ip() {
        for host in [
            "localhost",
            "localhost:6767",
            "app.localhost",
            "127.0.0.1:1",
            "[::1]:6767",
            "[fe80::1%eth0]",
            "10.0.0.5",
        ] {
            assert!(is_hostname_allowed(Some(host), None), "{host}");
        }
    }

    #[test]
    fn rejects_other_names_missing_and_malformed_headers() {
        for host in [
            "evil.example",
            "127.1",
            "256.1.1.1",
            "[::1",
            "",
            " ",
            "[]:1",
            "::1",
        ] {
            assert!(!is_hostname_allowed(Some(host), None), "{host}");
        }
        assert!(!is_hostname_allowed(None, None));
        assert!(!is_hostname_allowed(None, Some(&Hostnames::Any)));
    }

    #[test]
    fn any_allows_every_parsable_host() {
        assert!(is_hostname_allowed(
            Some("evil.example"),
            Some(&Hostnames::Any)
        ));
    }

    #[test]
    fn patterns_match_exact_names_and_dot_suffixes() {
        let list = patterns(&[".example.com", "Myhost"]);
        let allowed = |host| is_hostname_allowed(Some(host), Some(&list));
        assert!(allowed("example.com"));
        assert!(allowed("a.b.example.com:80"));
        assert!(allowed("MYHOST"));
        assert!(!allowed("notexample.com"));
        assert!(!allowed("example.com.evil"));
        assert!(!allowed("other"));
    }

    #[test]
    fn merge_dedupes_in_order_and_true_wins() {
        assert_eq!(
            merge_hostnames(&[
                Some(patterns(&[" a ", "b"])),
                None,
                Some(patterns(&["a", "", "c"]))
            ]),
            Hostnames::Patterns(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(merge_hostnames(&[None]), Hostnames::Patterns(vec![]));
        assert_eq!(
            merge_hostnames(&[Some(patterns(&["a"])), Some(Hostnames::Any)]),
            Hostnames::Any
        );
    }

    #[test]
    fn parses_the_environment_value() {
        assert_eq!(parse_hostnames_env(None), None);
        assert_eq!(parse_hostnames_env(Some("  ")), None);
        assert_eq!(parse_hostnames_env(Some(" TRUE ")), Some(Hostnames::Any));
        assert_eq!(
            parse_hostnames_env(Some("a, b,,c ")),
            Some(patterns(&["a", "b", "c"]))
        );
        assert_eq!(parse_hostnames_env(Some(",")), Some(patterns(&[])));
    }
}
