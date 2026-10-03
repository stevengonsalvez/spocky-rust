//! Replays the wire the pinned relay produced over real sockets
//! (`scripts/phase4/relay-protocol-live.exs`, committed as a fixture) through the Rust
//! crate: every HTTP status and body, every control frame as raw text, every close
//! code and reason. Masked: `ts` (`wall_clock`) and the random connection id (`generated_id`).

use spocky_relay_protocol::close::{self, CloseFrame};
use spocky_relay_protocol::connection::from_query;
use spocky_relay_protocol::query::{into_query_map, parse_qs};
use spocky_relay_protocol::{control, rejection};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const WALL_CLOCK: i64 = 4_242_424_242_424;

fn fixture() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/relay-protocol-live-baseline.tsv"),
    )
    .unwrap()
}

fn closed(frame: CloseFrame) -> String {
    format!("close {} {:?}", frame.code, frame.reason)
}

fn text(json: Result<String, control::InvalidUtf8>) -> String {
    format!("text {}", json.unwrap())
}

fn query_map(query: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
    into_query_map(parse_qs(query.as_bytes()).unwrap())
}

/// The id the relay would hold for a v2 client opened with `connectionId=<encoded>`.
fn connection_id(encoded: &str) -> Vec<u8> {
    let query = format!("serverId=live_a&role=client&v=2&connectionId={encoded}");
    from_query(&query_map(&query), || unreachable!("explicit id"))
        .unwrap()
        .connection_id
        .unwrap()
}

fn escape_cases() -> [(&'static str, &'static str); 6] {
    [
        ("quote_backslash", "a%22b%5Cc"),
        ("control_chars", "a%01%0Ab%1Fc"),
        ("unicode", "%C3%A9%E2%82%AC%F0%9F%98%80"),
        ("line_separator", "x%E2%80%A8y"),
        ("slash", "a%2Fb"),
        ("trim_nbsp", "%C2%A0trimmed%C2%A0"),
    ]
}

fn http_cases() -> Vec<(&'static str, String, bool)> {
    let long = "a".repeat(257);
    let keys = (1..=101).fold(String::new(), |mut keys, index| {
        let _ = write!(keys, "k{index}=v&");
        keys
    });
    vec![
        ("no_upgrade", "serverId=s&role=server".into(), false),
        ("no_role", "serverId=s".into(), true),
        ("bad_role", "serverId=s&role=x".into(), true),
        ("no_server_id", "role=server".into(), true),
        ("empty_server_id", "role=server&serverId=".into(), true),
        (
            "long_server_id",
            format!("role=server&serverId={long}"),
            true,
        ),
        ("bad_version", "role=server&serverId=s&v=3".into(), true),
        (
            "long_connection_id",
            format!("role=server&serverId=s&v=2&connectionId={long}"),
            true,
        ),
        ("bad_percent", "role=server&serverId=%zz".into(), true),
        ("empty_name", "=x&role=server&serverId=s".into(), true),
        (
            "too_many_keys",
            format!("{keys}role=server&serverId=s"),
            true,
        ),
    ]
}

fn http_answer(query: &str, upgrade: bool) -> rejection::Rejection {
    if !upgrade {
        return rejection::EXPECTED_WEBSOCKET_UPGRADE;
    }
    match parse_qs(query.as_bytes()) {
        Err(_) => rejection::MALFORMED_QUERY,
        Ok(pairs) => match from_query(&into_query_map(pairs), || [0; 8]) {
            Err(message) => rejection::invalid_connection(message),
            Ok(_) => panic!("{query} was expected to be rejected"),
        },
    }
}

fn expected(label: &str) -> String {
    let client_a = b"clt_a".as_slice();
    match label {
        "control_first" => text(control::sync(&[])),
        "client_connected" => text(control::connected(client_a)),
        "pong" => {
            text(Ok(control::pong(WALL_CLOCK))).replace(&WALL_CLOCK.to_string(), "<wall_clock>")
        }
        "buffered_to_data" => "text before-data".to_owned(),
        "generated_connected" => text(control::connected(b"conn_0123456789abcdef"))
            .replace("conn_0123456789abcdef", "<generated_id>"),
        "control_replaced_old" | "data_replaced" | "v1_replaced" => {
            closed(close::REPLACED_BY_NEW_CONNECTION)
        }
        "control_replaced_new_sync" => text(control::sync(&[client_a, b"conn_0123456789abcdef"]))
            .replace("conn_0123456789abcdef", "<generated_id>"),
        "sync_40" => {
            let ids: Vec<Vec<u8>> = (1..=40)
                .map(|index| format!("conn_{index}").into_bytes())
                .collect();
            let borrowed: Vec<&[u8]> = ids.iter().map(Vec::as_slice).collect();
            text(control::sync(&borrowed))
        }
        "client_left_control" => text(control::disconnected(client_a)),
        "client_left_data" => closed(close::CLIENT_DISCONNECTED),
        "client_b_connected" => text(control::connected(b"clt_b")),
        "server_disconnected" => closed(close::SERVER_DISCONNECTED),
        "invalid_utf8_control" => closed(close::SESSION_OWNER_MOVED),
        "invalid_utf8_client" => closed(close::SESSION_EXPIRED),
        "invalid_utf8_control_after" => ":none".to_owned(),
        "handshake_valid_forwarded" => {
            r#"text {"type":"hello","key":"<key>","capabilities":{}}"#.to_owned()
        }
        "handshake_invalid_close" => closed(close::INVALID_HANDSHAKE_KEY),
        "control_oversize" => closed(close::MESSAGE_TOO_LARGE),
        other => panic!("no expectation for {other}"),
    }
}

#[test]
fn http_rejections_match_the_pinned_relay() {
    let fixture = fixture();
    for (label, query, upgrade) in http_cases() {
        let line = fixture
            .lines()
            .find(|line| line.starts_with(&format!("http\t{label}\t")))
            .unwrap_or_else(|| panic!("fixture lacks {label}"));
        let answer = http_answer(&query, upgrade);
        assert!(
            line.contains(&format!(" {} ", answer.status)),
            "{label}: status {} not in {line}",
            answer.status
        );
        assert!(
            line.ends_with(&format!("body={:?}", answer.body)),
            "{label}: body {:?} not in {line}",
            answer.body
        );
    }
}

#[test]
fn control_frames_and_closes_match_the_pinned_relay_raw() {
    let fixture = fixture();
    let mut checked = 0;
    for line in fixture.lines().filter(|line| !line.starts_with("http\t")) {
        let (label, observed) = line.split_once('\t').unwrap();
        if label.starts_with("escape_") || label == "control_late" {
            continue;
        }
        assert_eq!(observed, expected(label), "{label}");
        checked += 1;
    }
    assert_eq!(checked, 20);
}

#[test]
fn escaped_identifiers_encode_like_jason() {
    let fixture = fixture();
    for (name, encoded) in escape_cases() {
        let id = connection_id(encoded);
        for (suffix, frame) in [
            ("", control::connected(&id)),
            ("_left", control::disconnected(&id)),
        ] {
            let line = fixture
                .lines()
                .find(|line| line.starts_with(&format!("escape_{name}{suffix}\t")))
                .unwrap_or_else(|| panic!("fixture lacks escape_{name}{suffix}"));
            assert_eq!(
                line.split_once('\t').unwrap().1,
                text(frame),
                "{name}{suffix}"
            );
        }
    }
}

#[test]
fn late_control_frames_are_disconnect_notices() {
    let fixture = fixture();
    let late: Vec<&str> = fixture
        .lines()
        .filter_map(|line| line.strip_prefix("control_late\t"))
        .collect();
    assert_eq!(
        late,
        [
            text(control::disconnected(b"conn_0123456789abcdef"))
                .replace("conn_0123456789abcdef", "<generated_id>"),
            text(control::disconnected(b"clt_b")),
        ]
    );
}

#[test]
fn an_identifier_jason_cannot_encode_has_no_frame() {
    assert_eq!(control::connected(b"\xff"), Err(control::InvalidUtf8));
    assert_eq!(control::disconnected(b"\xff"), Err(control::InvalidUtf8));
    assert_eq!(control::sync(&[b"ok", b"\xff"]), Err(control::InvalidUtf8));
}

#[test]
fn every_close_the_relay_sent_is_in_the_table() {
    let fixture = fixture();
    for line in fixture.lines() {
        let Some((_, observed)) = line.split_once('\t') else {
            continue;
        };
        if observed.starts_with("close ") {
            assert!(
                close::ALL.iter().any(|frame| closed(*frame) == observed),
                "{observed} missing from close::ALL"
            );
        }
    }
}
