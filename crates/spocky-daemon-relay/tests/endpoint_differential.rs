//! Differential of `daemon-endpoints.ts` (`parseHostPort`, `normalizeRelayProtocolVersion`,
//! `buildRelayWebSocketUrl`) against the Rust port over a corpus of hosts, ports, server
//! ids, connection ids and versions. Nothing is normalized: every error message and every
//! URL byte compares as raw text.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0); `SPOCKY_ALLOW_SKIP=1` skips explicitly.

mod support;

use serde_json::{Value, json};
use support::{NodeEndpoint, RustEndpoint, differential, pinned};

const HOSTS: &[&str] = &[
    "localhost",
    "LOCALHOST",
    "relay.example.test",
    "RELAY.Example.TEST",
    "127.0.0.1",
    "0x7f.1",
    "0177.0.0.1",
    "1.2.3",
    "256.1.1.1",
    "4294967296",
    "255.255.255.255",
    "0.0.0.0",
    "1e3",
    "bücher.example",
    "xn--bcher-kva.example",
    "faß.de",
    "ÀÉ.test",
    "日本語.jp",
    "xn--",
    "a b",
    "a/b",
    "a?b",
    "a#b",
    "a@b",
    "user:pw@host",
    "%41",
    "a%2fb",
    "host.",
    "..",
    "-",
    "_",
    "a_b",
    "a\u{2028}b",
    "a\nb",
    "\u{a0}host\u{a0}",
    "\u{feff}host",
    "\u{85}host",
    " host ",
    "host\n",
    "ho st",
    "a.b.c.d.e.f",
    "UPPER.case",
    "ex\u{ad}ample.test",
    "ex\u{200b}ample.test",
    "a:b",
    "a:",
    ":",
    "",
    " ",
];

const IPV6: &[&str] = &[
    "::1",
    "::",
    "2001:db8::1",
    "2001:DB8::1",
    "::ffff:1.2.3.4",
    "1:2:3:4:5:6:7:8",
    "fe80::1%eth0",
    "zzz",
    "1::2::3",
    "0:0:0:0:0:0:0:1",
    " ::1 ",
    "::1\n",
    "1:2:3:4:5:6:7:8:9",
    "",
];

const PORTS: &[&str] = &[
    "1", "80", "443", "6767", "65535", "65536", "0", "00080", "000000", "123456", "99999", "٣",
    "+80", "-1", "1.5", "80 ", "", "8_0",
];

const SERVER_IDS: &[&str] = &[
    "s",
    "",
    "a b",
    "é",
    "😀",
    "+&=%",
    "~*-._!'()",
    "a/b?c#d",
    "\u{0}\u{1f}\u{7f}",
    "ünï\u{2028}",
];

const CONNECTION_IDS: &[Option<&str>] = &[None, Some(""), Some("c1"), Some("a b"), Some("é/😀")];

fn versions() -> Vec<Option<Value>> {
    vec![
        None,
        Some(Value::Null),
        Some(json!("1")),
        Some(json!("2")),
        Some(json!(" 2 ")),
        Some(json!("")),
        Some(json!("3")),
        Some(json!("1.0")),
        Some(json!("\u{a0}2\u{feff}")),
        Some(json!("\u{85}2")),
        Some(json!(1)),
        Some(json!(2)),
        Some(json!(1.0)),
        Some(json!(3)),
        Some(json!(0)),
        Some(json!(1.5)),
        Some(json!(true)),
        Some(json!([1])),
        Some(json!({})),
    ]
}

fn build(
    endpoint: &str,
    use_tls: bool,
    server_id: &str,
    role: &str,
    connection_id: Option<&str>,
    version: Option<Value>,
) -> Value {
    let mut op = json!({
        "op": "buildUrl", "endpoint": endpoint, "useTls": use_tls,
        "serverId": server_id, "role": role,
    });
    if let Some(connection_id) = connection_id {
        op["connectionId"] = json!(connection_id);
    }
    if let Some(version) = version {
        op["version"] = version;
    }
    op
}

fn operations() -> Vec<Value> {
    let mut ops = vec![json!({ "op": "constants" })];
    let mut endpoints: Vec<String> = Vec::new();
    for host in HOSTS {
        for port in PORTS {
            endpoints.push(format!("{host}:{port}"));
        }
        endpoints.push((*host).to_owned());
    }
    for host in IPV6 {
        for port in PORTS {
            endpoints.push(format!("[{host}]:{port}"));
        }
        endpoints.push(format!("[{host}]"));
        endpoints.push(format!("[{host}]x:80"));
        endpoints.push(format!("{host}:80"));
    }
    endpoints.extend(
        [
            "[::1",
            "[]:80",
            "[[::1]]:80",
            "[::1]]:80",
            "a:1:2",
            "a:1:123456",
            "a:123:45678",
        ]
        .map(str::to_owned),
    );
    for endpoint in &endpoints {
        ops.push(json!({ "op": "parseHostPort", "input": endpoint }));
    }
    for version in versions() {
        ops.push(json!({ "op": "normalizeVersion", "value": version.unwrap_or(Value::Null) }));
    }
    // URL building: every endpoint with the default arguments, then every argument family
    // against a few fixed endpoints.
    for (index, endpoint) in endpoints.iter().enumerate() {
        ops.push(build(endpoint, index % 2 == 0, "srv", "server", None, None));
    }
    for endpoint in [
        "relay.example.test:443",
        "relay.example.test:80",
        "[::1]:6767",
        "10.0.0.1:80",
    ] {
        for use_tls in [false, true] {
            for server_id in SERVER_IDS {
                for connection_id in CONNECTION_IDS {
                    for role in ["server", "client"] {
                        ops.push(build(
                            endpoint,
                            use_tls,
                            server_id,
                            role,
                            *connection_id,
                            None,
                        ));
                    }
                }
            }
            for version in versions() {
                ops.push(build(endpoint, use_tls, "s", "server", Some("c"), version));
            }
        }
    }
    ops
}

#[test]
fn endpoint_functions_match_the_pinned_typescript() {
    let Some(pinned) = pinned() else { return };
    let mut node = NodeEndpoint::spawn(&pinned);
    let mut rust = RustEndpoint::new();
    let (transcript, mismatches) = differential(&mut node, &mut rust, &operations());
    if let Some(directory) = std::env::var_os("SPOCKY_RELAY_DAEMON_EVIDENCE") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("endpoint.txt"),
            transcript.join("\n"),
        )
        .unwrap();
    }
    assert!(
        mismatches.is_empty(),
        "{} operations differ; first 10:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
