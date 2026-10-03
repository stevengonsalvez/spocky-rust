//! Replays the wire the pinned relay produced over real sockets
//! (`scripts/phase4/relay-protocol-live.exs`, committed as fixtures) through the Rust
//! crate: every HTTP status and body, every control frame as raw text, every close code and
//! reason, and the order of `sync` ids for every id length class. Masked: `ts`
//! (`wall_clock`) and a generated v2 connection id (`generated_id`), nothing else.
//!
//! `SPOCKY_RELAY_LIVE_FIXTURE` and `SPOCKY_RELAY_GENERATED_FIXTURE` point the test at a
//! fresh capture instead of the committed one; the differential script does that.

use spocky_relay_protocol::close::{self, CloseFrame};
use spocky_relay_protocol::connection::from_query;
use spocky_relay_protocol::handshake::{Handshake, HandshakeType, check};
use spocky_relay_protocol::query::{into_query_map, parse_qs};
use spocky_relay_protocol::{control, rejection};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const WALL_CLOCK: i64 = 4_242_424_242_424;
const GENERATED: &str = "conn_0123456789abcdef";

fn fixture_path(variable: &str, name: &str) -> PathBuf {
    std::env::var_os(variable).map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name)
        },
        PathBuf::from,
    )
}

fn fixture() -> String {
    fs::read_to_string(fixture_path(
        "SPOCKY_RELAY_LIVE_FIXTURE",
        "relay-protocol-live-wire.tsv",
    ))
    .unwrap()
}

fn fixture_line<'a>(fixture: &'a str, label: &str) -> &'a str {
    fixture
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{label}\t")))
        .unwrap_or_else(|| panic!("fixture lacks {label}"))
}

fn closed(frame: CloseFrame) -> String {
    format!("close {} {:?}", frame.code, frame.reason)
}

fn text(json: Result<String, control::InvalidUtf8>) -> String {
    format!("text {}", json.unwrap())
}

fn masked(rendered: &str) -> String {
    rendered.replace(GENERATED, "<generated_id>")
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

/// The key the live script sends: 32 bytes of 7, canonical and supported.
fn hello_key() -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in [7_u8; 32].chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for position in 0..4 {
            if position <= chunk.len() {
                let index = usize::try_from((triple >> (18 - 6 * position)) & 0x3f).unwrap();
                encoded.push(char::from(alphabet[index]));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn hex16(value: u64) -> String {
    format!("{value:016x}")
}

fn padded(text: &str, length: usize, fill: char) -> String {
    let mut padded = text.to_owned();
    while padded.chars().count() < length {
        padded.push(fill);
    }
    padded
}

/// The ids each `sync_*` case of the live script connects, in connection order.
fn sync_ids(label: &str) -> Vec<String> {
    match label {
        "sync_40" => (1..=40).map(|index| format!("conn_{index}")).collect(),
        "sync_conn21" => (1..=40)
            .map(|index| format!("conn_{}", hex16(index * 7919)))
            .collect(),
        "sync_id16" => (1..=40)
            .map(|index| padded(&format!("i{index}"), 16, 'x'))
            .collect(),
        "sync_id32" => (1..=40)
            .map(|index| padded(&format!("j{index}"), 32, 'x'))
            .collect(),
        "sync_id255" => (1..=34)
            .map(|index| padded(&format!("k{index}"), 255, 'y'))
            .collect(),
        "sync_tails" => [12, 13, 14, 15, 17, 24, 28, 31, 32, 33, 48, 64]
            .into_iter()
            .flat_map(|length| {
                (1..=3).map(move |index| padded(&format!("t{length}_{index}"), length, 'z'))
            })
            .collect(),
        "sync_nonascii" => (1..=40).map(|index| format!("é€😀{index}")).collect(),
        "sync_33" => (1..=33)
            .map(|index| format!("conn_{}", hex16(index * 104_729)))
            .collect(),
        other => panic!("no ids for {other}"),
    }
}

fn sync_text(ids: &[String]) -> String {
    let borrowed: Vec<&[u8]> = ids.iter().map(String::as_bytes).collect();
    text(control::sync(&borrowed))
}

fn expected(label: &str, observed: &str) -> String {
    let client_a = b"clt_a".as_slice();
    match label {
        "control_first" => text(control::sync(&[])),
        "client_connected" => text(control::connected(client_a)),
        "pong" => {
            text(Ok(control::pong(WALL_CLOCK))).replace(&WALL_CLOCK.to_string(), "<wall_clock>")
        }
        "buffered_to_data" => "text before-data".to_owned(),
        "generated_connected" => masked(&text(control::connected(GENERATED.as_bytes()))),
        "control_replaced_old" | "data_replaced" | "v1_replaced" => {
            closed(close::REPLACED_BY_NEW_CONNECTION)
        }
        "control_replaced_new_sync" => {
            masked(&text(control::sync(&[client_a, GENERATED.as_bytes()])))
        }
        "client_left_control" => text(control::disconnected(client_a)),
        "client_left_data" => closed(close::CLIENT_DISCONNECTED),
        "client_b_connected" => text(control::connected(b"clt_b")),
        "server_disconnected" => closed(close::SERVER_DISCONNECTED),
        "invalid_utf8_control" => closed(close::SESSION_OWNER_MOVED),
        "invalid_utf8_client" => closed(close::SESSION_EXPIRED),
        "invalid_utf8_control_after" => ":none".to_owned(),
        // The relay forwards an accepted handshake untouched: the observed frame is the
        // payload the client sent, and the crate must accept it.
        "handshake_valid_forwarded" => {
            let payload = observed.strip_prefix("text ").expect("a text frame");
            assert_eq!(
                check(payload.as_bytes()),
                Handshake::Accept(HandshakeType::Hello)
            );
            assert!(payload.contains(&hello_key()));
            format!("text {payload}")
        }
        "handshake_invalid_close" => {
            let zero_key = "A".repeat(43) + "=";
            let payload = format!(r#"{{"type":"hello","key":"{zero_key}","capabilities":{{}}}}"#);
            assert_eq!(
                check(payload.as_bytes()),
                Handshake::Reject(HandshakeType::Hello)
            );
            closed(close::INVALID_HANDSHAKE_KEY)
        }
        "control_oversize" => closed(close::MESSAGE_TOO_LARGE),
        other => panic!("no expectation for {other}"),
    }
}

#[test]
fn http_rejections_match_the_pinned_relay() {
    let fixture = fixture();
    for (label, query, upgrade) in http_cases() {
        let line = fixture_line(&fixture, &format!("http\t{label}"));
        let answer = rejection::classify(upgrade, query.as_bytes(), || [0; 8])
            .expect_err("every case is rejected");
        assert!(
            line.starts_with(&format!("HTTP/1.1 {} ", answer.status)),
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
        if label.starts_with("escape_") || label.starts_with("sync_") || label == "control_late" {
            continue;
        }
        assert_eq!(observed, expected(label, observed), "{label}");
        checked += 1;
    }
    assert_eq!(checked, 19);
}

#[test]
fn sync_frames_list_ids_in_the_pinned_relay_order_for_every_id_class() {
    let fixture = fixture();
    for label in [
        "sync_40",
        "sync_conn21",
        "sync_id16",
        "sync_id32",
        "sync_id255",
        "sync_tails",
        "sync_nonascii",
        "sync_33",
    ] {
        let ids = sync_ids(label);
        assert!(ids.len() > 32, "{label} must exceed the flat map limit");
        assert_eq!(fixture_line(&fixture, label), sync_text(&ids), "{label}");
    }
}

#[test]
fn a_map_that_shrinks_back_to_32_keys_lists_ids_sorted_again() {
    let fixture = fixture();
    let ids = sync_ids("sync_33");
    // The live script disconnects the first client, then the second.
    let after_one = &ids[1..];
    let after_two = &ids[2..];
    assert_eq!(after_one.len(), 32);
    assert_eq!(
        fixture_line(&fixture, "sync_32_after_disconnect"),
        sync_text(after_one)
    );
    assert_eq!(
        fixture_line(&fixture, "sync_31_after_disconnect"),
        sync_text(after_two)
    );
    let mut sorted = after_one.to_vec();
    sorted.sort();
    assert_eq!(
        sync_text(after_one),
        sync_text(&sorted),
        "a flat map lists keys sorted"
    );
}

#[test]
fn generated_connection_ids_have_the_relay_shape_and_sync_in_its_order() {
    let generated = fs::read_to_string(fixture_path(
        "SPOCKY_RELAY_GENERATED_FIXTURE",
        "relay-protocol-live-generated.tsv",
    ))
    .unwrap();
    let mut ids: Vec<String> = Vec::new();
    for line in generated.lines() {
        let Some(frame) = line.strip_prefix("connected\t") else {
            continue;
        };
        let id = frame
            .strip_prefix(r#"{"type":"connected","connectionId":""#)
            .and_then(|rest| rest.strip_suffix(r#""}"#))
            .unwrap_or_else(|| panic!("unexpected connected frame {frame}"));
        assert_eq!(id.len(), 21, "{id}");
        assert!(id.starts_with("conn_"), "{id}");
        assert!(
            id[5..]
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
            "{id}"
        );
        assert_eq!(frame, control::connected(id.as_bytes()).unwrap());
        ids.push(id.to_owned());
    }
    assert_eq!(ids.len(), 40);
    let sync = generated
        .lines()
        .find_map(|line| line.strip_prefix("sync\t"))
        .expect("a sync frame");
    let borrowed: Vec<&[u8]> = ids.iter().map(String::as_bytes).collect();
    assert_eq!(sync, control::sync(&borrowed).unwrap());
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
            assert_eq!(
                fixture_line(&fixture, &format!("escape_{name}{suffix}")),
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
            masked(&text(control::disconnected(GENERATED.as_bytes()))),
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

#[test]
fn percent_encoded_route_parameters_decode_before_validation() {
    let percent = fs::read_to_string(fixture_path(
        "SPOCKY_RELAY_PERCENT_FIXTURE",
        "relay-protocol-live-percent.tsv",
    ))
    .unwrap();
    let mut lines = percent.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("control\t{}", control::sync(&[]).unwrap())
    );
    let mut checked = 0;
    for line in lines {
        let rest = line.strip_prefix("query\t").expect("a query line");
        let (query, observed) = rest.split_once('\t').unwrap();
        let connection = from_query(&query_map(query), || unreachable!("explicit id")).unwrap();
        let id = connection.connection_id.unwrap();
        assert_eq!(observed, text(control::connected(&id)), "{query}");
        checked += 1;
    }
    assert_eq!(checked, 8);
}
