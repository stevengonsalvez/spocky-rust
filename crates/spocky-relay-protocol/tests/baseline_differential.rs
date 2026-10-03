//! Raw-text differential against the pinned relay's own wire functions.
//!
//! The corpus and the baseline output are committed fixtures. The baseline output is
//! produced by `scripts/phase4/relay-protocol-differential.sh`, which runs
//! `PaseoRelay.HandshakeValidation`, `PaseoRelay.Connection`, `:cow_qs` and Jason from
//! the pinned checkout. This test renders the same corpus with the Rust crate and
//! compares every line as raw text. Only the random connection identifier is masked.

#![allow(clippy::too_many_lines)] // the corpus generators are long tables

use spocky_relay_protocol::connection::{Connection, Role, Version, from_query};
use spocky_relay_protocol::handshake::{Handshake, check};
use spocky_relay_protocol::query::{QueryValue, into_query_map, parse_qs};
use spocky_relay_protocol::{control, limits};
use std::cell::Cell;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |value, (index, byte)| {
                value | (u32::from(*byte) << (16 - 8 * index))
            });
        for position in 0..4 {
            if position <= chunk.len() {
                let index = usize::try_from((triple >> (18 - 6 * position)) & 0x3f).unwrap();
                encoded.push(char::from(BASE64[index]));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn unb64(text: &str) -> Vec<u8> {
    let symbols: Vec<u32> = text
        .bytes()
        .filter(|byte| *byte != b'=')
        .map(|byte| u32::try_from(BASE64.iter().position(|c| *c == byte).unwrap()).unwrap())
        .collect();
    let mut bytes = Vec::new();
    for chunk in symbols.chunks(4) {
        let accumulator = chunk
            .iter()
            .fold(0_u32, |value, symbol| (value << 6) | symbol)
            << (6 * (4 - chunk.len()));
        for index in 0..chunk.len() - 1 {
            bytes.push(u8::try_from((accumulator >> (16 - 8 * index)) & 0xff).unwrap());
        }
    }
    bytes
}

fn render_connection(query: &std::collections::BTreeMap<Vec<u8>, Vec<u8>>) -> String {
    let generated = Cell::new(false);
    let result = from_query(query, || {
        generated.set(true);
        [0xab; 8]
    });
    match result {
        Ok(Connection {
            server_id,
            role,
            version,
            connection_id,
        }) => {
            let role = match role {
                Role::Server => "server",
                Role::Client => "client",
            };
            let (version, id) = match version {
                Version::V1 => (1, "nil".to_owned()),
                Version::V2 if generated.get() => {
                    // The relay draws 8 random bytes and writes conn_ plus lowercase hex; the
                    // closure above returned [0xab; 8]. Only a well formed id is masked.
                    let id = connection_id.unwrap();
                    let well_formed = id.len() == 21
                        && id.starts_with(b"conn_")
                        && id[5..]
                            .iter()
                            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                        && id == b"conn_abababababababab";
                    (
                        2,
                        if well_formed {
                            "generated"
                        } else {
                            "BAD_GENERATED"
                        }
                        .to_owned(),
                    )
                }
                Version::V2 => (2, b64(&connection_id.unwrap())),
            };
            format!(
                "ok role={role} serverId={} v={version} connectionId={id}",
                b64(&server_id)
            )
        }
        Err(message) => format!("error {message}"),
    }
}

fn parse_ids(ids: &str) -> Vec<Vec<u8>> {
    if ids == "none" {
        return Vec::new();
    }
    ids.split(',')
        .map(|id| if id == "_" { Vec::new() } else { unb64(id) })
        .collect()
}

fn render_encoded(result: Result<String, control::InvalidUtf8>) -> String {
    match result {
        Ok(json) => format!("ok {json}"),
        Err(control::InvalidUtf8) => "error".to_owned(),
    }
}

fn render(line: &str) -> String {
    let mut parts = line.split('\t');
    let kind = parts.next().unwrap();
    let arguments: Vec<&str> = parts.collect();
    match (kind, arguments.as_slice()) {
        ("hs", [payload]) => match check(&unb64(payload)) {
            Handshake::NotHandshake => "not_handshake".to_owned(),
            Handshake::Accept(kind) => format!("accept {}", kind.as_str()),
            Handshake::Reject(kind) => format!("reject {}", kind.as_str()),
        },
        ("ping", [payload]) => if control::is_ping(&unb64(payload)) {
            "ping"
        } else {
            "ignore"
        }
        .to_owned(),
        ("qs", [query]) => match parse_qs(&unb64(query)) {
            Err(_) => "qs_error".to_owned(),
            Ok(pairs) => {
                let shown = pairs
                    .iter()
                    .map(|(name, value)| match value {
                        QueryValue::Flag => format!("{}:flag", b64(name)),
                        QueryValue::Bytes(bytes) => format!("{}:{}", b64(name), b64(bytes)),
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "pairs={shown} {}",
                    render_connection(&into_query_map(pairs))
                )
            }
        },
        ("sync", [ids]) => {
            let ids = parse_ids(ids);
            let borrowed: Vec<&[u8]> = ids.iter().map(Vec::as_slice).collect();
            render_encoded(control::sync(&borrowed))
        }
        ("syncdel", [ids, deleted]) => {
            // The Owner removes a client from its map. Whatever the history, the keys the
            // map holds now decide the order, so the remaining set is what the port gets.
            let deleted = parse_ids(deleted);
            let remaining: Vec<Vec<u8>> = parse_ids(ids)
                .into_iter()
                .filter(|id| !deleted.contains(id))
                .collect();
            let borrowed: Vec<&[u8]> = remaining.iter().map(Vec::as_slice).collect();
            render_encoded(control::sync(&borrowed))
        }
        ("connected", [id]) => render_encoded(control::connected(&unb64(id))),
        ("disconnected", [id]) => render_encoded(control::disconnected(&unb64(id))),
        ("pong", [ts]) => format!("ok {}", control::pong(ts.parse().unwrap())),
        ("limits", []) => format!(
            "{} {} {} {}",
            limits::MAXIMUM_FRAME_WIRE_BYTES,
            limits::MAXIMUM_CLIENT_FRAME_PAYLOAD_BYTES,
            limits::MAXIMUM_MESSAGE_PAYLOAD_BYTES,
            limits::MAXIMUM_CONTROL_PAYLOAD_BYTES
        ),
        other => panic!("unknown corpus case {other:?}"),
    }
}

fn render_corpus(corpus: &str) -> String {
    let mut output = String::new();
    for line in corpus.lines() {
        writeln!(output, "{line}\t=>\t{}", render(line)).unwrap();
    }
    output
}

const FIXTURE_PAIRS: [(&str, &str); 2] = [
    ("relay-protocol-corpus.tsv", "relay-protocol-baseline.tsv"),
    (
        "relay-protocol-extra-corpus.tsv",
        "relay-protocol-extra-baseline.tsv",
    ),
];

#[test]
fn rust_output_matches_the_pinned_relay_line_for_line() {
    for (corpus, baseline) in FIXTURE_PAIRS {
        let corpus = fs::read_to_string(fixture(corpus)).unwrap();
        let expected = fs::read_to_string(fixture(baseline)).unwrap();
        let actual = render_corpus(&corpus);
        let mismatches: Vec<String> = expected
            .lines()
            .zip(actual.lines())
            .filter(|(expected, actual)| expected != actual)
            .map(|(expected, actual)| format!("pinned: {expected}\nrust:   {actual}"))
            .collect();
        assert!(
            mismatches.is_empty(),
            "{baseline}: {} mismatches, first 5:\n{}",
            mismatches.len(),
            mismatches
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(expected.lines().count(), actual.lines().count());
    }
}

#[test]
#[ignore = "writes a rendering for scripts/phase4/relay-protocol-differential.sh"]
fn render_corpus_file() {
    let input = std::env::var("SPOCKY_RELAY_CORPUS_IN").unwrap();
    let output = std::env::var("SPOCKY_RELAY_RENDER_OUT").unwrap();
    fs::write(output, render_corpus(&fs::read_to_string(input).unwrap())).unwrap();
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn bytes(&mut self, count: usize) -> Vec<u8> {
        (0..count)
            .map(|_| u8::try_from(self.next() & 0xff).unwrap())
            .collect()
    }
}

const INTERESTING: &[u8] =
    b"\"{}[],:\\ \t\n\r\x0b\x0c0123456789eE.+-tfnulsraq/\x00\x1f\x7f\x80\xc2\xe2\xed\xf0\xff";

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    for _ in 0..rng.below(4) {
        let at = if bytes.is_empty() {
            0
        } else {
            rng.below(bytes.len())
        };
        match rng.below(5) {
            0 if !bytes.is_empty() => {
                bytes.remove(at);
            }
            1 => bytes.insert(at, *rng.pick(INTERESTING)),
            2 if !bytes.is_empty() => bytes[at] = *rng.pick(INTERESTING),
            3 if bytes.len() > 2 => {
                let end = (at + 1 + rng.below(8)).min(bytes.len());
                let piece = bytes[at..end].to_vec();
                bytes.splice(at..at, piece);
            }
            _ if bytes.len() > 1 => {
                let other = rng.below(bytes.len());
                bytes.swap(at, other);
            }
            _ => {}
        }
    }
    bytes
}

fn key_pool(rng: &mut Rng) -> Vec<String> {
    let mut keys = Vec::new();
    for _ in 0..12 {
        let mut key = rng.bytes(32);
        key[31] &= 0x7f;
        keys.push(b64(&key));
    }
    for hex in [
        "00".repeat(32),
        format!("01{}", "00".repeat(31)),
        "E0EB7A7C3B41B8AE1656E3FAF19FC46ADA098DEB9C32B1FD866205165F49B800".to_owned(),
        "5F9C95BCA3508C24B1D0B1559C83EF5B04445CC4581C8E86D8224EDDD09F1157".to_owned(),
        format!("ECFF{}7F", "FF".repeat(29)),
        format!("EDFF{}7F", "FF".repeat(29)),
        format!("EEFF{}7F", "FF".repeat(29)),
        format!("EBFF{}7F", "FF".repeat(29)),
        format!("EC{}7F", "FF".repeat(30)),
        format!("ED{}FF", "FF".repeat(30)),
        format!("EC{}7E", "FF".repeat(30)),
        format!("ED{}80", "00".repeat(30)),
        format!("EC{}7F", "00".repeat(30)),
    ] {
        let bytes: Vec<u8> = (0..32)
            .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap())
            .collect();
        keys.push(b64(&bytes));
    }
    let valid = keys[0].clone();
    keys.extend([
        valid[..43].to_owned(),
        format!("{valid}="),
        valid.replace('+', "-").replace('/', "_"),
        format!("{valid} "),
        format!("{}=", &valid[..43]),
        valid[..42].to_owned() + "B=",
        valid[..42].to_owned() + "C=",
        valid[..42].to_owned() + "A=",
        valid[..42].to_owned() + "AA",
        String::new(),
        "AAAA".to_owned(),
        b64(&[7; 31]),
        b64(&[7; 33]),
        valid.to_lowercase(),
        valid.replace('=', "\\u003d"),
        valid.replace(&valid[..1], "\\u0041"),
    ]);
    keys
}

fn handshake_corpus(rng: &mut Rng) -> Vec<Vec<u8>> {
    let keys = key_pool(rng);
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    for key in &keys {
        for kind in ["hello", "e2ee_hello"] {
            seeds.push(format!(r#"{{"type":"{kind}","key":"{key}"}}"#).into_bytes());
        }
    }
    let valid = &keys[0];
    let invalid = &keys[12];
    for text in [
        format!(r#"{{"type":"e2ee_hello","key":"{valid}","capabilities":{{}}}}"#),
        format!(r#"{{ "key" : "{valid}" , "type" : "hello" }}"#),
        format!("\t\r\n{{\"type\":\"hello\",\"key\":\"{valid}\"}}\n\t"),
        format!(r#"{{"type":"hello","key":"{valid}","key":"{invalid}"}}"#),
        format!(r#"{{"type":"hello","key":"{invalid}","key":"{valid}"}}"#),
        format!(r#"{{"type":"ping","type":"hello","key":"{invalid}"}}"#),
        format!(r#"{{"type":"hello","type":"ping","key":"{invalid}"}}"#),
        format!(r#"{{"type":1,"type":"hello","key":"{invalid}"}}"#),
        format!(r#"{{"type":["hello"],"key":"{invalid}"}}"#),
        format!(r#"{{"a":{{"type":"hello","key":"{invalid}"}}}}"#),
        format!(r#"[{{"type":"hello","key":"{invalid}"}}]"#),
        r#""hello""#.to_owned(),
        r#"{"type":"hello"}"#.to_owned(),
        r#"{"type":"hello","key":null}"#.to_owned(),
        r#"{"type":"hello","key":1}"#.to_owned(),
        r#"{"type":"hello","key":["a"]}"#.to_owned(),
        r#"{"type":"hello","key":{}}"#.to_owned(),
        r#"{"type":"hello","key":""}"#.to_owned(),
        r#"{"type":"Hello"}"#.to_owned(),
        r#"{"type":"hello "}"#.to_owned(),
        r#"{"type":"hello"}"#.to_owned(),
        r#"{"type":"hello","ignored":1e400}"#.to_owned(),
        r#"{"type":"hello","ignored":-1e400}"#.to_owned(),
        r#"{"type":"hello","ignored":1e-400}"#.to_owned(),
        r#"{"type":"hello","ignored":1E+2}"#.to_owned(),
        r#"{"type":"hello","ignored":1.5e3}"#.to_owned(),
        r#"{"type":"hello","ignored":01}"#.to_owned(),
        r#"{"type":"hello","ignored":-}"#.to_owned(),
        r#"{"type":"hello","ignored":1.}"#.to_owned(),
        r#"{"type":"hello","ignored":.5}"#.to_owned(),
        r#"{"type":"hello","ignored":-0}"#.to_owned(),
        r#"{"type":"hello","ignored":0.0e0}"#.to_owned(),
        r#"{"type":"hello","ignored":+1}"#.to_owned(),
        r#"{"type":"hello","ignored":NaN}"#.to_owned(),
        r#"{"type":"hello","ignored":Infinity}"#.to_owned(),
        r#"{"type":"hello","ignored":tru}"#.to_owned(),
        r#"{"type":"hello","ignored":nul}"#.to_owned(),
        r#"{"type":"hello",}"#.to_owned(),
        r#"{,"type":"hello"}"#.to_owned(),
        r#"{"type":"hello"} x"#.to_owned(),
        r#"{"type":"hello"}{}"#.to_owned(),
        "{'type':'hello'}".to_owned(),
        r#"{"type":"hello","s":"😀"}"#.to_owned(),
        r#"{"type":"hello","s":"\ud83d"}"#.to_owned(),
        r#"{"type":"hello","s":"\ude00"}"#.to_owned(),
        r#"{"type":"hello","s":"\ud83dA"}"#.to_owned(),
        r#"{"type":"hello","s":"\u00"}"#.to_owned(),
        r#"{"type":"hello","s":"\x41"}"#.to_owned(),
        r#"{"type":"hello","s":"\/"}"#.to_owned(),
        r#"{"type":"hello","s":"tab	raw"}"#.to_owned(),
        "{\"type\":\"hello\",\"s\":\"del\u{7f}\"}".to_owned(),
        "{\"type\":\"hello\",\"s\":\"\u{2028}\u{feff}\"}".to_owned(),
        "\u{feff}{\"type\":\"hello\"}".to_owned(),
        "{\"type\":\"hello\"}\u{feff}".to_owned(),
        "{\"type\":\"hello\"}\u{b}".to_owned(),
        "{\"type\":\"hello\"}\u{c}".to_owned(),
        "{\"type\":\"hello\"}\u{a0}".to_owned(),
        String::new(),
        " ".to_owned(),
        "null".to_owned(),
        "{}".to_owned(),
        "[]".to_owned(),
    ] {
        seeds.push(text.into_bytes());
    }
    seeds.push(format!(r#"{{"type":"hello","ignored":{}}}"#, "9".repeat(1_023)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":{}}}"#, "9".repeat(1_024)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":{}}}"#, "9".repeat(1_025)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":-{}}}"#, "9".repeat(1_023)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":-{}}}"#, "9".repeat(1_024)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":{}.5}}"#, "9".repeat(2_000)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":1.{}}}"#, "0".repeat(2_000)).into_bytes());
    seeds.push(format!(r#"{{"type":"hello","ignored":1.{}e5}}"#, "0".repeat(2_000)).into_bytes());
    seeds.push(vec![0xff, 0xfe]);
    seeds.push(b"{\"type\":\"hello\",\"s\":\"\xff\"}".to_vec());
    seeds.push(b"{\"type\":\"hello\",\"s\":\"\xed\xa0\x80\"}".to_vec());
    seeds.push(b"{\"type\":\"hello\",\"s\":\"\xc0\x80\"}".to_vec());
    seeds.push(b"{\"type\":\"hello\",\"s\":\"\xf4\x90\x80\x80\"}".to_vec());

    let mut corpus = seeds.clone();
    for _ in 0..1_400 {
        let seed = rng.pick(&seeds).clone();
        corpus.push(mutate(rng, &seed));
    }
    // Deep nesting is added once and never mutated, to keep the fixture small.
    corpus.push(
        format!(
            r#"{{"type":"hello","ignored":{}1{}}}"#,
            "[".repeat(20_000),
            "]".repeat(20_000)
        )
        .into_bytes(),
    );
    corpus.push(format!(r#"{{"type":"hello","ignored":{}}}"#, "[".repeat(20_000)).into_bytes());
    corpus
}

fn ping_corpus(rng: &mut Rng) -> Vec<Vec<u8>> {
    let seeds: Vec<Vec<u8>> = [
        r#"{"type":"ping"}"#,
        r#"{"type":"ping","ts":1}"#,
        r#" {"ts":1,"type":"ping"} "#,
        r#"{"type":"ping","type":"pong"}"#,
        r#"{"type":"pong","type":"ping"}"#,
        r#"{"type":"Ping"}"#,
        r#"{"type":["ping"]}"#,
        r#"{"type":"ping"}"#,
        r#"{"x":{"type":"ping"}}"#,
        r#"["ping"]"#,
        r#"{"type":"ping"} x"#,
        r#"{"type":"ping","a":1e999}"#,
        "",
        "ping",
    ]
    .iter()
    .map(|text| text.as_bytes().to_vec())
    .collect();
    let mut corpus = seeds.clone();
    for _ in 0..250 {
        let seed = rng.pick(&seeds).clone();
        corpus.push(mutate(rng, &seed));
    }
    corpus
}

fn percent(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut encoded, byte| {
        let _ = write!(encoded, "%{byte:02X}");
        encoded
    })
}

fn value_pool(rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut pool: Vec<Vec<u8>> = [
        "abc",
        "",
        "a",
        "srv_1",
        "conn_1",
        "x y",
        "x+y",
        "%20",
        "%2B",
        "é",
        "a=b",
        "a%26b",
        "%FF",
        "%C2",
        "%C2%A0",
        "a%00b",
        "%0A",
        "a%0Ab",
        "\"quoted\"",
        "back\\slash",
        "tab%09",
        "%E2%80%A8",
        "%F0%9F%98%80",
        "%ED%A0%80",
        "A%2fB",
        "~",
        "%7E",
        "%7e",
    ]
    .iter()
    .map(|text| text.as_bytes().to_vec())
    .collect();
    for length in [254, 255, 256, 257, 258, 300] {
        pool.push(vec![b'a'; length]);
    }
    for padded in [255, 256, 257] {
        let mut value = b"%20".to_vec();
        value.extend(vec![b'b'; padded]);
        value.extend(b"%20");
        pool.push(value);
    }
    pool.push(b"%C3%A9".repeat(128));
    pool.push(b"%C3%A9".repeat(129));
    pool.push(b"%E2%82%AC".repeat(86));
    pool.push(
        rng.bytes(12)
            .iter()
            .flat_map(|byte| percent(&[*byte]).into_bytes())
            .collect(),
    );
    pool
}

fn trim_pool() -> Vec<Vec<u8>> {
    let spaces: &[&[u8]] = &[
        b"%20",
        b"%09",
        b"%0A",
        b"%0B",
        b"%0C",
        b"%0D",
        b"%C2%85",
        b"%C2%A0",
        b"%E1%9A%80",
        b"%E2%80%80",
        b"%E2%80%8A",
        b"%E2%80%8B",
        b"%E2%80%A8",
        b"%E2%80%A9",
        b"%E2%80%AF",
        b"%E2%81%9F",
        b"%E3%80%80",
        b"%EF%BB%BF",
        b"%E1%A0%8E",
        b"%1C",
        b"%1F",
        b"+",
        b"%00",
        b"%FF",
        b"%C2",
        b"%E2%80",
        b"%A0",
        b"%85",
    ];
    let mut pool = Vec::new();
    for space in spaces {
        for core in [&b"2"[..], &b"1"[..], &b"id"[..], &b""[..]] {
            for shape in 0..4 {
                let mut value = Vec::new();
                if shape & 1 == 1 {
                    value.extend_from_slice(space);
                }
                value.extend_from_slice(core);
                if shape & 2 == 2 {
                    value.extend_from_slice(space);
                }
                pool.push(value);
            }
        }
    }
    for pair in spaces.windows(2) {
        pool.push([pair[0], b"2", pair[1]].concat());
        pool.push([pair[1], pair[0], b"7", pair[0], pair[1]].concat());
    }
    pool
}

fn query_corpus(rng: &mut Rng) -> Vec<Vec<u8>> {
    let pool = value_pool(rng);
    let trims = trim_pool();
    let roles: [&[u8]; 9] = [
        b"server",
        b"client",
        b"",
        b"Server",
        b"CLIENT",
        b"server%20",
        b"client%00",
        b"x",
        b"s%65rver",
    ];
    let versions: [&[u8]; 9] = [b"1", b"2", b"", b"3", b"02", b"2.0", b"v2", b"%32", b"1%00"];
    let mut corpus: Vec<Vec<u8>> = [
        "",
        "&",
        "a",
        "a&",
        "&a",
        "a&b",
        "a&&b",
        "=",
        "=b",
        "a=",
        "a=b",
        "a=b=c",
        "+",
        "+=+",
        "role=server&serverId=s",
        "role=client&serverId=s&v=2",
        "role=client&serverId=s&v=2&connectionId=",
        "role=server&serverId=s&v=2&connectionId=",
        "role=server&serverId=s&v=2&connectionId=c1",
        "role=server&role=client&serverId=s",
        "role=client&serverId=a&serverId=b",
        "role&serverId=s",
        "role=server&serverId",
        "role=server&serverId=s&v",
        "role=server&serverId=s&v&connectionId",
        "role=client&serverId=s&v=2&connectionId",
        "role=server&serverId=%",
        "role=server&serverId=%1",
        "role=server&serverId=%zz",
        "role=server&serverId=%ZZ",
        "role=server&serverId=%2",
        "%=1",
        "a%=1",
        "a=%1&b=2",
        "role=server&serverId=s&v=2&connectionId=%e2%82%ac",
        "role=server&serverId=%e2%82%AC",
    ]
    .iter()
    .map(|text| text.as_bytes().to_vec())
    .collect();
    for key_count in [98, 99, 100, 101, 102] {
        for trailing in ["", "&"] {
            let mut query = (0..key_count)
                .map(|index| format!("k{index}=v"))
                .collect::<Vec<_>>()
                .join("&");
            query.push_str(trailing);
            corpus.push(query.into_bytes());
        }
    }
    corpus.push(
        format!(
            "role=server&serverId=s&{}",
            (0..96)
                .map(|i| format!("k{i}"))
                .collect::<Vec<_>>()
                .join("&")
        )
        .into_bytes(),
    );
    corpus.push(
        format!(
            "role=server&serverId=s&{}",
            (0..97)
                .map(|i| format!("k{i}"))
                .collect::<Vec<_>>()
                .join("&")
        )
        .into_bytes(),
    );
    for _ in 0..900 {
        let mut fields: Vec<Vec<u8>> = Vec::new();
        let add = |rng: &mut Rng, name: &str, value: &[u8], fields: &mut Vec<Vec<u8>>| {
            let mut field = name.as_bytes().to_vec();
            if rng.below(12) != 0 {
                field.push(b'=');
                field.extend_from_slice(value);
            }
            fields.push(field);
        };
        if rng.below(10) > 0 {
            let role = if rng.below(100) < 85 {
                rng.pick(&[b"server".as_slice(), b"client".as_slice()])
                    .to_vec()
            } else {
                rng.pick(&roles).to_vec()
            };
            add(rng, "role", &role, &mut fields);
        }
        if rng.below(10) > 0 {
            let value = if rng.below(100) < 70 {
                b"srv".to_vec()
            } else {
                rng.pick(&pool).clone()
            };
            add(rng, "serverId", &value, &mut fields);
        }
        if rng.below(4) > 0 {
            let value = match rng.below(6) {
                0 => rng.pick(&trims).clone(),
                1..=3 => b"2".to_vec(),
                _ => rng.pick(&versions).to_vec(),
            };
            add(rng, "v", &value, &mut fields);
        }
        if rng.below(5) > 0 {
            let value = if rng.below(2) == 0 {
                rng.pick(&trims).clone()
            } else {
                rng.pick(&pool).clone()
            };
            add(rng, "connectionId", &value, &mut fields);
        }
        for _ in 0..rng.below(3) {
            let value = rng.pick(&pool).clone();
            let name = *rng.pick(&["role", "serverId", "v", "connectionId", "x", "session"]);
            add(rng, name, &value, &mut fields);
        }
        for index in (1..fields.len()).rev() {
            fields.swap(index, rng.below(index + 1));
        }
        let mut query = fields.join(&b'&');
        if rng.below(8) == 0 {
            query = mutate(rng, &query);
        }
        corpus.push(query);
    }
    corpus
}

fn encoder_corpus(rng: &mut Rng) -> Vec<String> {
    let id_pool: Vec<Vec<u8>> = vec![
        b"a".to_vec(),
        b"conn_0011223344556677".to_vec(),
        Vec::new(),
        b"quote\"back\\slash/".to_vec(),
        b"\x00\x01\x08\x09\x0a\x0b\x0c\x0d\x1b\x1f\x7f".to_vec(),
        "é€😀\u{2028}\u{2029}\u{feff}\u{85}\u{a0}"
            .as_bytes()
            .to_vec(),
        vec![0xff],
        vec![0xc3],
        vec![0xed, 0xa0, 0x80],
        b"a<b>&c'".to_vec(),
        b"</script>".to_vec(),
        vec![b'z'; 256],
    ];
    let mut lines = Vec::new();
    for id in &id_pool {
        lines.push(format!("connected\t{}", b64(id)));
        lines.push(format!("disconnected\t{}", b64(id)));
        lines.push(format!(
            "sync\t{}",
            if id.is_empty() {
                "_".to_owned()
            } else {
                b64(id)
            }
        ));
    }
    lines.push("sync\tnone".to_owned());
    for count in [2, 3, 5, 12, 31, 32, 33, 34, 40, 64, 100, 257] {
        for _ in 0..8 {
            let ids: Vec<String> = (0..count)
                .map(|_| {
                    let length = rng.below(12);
                    let mut id = rng.bytes(length);
                    for byte in &mut id {
                        *byte = b"abcXYZ019_-. \"\\"[usize::from(*byte) % 15];
                    }
                    if id.is_empty() {
                        "_".to_owned()
                    } else {
                        b64(&id)
                    }
                })
                .collect();
            lines.push(format!("sync\t{}", ids.join(",")));
        }
    }
    for ts in [
        0_i64,
        1,
        1_790_000_000_000,
        9_007_199_254_740_993,
        -1,
        i64::MAX,
    ] {
        lines.push(format!("pong\t{ts}"));
    }
    lines
}

#[test]
#[ignore = "regenerates the committed corpus fixture"]
fn write_corpus() {
    let output = std::env::var("SPOCKY_RELAY_CORPUS_OUT").unwrap();
    let mut rng = Rng(0x005e_ed0f_5b0c_4ab1);
    let mut corpus = String::from("limits\n");
    for payload in handshake_corpus(&mut rng) {
        writeln!(corpus, "hs\t{}", b64(&payload)).unwrap();
    }
    for payload in ping_corpus(&mut rng) {
        writeln!(corpus, "ping\t{}", b64(&payload)).unwrap();
    }
    for query in query_corpus(&mut rng) {
        writeln!(corpus, "qs\t{}", b64(&query)).unwrap();
    }
    for line in encoder_corpus(&mut rng) {
        writeln!(corpus, "{line}").unwrap();
    }
    fs::write(output, corpus).unwrap();
}

/// Distinct ids for a map: `count` ids of one shape.
fn id_set(rng: &mut Rng, shape: &str, count: usize) -> Vec<Vec<u8>> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
    let wide = ["é", "€", "😀", "ß", "日本", "ж"];
    let mut ids: Vec<Vec<u8>> = Vec::new();
    let mut counter = 0_usize;
    while ids.len() < count {
        counter += 1;
        let id = match shape {
            "conn21" => {
                let mut id = b"conn_".to_vec();
                for _ in 0..16 {
                    id.push(b"0123456789abcdef"[rng.below(16)]);
                }
                id
            }
            "nonascii" => {
                let mut id = Vec::new();
                for _ in 0..=rng.below(8) {
                    id.extend_from_slice(rng.pick(&wide).as_bytes());
                }
                id.extend_from_slice(counter.to_string().as_bytes());
                id
            }
            "mixed" => {
                let length = 1 + rng.below(40);
                let mut id = counter.to_string().into_bytes();
                while id.len() < length {
                    id.push(*rng.pick(ALPHABET));
                }
                id
            }
            length => {
                let length: usize = length.parse().unwrap();
                let mut id = format!("{counter}_").into_bytes();
                while id.len() < length {
                    id.push(*rng.pick(ALPHABET));
                }
                id.truncate(length);
                id
            }
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn ids_field(ids: &[Vec<u8>]) -> String {
    if ids.is_empty() {
        return "none".to_owned();
    }
    ids.iter()
        .map(|id| {
            if id.is_empty() {
                "_".to_owned()
            } else {
                b64(id)
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Two keys whose hashes share their low `bits` bits: they stay together in the trie for
/// `bits / 4` levels. Searched deterministically.
fn near_collision(prefix: &str, bits: u32) -> (Vec<u8>, Vec<u8>) {
    let mask = (1_u64 << bits) - 1;
    let mut seen = std::collections::HashMap::new();
    for counter in 0_u64.. {
        let key = format!("{prefix}{counter}").into_bytes();
        let low = spocky_relay_protocol::erlang_map::hash(&key) & mask;
        if let Some(other) = seen.insert(low, key.clone()) {
            return (other, key);
        }
    }
    unreachable!()
}

fn extra_json_cases() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let key = b64(&[7; 32]);
    let invalid = b64(&[0; 32]);
    let mut hs: Vec<String> = Vec::new();
    for field in [
        r#""s":"\ud83d\ude00""#,
        "\"s\":\"😀\"",
        r#""s":"\ud83d\ude00\ud83d\ude00""#,
        r#""s":"a\u0000b""#,
        r#""\ud83d\ude00":1"#,
        r#""n":1e-310"#,
        r#""n":4.9e-324"#,
        r#""n":2.2250738585072014e-308"#,
        r#""n":1.7976931348623157e308"#,
        r#""n":1.7976931348623158e308"#,
        r#""n":1.7976931348623159e308"#,
        r#""n":1.797693134862316e308"#,
        r#""n":2e308"#,
        r#""n":1e309"#,
        r#""n":-1e-400"#,
        r#""n":1E-324"#,
        r#""n":1e-1000"#,
    ] {
        hs.push(format!(r#"{{"type":"hello","key":"{invalid}",{field}}}"#));
        hs.push(format!(r#"{{"type":"hello","key":"{key}",{field}}}"#));
    }
    hs.push(format!(
        r#"{{"type":"hello","key":"{invalid}","n":{}e-1100}}"#,
        "9".repeat(1_100)
    ));
    hs.push(format!(
        r#"{{"type":"hello","key":"{invalid}","n":0.{}1}}"#,
        "0".repeat(1_100)
    ));
    hs.push(format!(
        r#"{{"type":"hello","key":"{invalid}","n":{}.5e-3}}"#,
        "1".repeat(1_100)
    ));
    // Escaped field names and values, and a first duplicate that is not a string.
    for text in [
        format!(r#"{{"t\u0079pe":"hello","key":"{key}"}}"#),
        format!(r#"{{"t\u0079pe":"hello","k\u0065y":"{invalid}"}}"#),
        format!(r#"{{"type":"hell\u006f","key":"{key}"}}"#),
        format!(r#"{{"type":"hell\u006f","key":"{invalid}"}}"#),
        format!(r#"{{"type":"e2ee\u005fhello","key":"{invalid}"}}"#),
        format!(r#"{{"type":{{"a":1}},"type":"hello","key":"{invalid}"}}"#),
        format!(r#"{{"type":"hello","type":{{"a":1}},"key":"{invalid}"}}"#),
        format!(r#"{{"type":null,"type":"hello","key":"{invalid}"}}"#),
        format!(r#"{{"type":[],"type":"hello","key":"{invalid}"}}"#),
        format!(r#"{{"key":{{"a":1}},"key":"{key}","type":"hello"}}"#),
        format!(r#"{{"key":"{invalid}","type":"hello","key":"{key}"}}"#),
        format!(r#"{{"\ud83d\ude00":"x","type":"hello","key":"{invalid}"}}"#),
        format!("{{\"😀\":1,\"type\":\"hello\",\"key\":\"{invalid}\"}}"),
    ] {
        hs.push(text);
    }
    let ping = [
        r#"{"t\u0079pe":"ping"}"#,
        r#"{"type":"pi\u006eg"}"#,
        r#"{"type":{"x":1},"type":"ping"}"#,
        r#"{"type":"ping","type":{"x":1}}"#,
        r#"{"type":null,"type":"ping"}"#,
        r#"{"s":"\ud83d\ude00","type":"ping"}"#,
        "{\"s\":\"😀\",\"type\":\"ping\"}",
        r#"{"type":"ping","s":"a\u0000b"}"#,
        r#"{"type":"ping","n":1e-310}"#,
        r#"{"type":"ping","n":1.7976931348623159e308}"#,
        r#"{"type":"ping","n":1.797693134862316e308}"#,
        r#"{"type":"ping","n":2e308}"#,
        r#"{"type":"ping","n":-1e-400}"#,
    ];
    (
        hs.into_iter().map(String::into_bytes).collect(),
        ping.iter().map(|text| text.as_bytes().to_vec()).collect(),
    )
}

#[test]
#[ignore = "regenerates the committed extra corpus fixture"]
fn write_extra_corpus() {
    let output = std::env::var("SPOCKY_RELAY_CORPUS_OUT").unwrap();
    let mut rng = Rng(0x00c0_ffee_0b57_ac1e);
    let mut corpus = String::new();
    let (hs, ping) = extra_json_cases();
    for payload in &hs {
        writeln!(corpus, "hs\t{}", b64(payload)).unwrap();
    }
    for payload in &ping {
        writeln!(corpus, "ping\t{}", b64(payload)).unwrap();
    }
    // Maps past 32 keys: every id length class the hash loop treats differently.
    for (shape, counts) in [
        ("conn21", vec![33, 34, 40, 64, 100]),
        ("16", vec![33, 40, 64]),
        ("12", vec![33, 40]),
        ("13", vec![33, 40]),
        ("14", vec![33, 40]),
        ("15", vec![33, 40]),
        ("17", vec![33, 40]),
        ("24", vec![33, 40]),
        ("31", vec![33, 40]),
        ("32", vec![33, 40, 64]),
        ("33", vec![33, 40]),
        ("48", vec![33, 40]),
        ("255", vec![33, 40]),
        ("nonascii", vec![33, 40, 64]),
        ("mixed", vec![33, 34, 40, 64, 100]),
    ] {
        for count in counts {
            for _ in 0..2 {
                let ids = id_set(&mut rng, shape, count);
                writeln!(corpus, "sync\t{}", ids_field(&ids)).unwrap();
            }
        }
    }
    // Trie depth: keys that share many low hash bits sit deep in the trie.
    for (prefix, bits) in [("a", 32), ("b", 32), ("c", 36), ("d", 40)] {
        let (first, second) = near_collision(prefix, bits);
        let mut ids = id_set(&mut rng, "mixed", 40);
        ids.push(first);
        ids.push(second);
        writeln!(corpus, "sync\t{}", ids_field(&ids)).unwrap();
    }
    // Disconnects: a map that grew past 32 keys and shrinks, as the relay's Owner removes
    // clients. The remaining count crosses 33, 32 and 31.
    for (shape, total) in [
        ("conn21", 40),
        ("mixed", 40),
        ("16", 36),
        ("nonascii", 34),
        ("33", 40),
    ] {
        let ids = id_set(&mut rng, shape, total);
        for remaining in [total - 1, 33, 32, 31, 20, 1, 0] {
            if remaining >= total {
                continue;
            }
            let mut order: Vec<usize> = (0..total).collect();
            for index in (1..total).rev() {
                order.swap(index, rng.below(index + 1));
            }
            let deleted: Vec<Vec<u8>> = order[..total - remaining]
                .iter()
                .map(|index| ids[*index].clone())
                .collect();
            writeln!(
                corpus,
                "syncdel\t{}\t{}",
                ids_field(&ids),
                ids_field(&deleted)
            )
            .unwrap();
        }
    }
    fs::write(output, corpus).unwrap();
}
