//! Git remote parsing from pinned Paseo `protocol/src/git-remote.ts`.
//! URL remotes use the WHATWG URL parser, as `new URL()` does.

use url::Url;

/// GitHub cloud hosts from `forge-manifest.ts`.
const GITHUB_HOSTS: [&str; 2] = ["github.com", "ssh.github.com"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRemoteTransport {
    Scp,
    Ssh,
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemoteLocation {
    pub transport: GitRemoteTransport,
    pub host: String,
    pub port: Option<String>,
    pub path: String,
}

/// `isGitHubHost`: exact membership, no normalization.
#[must_use]
pub fn is_github_host(host: &str) -> bool {
    GITHUB_HOSTS.contains(&host)
}

/// `normalizeHost`.
#[must_use]
pub fn normalize_host(host: &str) -> String {
    js_trim(host).trim_end_matches('.').to_lowercase()
}

/// `String.prototype.trim`: ECMAScript white space and line terminators.
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
}

/// `normalizeRemotePath`.
fn normalize_remote_path(path: &str) -> Option<String> {
    let trimmed = js_trim(path).trim_matches('/');
    let normalized = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    (!normalized.is_empty()).then(|| normalized.to_owned())
}

/// `isValidRemoteHost`: `/^[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?$/u`.
fn is_valid_remote_host(host: &str) -> bool {
    let bytes = host.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let inner = |byte: &u8| edge(byte) || matches!(byte, b'.' | b'_' | b'-');
    match bytes {
        [] => false,
        [only] => edge(only),
        [first, middle @ .., last] => edge(first) && edge(last) && middle.iter().all(inner),
    }
}

/// `/^[^@]+@([^:]+):(.+)$/u`: `.` excludes ECMAScript line terminators.
fn match_scp_like(value: &str) -> Option<(&str, &str)> {
    let at = value.find('@')?;
    if at == 0 {
        return None;
    }
    let rest = &value[at + 1..];
    let colon = rest.find(':')?;
    if colon == 0 {
        return None;
    }
    let (host, path) = (&rest[..colon], &rest[colon + 1..]);
    let is_terminator =
        |character: char| matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    if path.is_empty() || path.contains(is_terminator) {
        return None;
    }
    Some((host, path))
}

/// `decodeURIComponent`; `None` where it throws `URIError`.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            let text = std::str::from_utf8(hex).ok()?;
            decoded.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// `parseGitRemoteLocation`.
#[must_use]
pub fn parse_git_remote_location(remote_url: &str) -> Option<GitRemoteLocation> {
    let trimmed = js_trim(remote_url);
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains("://")
        && let Some((host, path)) = match_scp_like(trimmed)
    {
        let host = normalize_host(host);
        let path = normalize_remote_path(path)?;
        if !is_valid_remote_host(&host) {
            return None;
        }
        return Some(GitRemoteLocation {
            transport: GitRemoteTransport::Scp,
            host,
            port: None,
            path,
        });
    }
    let parsed = Url::parse(trimmed).ok()?;
    let (transport, default_port) = match parsed.scheme() {
        "https" => (GitRemoteTransport::Https, "443"),
        "http" => (GitRemoteTransport::Http, "80"),
        "ssh" => (GitRemoteTransport::Ssh, "22"),
        _ => return None,
    };
    let host = normalize_host(parsed.host_str().unwrap_or_default());
    let path = normalize_remote_path(&decode_uri_component(parsed.path())?)?;
    if !is_valid_remote_host(&host) {
        return None;
    }
    let port = parsed
        .port()
        .map(|port| port.to_string())
        .filter(|port| port != default_port);
    Some(GitRemoteLocation {
        transport,
        host,
        port,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::{GitRemoteTransport, parse_git_remote_location};

    fn summary(remote: &str) -> Option<(GitRemoteTransport, String, Option<String>, String)> {
        parse_git_remote_location(remote).map(|location| {
            (
                location.transport,
                location.host,
                location.port,
                location.path,
            )
        })
    }

    // Expected values follow pinned `git-remote.ts` run under node 22.
    #[test]
    fn parses_like_baseline() {
        assert_eq!(
            summary("git@GitHub.com:Owner/Repo.git"),
            Some((
                GitRemoteTransport::Scp,
                "github.com".into(),
                None,
                "Owner/Repo".into()
            ))
        );
        assert_eq!(
            summary("https://github.com/owner/repo.git/"),
            Some((
                GitRemoteTransport::Https,
                "github.com".into(),
                None,
                "owner/repo".into()
            ))
        );
        assert_eq!(
            summary("ssh://git@host.example:2222/a/b"),
            Some((
                GitRemoteTransport::Ssh,
                "host.example".into(),
                Some("2222".into()),
                "a/b".into()
            ))
        );
        assert_eq!(
            summary("ssh://git@host.example:22/a/b").map(|parsed| parsed.2),
            Some(None)
        );
        assert_eq!(
            summary("https://h.example:443/a%20b").map(|parsed| (parsed.2, parsed.3)),
            Some((None, "a b".into()))
        );
        assert_eq!(summary("https://h.example/%E0%A4%A"), None);
        // `u8::from_str_radix` alone would accept the sign in "%+f".
        assert_eq!(super::decode_uri_component("a%+fb"), None);
        assert_eq!(super::decode_uri_component("a%2Fb").as_deref(), Some("a/b"));
        assert_eq!(summary("file:///tmp/repo"), None);
        assert_eq!(summary("git://host/repo"), None);
        assert_eq!(summary("   "), None);
        assert_eq!(summary("https://h.example/"), None);
        assert_eq!(summary("me@bad_host!:x"), None);
    }
}
