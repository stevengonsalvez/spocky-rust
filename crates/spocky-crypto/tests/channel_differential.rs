//! Differential check of the encrypted channel against the pinned Paseo
//! relay source (`packages/relay/src/encrypted-channel.ts` at 5de45e2).
//!
//! Each scenario runs the same operations on the original TypeScript
//! channel under node 22.20.0 and on the Rust port, and asserts identical
//! entries after every operation: every transport frame with its exact
//! bytes, every close code and reason, every event, error text, handshake
//! outcome, and send result. Pair scenarios connect a client and a daemon
//! in all four combinations (TypeScript or Rust on each side) and require
//! one identical transcript.
//!
//! Nothing is normalized. The only random inputs, the client key pair and
//! the nonces, come from one seeded xorshift32 byte stream injected through
//! `nacl.setPRNG` on the TypeScript side and the channel random source on
//! the Rust side.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0). Without it the tests FAIL;
//! `SPOCKY_ALLOW_SKIP=1` (exactly) skips them explicitly outside the gate.
//! `SPOCKY_E2EE_EVIDENCE` names a directory that receives both raw
//! transcripts of every scenario.

mod support;

use serde_json::{Value, json};
use spocky_crypto::{
    base64_js::array_buffer_to_base64, derive_shared_key, encrypt_with_nonce, export_public_key,
    key_pair_from_secret,
};
use support::{Side, XorShift, differential, hex, pair, pinned, quote};

const DAEMON_SECRET: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DAEMON_PUBLIC: &str = "j0DFrbaPJWJK5bIU6nZ6bslNgp09e14a0bpvPiE4KF8=";
const PEER_SECRET: [u8; 32] = [
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
];
const PEER_PUBLIC: &str = "NYBy1jZYgNGu6jKa35EhODhR7SGijjt16WXQ0s0WYlQ=";
const OTHER_PUBLIC: &str = "lEteBny6Zc3JnI9tvY4XYpQ7pal8uHBS99FUCisHPVU=";
const LOW_ORDER_KEY: &str = "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const READY_BINARY: &str = r#"{"type":"e2ee_ready","capabilities":{"binaryCiphertext":true}}"#;
const READY_LEGACY: &str = r#"{"type":"e2ee_ready"}"#;
const CLIENT_SEED: u64 = 0x1234_5678;
const DAEMON_SEED: u64 = 0x0bad_cafe;

fn daemon_secret() -> [u8; 32] {
    DAEMON_SECRET
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

/// The key a daemon shares with the fixed peer key pair.
fn peer_shared() -> [u8; 32] {
    let daemon = key_pair_from_secret(daemon_secret());
    derive_shared_key(&PEER_SECRET, &daemon.public_key).unwrap()
}

/// The key the seeded client shares with the daemon: the client secret is
/// the first 32 bytes of its random stream.
fn client_shared() -> [u8; 32] {
    let mut secret = [0_u8; 32];
    rand_core::RngCore::fill_bytes(
        &mut XorShift(u32::try_from(CLIENT_SEED).unwrap()),
        &mut secret,
    );
    let daemon = key_pair_from_secret(daemon_secret());
    derive_shared_key(&secret, &daemon.public_key).unwrap()
}

fn seal(shared: &[u8; 32], nonce: u8, plaintext: &[u8]) -> Vec<u8> {
    encrypt_with_nonce(shared, &[nonce; 24], plaintext).unwrap()
}

fn hello(key: &str, capabilities: &str) -> String {
    format!(
        r#"{{"type":"e2ee_hello","key":{}{capabilities}}}"#,
        quote(key)
    )
}

fn binary_capability() -> &'static str {
    r#","capabilities":{"binaryCiphertext":true}"#
}

fn client() -> Value {
    json!({ "op": "client", "daemonKey": DAEMON_PUBLIC, "seed": CLIENT_SEED })
}

fn daemon() -> Value {
    json!({ "op": "daemon", "secret": DAEMON_SECRET, "seed": DAEMON_SEED })
}

fn deliver_text(text: &str) -> Value {
    json!({ "op": "deliver", "text": text })
}

fn frame_base64(bytes: &[u8]) -> Value {
    frame_text(&array_buffer_to_base64(bytes))
}

fn deliver_binary(bytes: &[u8]) -> Value {
    json!({ "op": "deliver", "binary": hex(bytes) })
}

fn deliver_base64(bytes: &[u8]) -> Value {
    deliver_text(&array_buffer_to_base64(bytes))
}

fn send_text(text: &str) -> Value {
    json!({ "op": "send", "text": text })
}

fn send_binary(bytes: &[u8]) -> Value {
    json!({ "op": "send", "binary": hex(bytes) })
}

fn mode(kind: &str, message: &str) -> Value {
    json!({ "op": "mode", "kind": kind, "message": message })
}

fn settle(id: u64, error: Option<&str>) -> Value {
    json!({ "op": "settle", "id": id, "error": error })
}

fn count(transcript: &[Vec<String>], prefix: &str) -> usize {
    transcript
        .iter()
        .flatten()
        .filter(|entry| entry.starts_with(prefix))
        .count()
}

fn has(transcript: &[Vec<String>], entry: &str) -> bool {
    transcript.iter().flatten().any(|actual| actual == entry)
}

#[test]
fn client_handshake_negotiates_binary_and_flushes_the_backlog() {
    let shared = client_shared();
    let Some(transcript) = differential(
        "client-handshake-binary",
        &[
            client(),
            // Handshake traffic other than e2ee_ready is ignored, including
            // ciphertext that arrives before ready.
            deliver_binary(READY_BINARY.as_bytes()),
            deliver_text("not json"),
            deliver_base64(&seal(&shared, 1, b"early")),
            deliver_text(&hello(PEER_PUBLIC, "")),
            json!({ "op": "is-open" }),
            send_text("queued text"),
            send_binary(&[0, 1, 2, 0xff]),
            json!({ "op": "wire-length", "text": "queued text" }),
            deliver_text(READY_BINARY),
            json!({ "op": "is-open" }),
            json!({ "op": "tick" }),
            json!({ "op": "tick" }),
            send_text("héllo"),
            send_binary(b"ASCII terminal output"),
            json!({ "op": "wire-length", "binary": "00010203" }),
            json!({ "op": "wire-length", "text": "é" }),
            deliver_base64(&seal(&shared, 2, "text ✓".as_bytes())),
            deliver_binary(&seal(&shared, 3, &[0xde, 0xad])),
            // Stray handshake frames on an open client are ignored.
            deliver_text(READY_LEGACY),
            deliver_text(&hello(PEER_PUBLIC, "")),
        ],
    ) else {
        return;
    };
    assert_eq!(count(&transcript, r#"{"t":"open"}"#), 1);
    assert_eq!(count(&transcript, r#"{"t":"wire","#), 5);
    assert!(has(&transcript, r#"{"t":"message","text":"text ✓"}"#));
    assert!(has(&transcript, r#"{"t":"message","binary":"dead"}"#));
}

#[test]
fn client_with_legacy_daemon_stays_base64_only() {
    let shared = client_shared();
    let Some(transcript) = differential(
        "client-legacy-daemon",
        &[
            client(),
            deliver_text(READY_LEGACY),
            send_binary(b"legacy binary"),
            json!({ "op": "wire-length", "binary": "00010203" }),
            // Legacy receive mode guesses the plaintext kind from its bytes.
            deliver_base64(&seal(&shared, 4, b"utf8 text")),
            deliver_base64(&seal(&shared, 5, &[0xff, 0xfe])),
            deliver_base64(&seal(&shared, 6, b"\xef\xbb\xbfbom")),
            // A binary-opcode frame decodes as base64 first.
            json!({ "op": "deliver", "binary": hex(array_buffer_to_base64(&seal(&shared, 7, b"opcode lost")).as_bytes()), "isBinary": true }),
            deliver_binary(&seal(&shared, 8, b"raw bytes")),
        ],
    ) else {
        return;
    };
    assert!(has(&transcript, r#"{"t":"message","binary":"fffe"}"#));
    assert!(has(&transcript, r#"{"t":"message","text":"bom"}"#));
    assert!(has(&transcript, r#"{"t":"message","text":"opcode lost"}"#));
    // Raw ciphertext on a binary opcode still decodes as base64 first,
    // which base64-js never rejects, so the legacy path cannot open it.
    assert!(!has(&transcript, r#"{"t":"message","text":"raw bytes"}"#));
    assert!(has(
        &transcript,
        r#"{"t":"transport-close","code":1011,"reason":"Decryption failed"}"#
    ));
}

#[test]
fn client_hello_retry_follows_the_interval_rules() {
    let Some(transcript) = differential(
        "client-hello-retry",
        &[
            client(),
            json!({ "op": "tick" }),
            json!({ "op": "tick" }),
            deliver_text(READY_LEGACY),
            json!({ "op": "tick" }),
            json!({ "op": "transport-close", "code": 1006, "reason": "gone" }),
            json!({ "op": "tick" }),
            json!({ "op": "is-open" }),
            send_text("after close"),
        ],
    ) else {
        return;
    };
    assert_eq!(count(&transcript, r#"{"t":"wire","#), 3);
    assert!(has(
        &transcript,
        r#"{"t":"sent","handle":1,"error":"Channel not open"}"#
    ));
}

#[test]
fn client_close_keeps_retrying_hello_until_the_transport_closes() {
    let Some(transcript) = differential(
        "client-close-retry",
        &[
            client(),
            json!({ "op": "close" }),
            json!({ "op": "tick" }),
            json!({ "op": "tick" }),
            json!({ "op": "transport-close", "code": 1000, "reason": "Normal closure" }),
            json!({ "op": "tick" }),
        ],
    ) else {
        return;
    };
    assert_eq!(count(&transcript, r#"{"t":"wire","#), 3);
}

#[test]
fn client_retry_stops_once_set_state_opens_the_channel() {
    let Some(transcript) = differential(
        "client-set-state-retry",
        &[
            client(),
            json!({ "op": "set-state", "state": "connecting" }),
            deliver_text(READY_LEGACY),
            json!({ "op": "tick" }),
            json!({ "op": "set-state", "state": "open" }),
            json!({ "op": "tick" }),
            json!({ "op": "set-state", "state": "handshaking" }),
            json!({ "op": "tick" }),
            send_text("queued again"),
            json!({ "op": "is-open" }),
        ],
    ) else {
        return;
    };
    assert_eq!(count(&transcript, r#"{"t":"wire","#), 2);
}

#[test]
fn client_hello_send_failures_are_reported_not_thrown() {
    for kind in ["throw", "reject"] {
        let Some(transcript) = differential(
            &format!("client-hello-{kind}"),
            &[
                mode(kind, "WebSocket not open (readyState=2)"),
                client(),
                json!({ "op": "tick" }),
                json!({ "op": "transport-close", "code": 1000, "reason": "closed" }),
                json!({ "op": "tick" }),
            ],
        ) else {
            return;
        };
        assert_eq!(
            count(
                &transcript,
                r#"{"t":"error","message":"WebSocket not open (readyState=2)"}"#
            ),
            2
        );
    }
    differential(
        "client-hello-pending",
        &[
            mode("pending", ""),
            client(),
            json!({ "op": "tick" }),
            settle(2, None),
            settle(1, Some("hello send failed")),
        ],
    );
}

#[test]
fn client_rejects_malformed_daemon_keys() {
    for (name, key) in [
        ("encoding", "!!!!"),
        (
            "noncanonical",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
        ),
        ("length", "AAAA"),
        ("low-order", LOW_ORDER_KEY),
    ] {
        let Some(transcript) = differential(
            &format!("client-key-{name}"),
            &[json!({ "op": "client", "daemonKey": key, "seed": CLIENT_SEED })],
        ) else {
            return;
        };
        assert_eq!(count(&transcript, r#"{"t":"created","error":"#), 1);
    }
}

#[test]
fn handshake_backlog_keeps_only_the_newest_200_sends() {
    let mut ops = vec![client()];
    ops.extend((0..205).map(|index| send_text(&format!("queued {index}"))));
    ops.push(deliver_text(READY_LEGACY));
    let Some(transcript) = differential("client-backlog-200", &ops) else {
        return;
    };
    let flushed = transcript.last().unwrap();
    assert_eq!(flushed.len(), 201, "open plus 200 flushed frames");
    assert_eq!(count(&transcript, r#"{"t":"sent","#), 205);
}

#[test]
fn backlog_flush_failure_reports_and_closes() {
    let Some(transcript) = differential(
        "client-backlog-failure",
        &[
            client(),
            send_text("first"),
            send_text("second"),
            send_text("third"),
            json!({ "op": "queue", "modes": [
                { "kind": "sync" },
                { "kind": "reject", "message": "backlog send failed" },
            ]}),
            deliver_text(READY_BINARY),
            json!({ "op": "is-open" }),
            send_text("after failure"),
        ],
    ) else {
        return;
    };
    assert!(has(
        &transcript,
        r#"{"t":"error","message":"backlog send failed"}"#
    ));
    assert!(has(
        &transcript,
        r#"{"t":"transport-close","code":1011,"reason":"backlog send failed"}"#
    ));
}

#[test]
fn pending_backlog_flush_interleaves_with_live_sends() {
    differential(
        "client-backlog-pending",
        &[
            client(),
            send_text("p1"),
            send_text("p2"),
            mode("pending", ""),
            deliver_text(READY_BINARY),
            send_binary(&[9, 9]),
            settle(2, None),
            settle(4, Some("live send failed")),
            settle(3, None),
        ],
    );
    let Some(transcript) = differential(
        "client-backlog-closed-mid-flush",
        &[
            client(),
            send_text("p1"),
            send_text("p2"),
            mode("pending", ""),
            deliver_text(READY_LEGACY),
            json!({ "op": "transport-close", "code": 1006, "reason": "dropped" }),
            settle(2, None),
        ],
    ) else {
        return;
    };
    assert!(has(
        &transcript,
        r#"{"t":"transport-close","code":1011,"reason":"Channel not open"}"#
    ));
}

#[test]
fn reentrant_send_in_onopen_precedes_the_backlog() {
    let Some(transcript) = differential(
        "client-reentrant-onopen",
        &[
            json!({ "op": "on-open-send", "text": "from onopen" }),
            client(),
            send_text("backlog"),
            deliver_text(READY_LEGACY),
        ],
    ) else {
        return;
    };
    assert_eq!(transcript.last().unwrap().len(), 3);
}

#[test]
fn daemon_handshake_answers_with_matching_capabilities() {
    let shared = peer_shared();
    differential(
        "daemon-handshake-binary",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, binary_capability())),
            deliver_binary(&seal(&shared, 1, b"binary in")),
            deliver_base64(&seal(&shared, 2, b"text in")),
            send_binary(&[1, 2, 3]),
            send_text("text out"),
            json!({ "op": "wire-length", "binary": "010203" }),
        ],
    );
    differential(
        "daemon-handshake-legacy",
        &[
            daemon(),
            deliver_text(&format!(" \n{}\t", hello(PEER_PUBLIC, ""))),
            deliver_base64(&seal(&shared, 3, b"pipelined legacy text")),
            send_binary(&[1, 2, 3]),
        ],
    );
}

#[test]
fn daemon_rejects_invalid_hellos_with_exact_errors() {
    let long = format!(
        "{{\"type\":\"other\",\n\n\"pad\":\"{}\"}}",
        "x ".repeat(120)
    );
    let cases = [
        ("invalid-type", deliver_text(r#"{"type":"invalid"}"#)),
        ("not-json", deliver_text("hello there")),
        ("binary", deliver_binary(hello(PEER_PUBLIC, "").as_bytes())),
        ("long", deliver_text(&long)),
        ("number-type", deliver_text(r#"{"type":7,"key":"k"}"#)),
        ("array", deliver_text(r#"["e2ee_hello"]"#)),
        (
            "null-capabilities",
            deliver_text(&hello(PEER_PUBLIC, r#","capabilities":null"#)),
        ),
        (
            "blank-key",
            deliver_text(r#"{"type":"e2ee_hello","key":" \t "}"#),
        ),
        (
            "bad-capability",
            deliver_text(&hello(
                PEER_PUBLIC,
                r#","capabilities":{"binaryCiphertext":"yes"}"#,
            )),
        ),
        (
            "lone-surrogate",
            deliver_text(r#"{"type":"\ud800x","key":"k"}"#),
        ),
        ("nested-type", deliver_text(r#"{"type":{"a":[1]},"key":2}"#)),
        (
            "lossy-bytes",
            json!({ "op": "deliver", "binary": "7bed a080c080f4908080e28241".replace(' ', ""), "isBinary": false }),
        ),
        (
            "bom-text",
            json!({ "op": "deliver", "binary": hex(format!("\u{feff}{}", hello("", "")).as_bytes()), "isBinary": false }),
        ),
        ("key-encoding", deliver_text(&hello("!!!!", ""))),
        ("key-length", deliver_text(&hello("AAAA", ""))),
        ("key-low-order", deliver_text(&hello(LOW_ORDER_KEY, ""))),
    ];
    for (name, frame) in cases {
        let Some(transcript) = differential(
            &format!("daemon-invalid-{name}"),
            &[
                daemon(),
                frame,
                // A later valid frame is either a fresh hello attempt or,
                // once the key checks ran, buffered forever.
                deliver_base64(&seal(&peer_shared(), 9, b"after failure")),
            ],
        ) else {
            return;
        };
        assert_eq!(
            count(&transcript, r#"{"t":"handshake","error":"#),
            1,
            "{name}"
        );
    }
}

#[test]
fn daemon_retry_after_an_invalid_hello_opens_without_resolving() {
    let Some(transcript) = differential(
        "daemon-invalid-then-valid",
        &[
            daemon(),
            deliver_text(r#"{"type":"invalid"}"#),
            deliver_text(&hello(PEER_PUBLIC, "")),
            deliver_base64(&seal(&peer_shared(), 1, b"delivered anyway")),
        ],
    ) else {
        return;
    };
    assert!(has(&transcript, r#"{"t":"open"}"#));
    assert_eq!(count(&transcript, r#"{"t":"handshake","#), 1);
}

#[test]
fn daemon_ready_send_failures_reject_the_handshake() {
    for kind in ["throw", "reject"] {
        differential(
            &format!("daemon-ready-{kind}"),
            &[
                mode(kind, "ready send failed"),
                daemon(),
                deliver_text(&hello(PEER_PUBLIC, "")),
                deliver_base64(&seal(&peer_shared(), 1, b"buffered forever")),
            ],
        );
    }
}

#[test]
fn daemon_buffers_while_ready_is_pending_and_replays_in_order() {
    let shared = peer_shared();
    let Some(transcript) = differential(
        "daemon-ready-pending",
        &[
            mode("pending", ""),
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, binary_capability())),
            deliver_binary(&seal(&shared, 1, b"first")),
            deliver_text(&hello(OTHER_PUBLIC, "")),
            deliver_text(READY_LEGACY),
            deliver_text(r#"{"type":"app","plain":true}"#),
            deliver_base64(&seal(&shared, 2, b"second")),
            deliver_binary(&seal(&shared, 1, b"first")),
            mode("sync", ""),
            settle(1, None),
            deliver_base64(&seal(&shared, 3, b"live")),
        ],
    ) else {
        return;
    };
    let replay = &transcript[10];
    assert_eq!(replay[0], r#"{"t":"open"}"#);
    assert!(
        replay
            .iter()
            .any(|entry| entry.contains("Received plaintext frame"))
    );
    assert_eq!(replay.last().unwrap(), r#"{"t":"handshake","ok":true}"#);
}

#[test]
fn daemon_close_or_error_during_handshake_rejects_first() {
    differential(
        "daemon-close-while-ready-pending",
        &[
            mode("pending", ""),
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            deliver_base64(&seal(&peer_shared(), 1, b"buffered")),
            json!({ "op": "transport-close", "code": 1006, "reason": "relay gone" }),
            settle(1, None),
        ],
    );
    differential(
        "daemon-ready-pending-rejects",
        &[
            mode("pending", ""),
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            settle(1, Some("socket closed before ready")),
            deliver_text(&hello(PEER_PUBLIC, "")),
        ],
    );
    differential(
        "daemon-error-before-hello",
        &[
            daemon(),
            json!({ "op": "transport-error", "message": "socket error" }),
            json!({ "op": "transport-close", "code": 1006, "reason": "" }),
        ],
    );
}

#[test]
fn daemon_rehello_reuses_or_rejects_the_session_key() {
    let shared = peer_shared();
    let Some(transcript) = differential(
        "daemon-rehello",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, binary_capability())),
            deliver_text(&hello(PEER_PUBLIC, "")),
            deliver_binary(&seal(&shared, 1, b"still the same key")),
            deliver_text(&hello(OTHER_PUBLIC, "")),
            json!({ "op": "is-open" }),
            deliver_binary(&seal(&shared, 2, b"ignored once closed")),
        ],
    ) else {
        return;
    };
    assert!(has(
        &transcript,
        r#"{"t":"transport-close","code":1008,"reason":"E2EE re-handshake key mismatch"}"#
    ));
    assert!(!has(
        &transcript,
        r#"{"t":"message","binary":"69676e6f726564206f6e636520636c6f736564"}"#
    ));
}

#[test]
fn daemon_rehello_failures_fall_through_to_ciphertext_decoding() {
    let shared = peer_shared();
    for (name, frame) in [
        ("encoding", hello("!!!!", "")),
        ("length", hello("AAAA", "")),
        ("low-order", hello(LOW_ORDER_KEY, "")),
    ] {
        differential(
            &format!("daemon-rehello-{name}"),
            &[
                daemon(),
                deliver_text(&hello(PEER_PUBLIC, "")),
                deliver_text(&frame),
                deliver_base64(&seal(&shared, 1, b"still open")),
            ],
        );
    }
    differential(
        "daemon-rehello-ready-fails",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            mode("reject", "ready retry failed"),
            deliver_text(&hello(PEER_PUBLIC, "")),
            mode("pending", ""),
            deliver_text(&hello(PEER_PUBLIC, "")),
            deliver_text(&hello(PEER_PUBLIC, "")),
            settle(3, Some("late failure")),
            settle(4, None),
        ],
    );
    differential(
        "daemon-rehello-mismatch-close-throws",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            json!({ "op": "close-mode", "kind": "throw", "message": "already closing" }),
            deliver_text(&hello(OTHER_PUBLIC, "")),
            json!({ "op": "is-open" }),
        ],
    );
}

#[test]
fn open_channel_rejects_tampered_truncated_and_foreign_frames() {
    let shared = peer_shared();
    let valid = seal(&shared, 1, b"valid");
    let mut tampered = valid.clone();
    tampered[30] ^= 1;
    let mut tampered_nonce = valid.clone();
    tampered_nonce[0] ^= 0x80;
    let foreign = seal(&[7; 32], 1, b"valid");
    let Some(transcript) = differential(
        "daemon-adversarial-frames",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, binary_capability())),
            deliver_binary(&tampered),
            deliver_binary(&tampered_nonce),
            deliver_binary(&foreign),
            deliver_binary(&valid[..23]),
            deliver_binary(&valid[..24]),
            deliver_binary(&valid[..39]),
            deliver_binary(&valid[..valid.len() - 1]),
            deliver_text(""),
            deliver_text(" = "),
            deliver_text("!!!!"),
            deliver_text(r#"{"type":"app"}"#),
            deliver_text(" \u{a0}{\"type\":\"app\"}"),
            deliver_text("{not json"),
            deliver_text("[1,2]"),
            json!({ "op": "deliver", "text": "AAAA", "isBinary": true }),
            deliver_base64(&seal(&shared, 2, &[0xc3, 0x28])),
            deliver_base64(&seal(&shared, 3, b"\xef\xbb\xbftext")),
            json!({ "op": "deliver", "binary": hex(array_buffer_to_base64(&seal(&shared, 4, b"text as bytes")).as_bytes()), "isBinary": false }),
            // The channel stays open after every failure, so a valid frame,
            // a replay, and a reordered frame are all still delivered.
            deliver_binary(&valid),
            deliver_binary(&valid),
            deliver_binary(&seal(&shared, 6, b"sixth")),
            deliver_binary(&seal(&shared, 5, b"fifth")),
            json!({ "op": "is-open" }),
        ],
    ) else {
        return;
    };
    assert_eq!(
        count(&transcript, r#"{"t":"message","binary":"76616c6964"}"#),
        2
    );
    for reason in [
        "Decryption failed",
        "Ciphertext bundle too short",
        "Invalid typed array length: -1",
        "Received plaintext frame on encrypted channel",
        "Binary WebSocket frame did not contain bytes",
        "The encoded data was not valid for encoding utf-8",
    ] {
        assert!(
            has(
                &transcript,
                &format!(
                    r#"{{"t":"transport-close","code":1011,"reason":{}}}"#,
                    quote(reason)
                )
            ),
            "{reason}"
        );
    }
}

#[test]
fn raw_channels_and_close_behave_like_the_class() {
    let shared = peer_shared();
    differential(
        "raw-channel",
        &[
            json!({ "op": "raw", "shared": hex(&shared), "binary": true, "daemonSecret": null, "open": false, "seed": 3 }),
            json!({ "op": "is-open" }),
            send_binary(&[1]),
            json!({ "op": "raw", "shared": hex(&shared), "binary": true, "daemonSecret": null, "open": true, "seed": 3 }),
            mode("pending", ""),
            send_binary(&[1, 2, 3]),
            json!({ "op": "is-open" }),
            settle(1, None),
            json!({ "op": "transport-error", "message": "socket error" }),
            json!({ "op": "close-mode", "kind": "throw", "message": "close failed" }),
            json!({ "op": "close", "code": 4000, "reason": "custom" }),
            json!({ "op": "close-mode", "kind": "ok" }),
            json!({ "op": "close" }),
            send_text("closed"),
            json!({ "op": "transport-close", "code": 1000, "reason": "bye" }),
        ],
    );
}

#[test]
fn helpers_match_the_pinned_runtime() {
    let mut ops = Vec::new();
    for text in [
        "{}",
        " [1] ",
        r#"{"type":"e2ee_hello","key":"k","capabilities":{"binaryCiphertext":true}}"#,
        r#"{"type":"a","type":"e2ee_ready","capabilities":{"binaryCiphertext":null}}"#,
        r#"{"key":"\ud83d\ude00","capabilities":[],"type":false}"#,
        r#"{"type":"\udc00"}"#,
        r#"{"capabilities":{"binaryCiphertext":{"x":1}},"type":1e400}"#,
        "\"\\u0000\"",
        "01",
        "{\"a\":1,}",
        "\u{feff}{}",
        "\u{a0}{}",
        "[\"\t\"]",
        "-0.0e-0",
        "nulls",
    ] {
        ops.push(json!({ "op": "probe-json", "text": text }));
    }
    ops.push(
        json!({ "op": "probe-json", "text": format!("{}{}", "[".repeat(5000), "]".repeat(5000)) }),
    );
    for bytes in [
        "",
        "41",
        "efbbbf41",
        "efbbbfefbbbf",
        "efbb41",
        "ff",
        "c080",
        "eda080",
        "f4908080",
        "e282",
        "e28241",
        "f09f98",
        "f09f9880",
        "c3",
        "80bf",
        "f8888080",
    ] {
        ops.push(json!({ "op": "probe-decode", "binary": bytes }));
    }
    for text in [
        "",
        "=",
        " =",
        "==",
        "Zg",
        "Zm8",
        "Zm9vY",
        "Zm9v====",
        "Zg==Zm9v",
        "-_-_",
        "!!!!",
        "\u{a0}Zm9v\u{feff}",
        "\u{85}Zm9v",
        "é",
        "😀",
        "Zm9v\nYmFy",
        "AB=C",
    ] {
        ops.push(json!({ "op": "probe-base64", "text": text }));
    }
    for value in [0, 1, 40, 56, 59, 60, 1_000_000] {
        ops.push(json!({ "op": "probe-wire-sizes", "value": value }));
    }
    differential("helpers", &ops);
}

#[test]
fn rethrown_plaintext_frame_errors_close_with_their_message() {
    let shared = peer_shared();
    let Some(transcript) = differential(
        "daemon-rethrow-plaintext-frame",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            // Transport errors raised by a re-hello.
            mode("reject", "relay saw plaintext frame"),
            deliver_text(&hello(PEER_PUBLIC, "")),
            mode("pending", ""),
            deliver_text(&hello(PEER_PUBLIC, "")),
            settle(3, Some("late plaintext frame")),
            mode("sync", ""),
            // V8 parse errors that quote the frame itself.
            deliver_text(r#"{"plaintext frame":}"#),
            deliver_text(r#"{"plaintext frame":]"#),
            deliver_text(r#"{"plaintext frame": }"#),
            deliver_text(r#"{"a":["plaintext frame",]}"#),
            deliver_text(r"{plaintext frame}"),
            deliver_base64(&seal(&shared, 1, b"still open")),
            // A throwing close during key rotation.
            json!({ "op": "close-mode", "kind": "throw", "message": "close saw plaintext frame" }),
            deliver_text(&hello(OTHER_PUBLIC, "")),
            json!({ "op": "is-open" }),
        ],
    ) else {
        return;
    };
    for reason in [
        "relay saw plaintext frame",
        "late plaintext frame",
        r#"Unexpected token '}', "{"plaintext frame":}" is not valid JSON"#,
        r#"Unexpected token ']', "{"plaintext frame":]" is not valid JSON"#,
        "close saw plaintext frame",
    ] {
        assert!(
            has(
                &transcript,
                &format!(
                    r#"{{"t":"transport-close","code":1011,"reason":{}}}"#,
                    quote(reason)
                )
            ),
            "{reason}"
        );
    }
    assert!(has(&transcript, r#"{"t":"message","text":"still open"}"#));
}

#[test]
fn only_error_rejections_are_rethrown_from_a_rehello() {
    let Some(transcript) = differential(
        "daemon-rehello-non-error-rejection",
        &[
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
            // A string rejection carrying the marker falls through to
            // ciphertext decoding of the hello text instead of closing with it.
            mode("reject-value", "relay saw plaintext frame"),
            deliver_text(&hello(PEER_PUBLIC, "")),
            mode("pending", ""),
            deliver_text(&hello(PEER_PUBLIC, "")),
            json!({ "op": "settle", "id": 3, "errorValue": "late plaintext frame" }),
            // The same text as an Error rethrows.
            deliver_text(&hello(PEER_PUBLIC, "")),
            settle(4, Some("late plaintext frame")),
            mode("sync", ""),
            deliver_base64(&seal(&peer_shared(), 1, b"still open")),
        ],
    ) else {
        return;
    };
    assert!(!has(
        &transcript,
        r#"{"t":"transport-close","code":1011,"reason":"relay saw plaintext frame"}"#
    ));
    // Only the Error-typed settle of the same text closes with it.
    assert_eq!(
        count(
            &transcript,
            r#"{"t":"transport-close","code":1011,"reason":"late plaintext frame"}"#
        ),
        1
    );
    // Non-Error rejections elsewhere behave like errors, with String(value).
    differential(
        "non-error-rejections",
        &[
            mode("reject-value", "hello rejected with a string"),
            client(),
            json!({ "op": "tick" }),
            mode("sync", ""),
            deliver_text(READY_LEGACY),
            mode("reject-value", "app send rejected with a string"),
            send_text("rejected"),
            mode("pending", ""),
            send_text("pending"),
            json!({ "op": "settle", "id": 4, "errorValue": "pending rejected with a string" }),
        ],
    );
    differential(
        "daemon-ready-non-error-rejection",
        &[
            mode("reject-value", "ready rejected with a string"),
            daemon(),
            deliver_text(&hello(PEER_PUBLIC, "")),
        ],
    );
}

#[test]
fn unexpected_token_messages_match_v8() {
    let bases = [
        r#"{"plaintext frame":[1,true,null]}"#,
        r#"{"a":{"b":"plaintext frame"},"c":false}"#,
    ];
    let inserts = [
        ":", ",", "}", "]", "x", "\"", "1", "-", " ", "é", "😀", "t", "n", "{", "[",
    ];
    let mut ops = Vec::new();
    for base in bases {
        let units: Vec<char> = base.chars().collect();
        for index in 0..=units.len() {
            for insert in inserts {
                let mut text: String = units[..index].iter().collect();
                text.push_str(insert);
                text.extend(&units[index..]);
                ops.push(json!({ "op": "probe-json-error", "text": text }));
            }
        }
    }
    let Some(transcript) = differential("json-unexpected-token", &ops) else {
        return;
    };
    assert!(
        count(
            &transcript,
            r#"{"t":"json-error","message":"Unexpected token"#
        ) > 100
    );
    assert!(count(&transcript, r#"{"t":"json-error","message":null}"#) > 100);
}

fn frame_text(text: &str) -> Value {
    json!({ "text": text })
}

fn frame_binary(bytes: &[u8]) -> Value {
    json!({ "binary": hex(bytes) })
}

#[test]
fn frames_delivered_in_one_task_interleave_with_awaited_sends() {
    let shared = peer_shared();
    // The daemon buffers everything that arrives while its ready frame is
    // in flight, drops buffered hello and ready frames, and replays the rest
    // after opening.
    let Some(transcript) = differential(
        "batch-daemon-same-task",
        &[
            daemon(),
            json!({ "op": "batch", "frames": [
                frame_text(&hello(PEER_PUBLIC, binary_capability())),
                frame_binary(&seal(&shared, 1, b"first")),
                frame_text(&hello(PEER_PUBLIC, binary_capability())),
                frame_text(READY_LEGACY),
                frame_text(r#"{"type":"app"}"#),
                frame_binary(&seal(&shared, 2, b"second")),
                frame_binary(&seal(&shared, 1, b"first")),
            ]}),
            deliver_binary(&seal(&shared, 3, b"after")),
        ],
    ) else {
        return;
    };
    assert_eq!(
        count(&transcript, r#"{"t":"message","binary":"666972737"#),
        2
    );
    assert!(has(&transcript, r#"{"t":"handshake","ok":true}"#));
    differential(
        "batch-daemon-ciphertext-first",
        &[
            daemon(),
            json!({ "op": "batch", "frames": [
                frame_text(&hello(PEER_PUBLIC, "")),
                frame_base64(&seal(&shared, 4, b"pipelined")),
            ]}),
        ],
    );
    let client_key = client_shared();
    // A client that opens on ready flushes its backlog while later frames of
    // the same task are already being handled.
    let Some(transcript) = differential(
        "batch-client-same-task",
        &[
            client(),
            send_text("queued one"),
            send_binary(&[7, 7]),
            send_text("queued two"),
            json!({ "op": "batch", "frames": [
                frame_text(READY_BINARY),
                frame_binary(&seal(&client_key, 1, b"early reply")),
                frame_text(READY_LEGACY),
                frame_text(&hello(PEER_PUBLIC, "")),
                frame_base64(&seal(&client_key, 2, b"text reply")),
            ]}),
            deliver_text(READY_LEGACY),
        ],
    ) else {
        return;
    };
    assert!(has(&transcript, r#"{"t":"open"}"#));
    // The same batch with a failing backlog send.
    differential(
        "batch-client-flush-failure",
        &[
            client(),
            send_text("queued one"),
            send_text("queued two"),
            json!({ "op": "queue", "modes": [{ "kind": "sync" }, { "kind": "reject", "message": "flush failed" }] }),
            json!({ "op": "batch", "frames": [
                frame_text(READY_BINARY),
                frame_binary(&seal(&client_key, 3, b"during flush")),
            ]}),
        ],
    );
}

fn pair_steps() -> Vec<(&'static str, Value)> {
    vec![
        ("daemon", daemon()),
        ("client", client()),
        ("client", send_text("Hello from client")),
        ("daemon", send_text("Hello from daemon")),
        ("client", send_binary(&[0, 1, 2, 3, 0xff])),
        ("daemon", send_binary(b"terminal bytes")),
        ("client", send_text("Second message from client")),
        // The retry interval stops once open and once the transport closes.
        ("client", json!({ "op": "tick" })),
        (
            "client",
            json!({ "op": "transport-close", "code": 1000, "reason": "done" }),
        ),
        ("client", json!({ "op": "tick" })),
    ]
}

#[test]
fn typescript_and_rust_interoperate_in_every_direction() {
    let Some(pinned) = pinned() else {
        return;
    };
    let steps = pair_steps();
    let reference = pair(Side::Node, Side::Node, &pinned, &steps);
    support::record_lines("pair.client-node-daemon-node", &reference);
    for (client, daemon) in [
        (Side::Node, Side::Rust),
        (Side::Rust, Side::Node),
        (Side::Rust, Side::Rust),
    ] {
        let transcript = pair(client, daemon, &pinned, &steps);
        support::record_lines(
            &format!("pair.client-{client:?}-daemon-{daemon:?}").to_lowercase(),
            &transcript,
        );
        assert_eq!(
            transcript, reference,
            "client {client:?} with daemon {daemon:?}"
        );
    }
    let delivered = |entry: &str| {
        reference
            .iter()
            .filter(|line| line.ends_with(entry))
            .count()
    };
    assert_eq!(delivered(r#"{"t":"open"}"#), 2);
    assert_eq!(delivered(r#"{"t":"handshake","ok":true}"#), 1);
    assert_eq!(
        delivered(r#"{"t":"message","text":"Hello from client"}"#),
        1
    );
    assert_eq!(
        delivered(r#"{"t":"message","text":"Hello from daemon"}"#),
        1
    );
    assert_eq!(delivered(r#"{"t":"message","binary":"00010203ff"}"#), 1);
    assert_eq!(
        delivered(&format!(
            r#"{{"t":"message","binary":"{}"}}"#,
            hex(b"terminal bytes")
        )),
        1
    );
    let public = export_public_key(&key_pair_from_secret(daemon_secret()).public_key).unwrap();
    assert_eq!(public, DAEMON_PUBLIC);
}
