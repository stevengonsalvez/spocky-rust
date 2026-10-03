//! Home files the baseline bootstrap writes before the daemon listens.
//!
//! `createAgentProviderRuntime` starts the `OpenCode` bridge unconditionally
//! (`bootstrap.ts`), and `OpenCodeBridge.start` materializes its plugin
//! (`agent/providers/opencode/bridge.ts` `materializePlugin`): the built
//! `bridge-plugin.bundle.mjs`, written atomically to
//! `$PASEO_HOME/runtime/opencode/paseo-<sha256 of the bytes>.mjs`.
//!
//! `assets/opencode-bridge-plugin.bundle.mjs` is that file copied byte for
//! byte from the pinned Paseo `5de45e2` build
//! (`packages/server/dist/server/server/agent/providers/opencode/`), produced
//! by `packages/server/scripts/build-opencode-bridge-plugin.mjs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use spocky_contracts::url::Url;
use spocky_daemon::listen::ListenTarget;
use spocky_session::clock::random_uuid;

/// The pinned `OpenCode` bridge plugin bundle.
const OPENCODE_BRIDGE_PLUGIN: &[u8] = include_bytes!("../assets/opencode-bridge-plugin.bundle.mjs");

/// SHA-256 of the pinned bundle, as the pinned daemon names the file.
pub const OPENCODE_BRIDGE_PLUGIN_SHA256: &str =
    "a88cef53578dcb32cfa4af17e13e44c84751a30e5c19f847733904dd84eda872";

/// `createAgentMcpBaseUrl(boundListenTarget)`: the agent MCP url for the
/// target the listener bound, `None` for a socket or pipe listener. The url
/// goes through contracts' port of node's URL parser (ada 2.9.2), as
/// `new URL(..).toString()` does, so the host is lowercased, an IPv6 literal
/// compressed, an IPv4 form normalized and an international name converted by
/// ada's IDNA rules (the `url` crate follows another standard revision).
///
/// # Errors
///
/// `Invalid URL`, which the pinned URL constructor throws for a host it
/// rejects.
pub fn agent_mcp_base_url(bound: &ListenTarget) -> Result<Option<String>, String> {
    let ListenTarget::Tcp { host, port } = bound else {
        return Ok(None);
    };
    // `resolveAgentMcpClientHost`, then `formatHostForHttpUrl`.
    let host = match host.as_str() {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        other => other,
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    // `new URL("/mcp/agents", base)`: a base or result the parser rejects is
    // the `ERR_INVALID_URL` throw.
    Url::parse("/mcp/agents", Some(&format!("http://{host}:{port}")))
        .map(|url| Some(url.href()))
        .ok_or_else(|| "Invalid URL".to_owned())
}

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// `writeFileAtomic` (`atomic-file.ts`): create the parent directories,
/// write `.<name>.<pid>.<Date.now()>.<uuid>.tmp` beside the target, then
/// rename it over the target; the temporary file is removed on failure.
fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = directory.join(format!(
        ".{name}.{}.{millis}.{}.tmp",
        std::process::id(),
        random_uuid()
    ));
    let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// `materializePlugin()`: writes the `OpenCode` bridge plugin and returns its
/// path.
///
/// # Errors
///
/// A failure to create the directories or write the file, which fails the
/// baseline's bootstrap too.
pub fn materialize_opencode_bridge_plugin(paseo_home: &Path) -> io::Result<PathBuf> {
    let digest = hex_sha256(OPENCODE_BRIDGE_PLUGIN);
    let destination = paseo_home
        .join("runtime")
        .join("opencode")
        .join(format!("paseo-{digest}.mjs"));
    write_file_atomic(&destination, OPENCODE_BRIDGE_PLUGIN)?;
    Ok(destination)
}

/// The schedule store's directory. `ScheduleService.start` recovers
/// interrupted runs by listing the store, and `ScheduleStore` creates
/// `$PASEO_HOME/schedules` on first use (`schedule/store.ts`). The schedule
/// service itself is outside the slice; this is its bootstrap effect on the
/// home.
///
/// # Errors
///
/// A failure to create the directory, which fails the baseline's bootstrap.
pub fn ensure_schedule_store_dir(paseo_home: &Path) -> io::Result<()> {
    fs::create_dir_all(paseo_home.join("schedules"))
}

#[cfg(test)]
mod tests {
    use spocky_daemon::listen::ListenTarget;

    use super::{
        OPENCODE_BRIDGE_PLUGIN, OPENCODE_BRIDGE_PLUGIN_SHA256, agent_mcp_base_url,
        ensure_schedule_store_dir, hex_sha256, materialize_opencode_bridge_plugin,
    };

    #[test]
    fn embedded_bundle_is_the_pinned_build() {
        assert_eq!(OPENCODE_BRIDGE_PLUGIN.len(), 547_817);
        assert_eq!(
            hex_sha256(OPENCODE_BRIDGE_PLUGIN),
            OPENCODE_BRIDGE_PLUGIN_SHA256
        );
    }

    #[test]
    fn plugin_lands_under_its_digest_with_no_temporary_left() {
        let home = std::env::temp_dir().join(format!(
            "spocky-daemon-app-bootstrap-{}-{}",
            std::process::id(),
            spocky_session::clock::random_uuid()
        ));
        let path = materialize_opencode_bridge_plugin(&home).expect("materialize");
        assert_eq!(
            path,
            home.join("runtime")
                .join("opencode")
                .join(format!("paseo-{OPENCODE_BRIDGE_PLUGIN_SHA256}.mjs"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), OPENCODE_BRIDGE_PLUGIN);
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        // A second start rewrites the same file.
        materialize_opencode_bridge_plugin(&home).expect("rewrite");
        ensure_schedule_store_dir(&home).expect("schedules");
        ensure_schedule_store_dir(&home).expect("schedules again");
        assert!(home.join("schedules").is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }

    fn tcp(host: &str, port: i64) -> ListenTarget {
        ListenTarget::Tcp {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn the_agent_mcp_url_matches_new_url_for_the_bound_target() {
        // Printed by node 22: new URL("/mcp/agents", `http://${host}:${port}`)
        // after resolveAgentMcpClientHost and formatHostForHttpUrl.
        let cases = [
            ("127.0.0.1", 43211, "http://127.0.0.1:43211/mcp/agents"),
            ("0.0.0.0", 6767, "http://127.0.0.1:6767/mcp/agents"),
            ("::", 6767, "http://[::1]:6767/mcp/agents"),
            ("[::]", 6767, "http://[::1]:6767/mcp/agents"),
            ("::1", 7000, "http://[::1]:7000/mcp/agents"),
            ("localhost", 80, "http://localhost/mcp/agents"),
            ("LocalHost", 81, "http://localhost:81/mcp/agents"),
            ("[0:0:0:0:0:0:0:1]", 9, "http://[::1]:9/mcp/agents"),
            ("0:0:0:0:0:0:0:1", 9, "http://[::1]:9/mcp/agents"),
            ("EXAMPLE.com", 8080, "http://example.com:8080/mcp/agents"),
            ("127.1", 5, "http://127.0.0.1:5/mcp/agents"),
            ("0x7f.1", 6, "http://127.0.0.1:6/mcp/agents"),
            (
                "[::ffff:127.0.0.1]",
                8,
                "http://[::ffff:7f00:1]:8/mcp/agents",
            ),
            (
                "\u{dc}n\u{ef}.test",
                9,
                "http://xn--n-nga1b.test:9/mcp/agents",
            ),
        ];
        for (host, port, expected) in cases {
            assert_eq!(
                agent_mcp_base_url(&tcp(host, port)),
                Ok(Some(expected.to_owned())),
                "{host}:{port}"
            );
        }
        for target in [
            ListenTarget::Socket {
                path: "/tmp/paseo.sock".to_owned(),
            },
            ListenTarget::Pipe {
                path: r"\\.\pipe\paseo".to_owned(),
            },
        ] {
            assert_eq!(agent_mcp_base_url(&target), Ok(None));
        }
    }

    #[test]
    fn international_hosts_match_new_url() {
        // Printed by node 22 (ada 2.9.2): new URL("/mcp/agents", `http://${host}:4000`).
        let cases = [
            (
                "b\u{fc}cher.example",
                4000,
                "http://xn--bcher-kva.example:4000/mcp/agents",
            ),
            (
                "\u{42f}\u{41d}\u{414}\u{415}\u{41a}\u{421}.\u{440}\u{444}",
                4000,
                "http://xn--d1acpjx3f.xn--p1ai:4000/mcp/agents",
            ),
            ("fa\u{df}.de", 4000, "http://xn--fa-hia.de:4000/mcp/agents"),
            (
                "\u{ff41}\u{ff42}\u{ff43}.com",
                4000,
                "http://abc.com:4000/mcp/agents",
            ),
            ("a\u{ad}b.com", 4000, "http://ab.com:4000/mcp/agents"),
            ("\u{2603}.net", 4000, "http://xn--n3h.net:4000/mcp/agents"),
            ("EXAMPLE.COM", 4000, "http://example.com:4000/mcp/agents"),
            (
                "xn--bcher-kva.example",
                4000,
                "http://xn--bcher-kva.example:4000/mcp/agents",
            ),
            ("a_b.example", 4000, "http://a_b.example:4000/mcp/agents"),
            (
                "caf\u{e9}.\u{e9}xample",
                4000,
                "http://xn--caf-dma.xn--xample-9ua:4000/mcp/agents",
            ),
            (
                "\u{130}stanbul.test",
                4000,
                "http://xn--istanbul-o0e.test:4000/mcp/agents",
            ),
            (
                "\u{3c3}\u{3c2}.gr",
                4000,
                "http://xn--3xab.gr:4000/mcp/agents",
            ),
            (
                "1\u{ff0e}2\u{ff0e}3\u{ff0e}4",
                4000,
                "http://1.2.3.4:4000/mcp/agents",
            ),
            ("a..b", 4000, "http://a..b:4000/mcp/agents"),
            ("-a.com", 4000, "http://-a.com:4000/mcp/agents"),
            ("a-.com", 4000, "http://a-.com:4000/mcp/agents"),
            (
                "m\u{fc}ller.\u{fc}ber.de",
                4000,
                "http://xn--mller-kva.xn--ber-goa.de:4000/mcp/agents",
            ),
        ];
        for (host, port, expected) in cases {
            assert_eq!(
                agent_mcp_base_url(&tcp(host, port)),
                Ok(Some(expected.to_owned())),
                "{host:?}"
            );
        }
        // `new URL` throws ERR_INVALID_URL for these hosts: a space, an
        // invalid punycode label and a lone zero width joiner.
        for host in ["a b", "xn--a.com", "\u{200d}.com"] {
            assert_eq!(
                agent_mcp_base_url(&tcp(host, 7)),
                Err("Invalid URL".to_owned()),
                "{host:?}"
            );
        }
    }
}
