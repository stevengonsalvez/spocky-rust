//! PTY output decoding: fixed cases and a differential against the pinned
//! Node `StringDecoder("utf8")` over seeded chunkings of valid, truncated,
//! overlong, surrogate, and stray continuation bytes. Every `write` result
//! and the final `end` must match.

mod support;

use std::fmt::Write as _;

use spocky_contracts::js_value::{JsValue, stringify};
use spocky_terminal::utf8_decoder::Utf8Decoder;

#[test]
fn holds_a_character_split_across_chunks() {
    let mut decoder = Utf8Decoder::new();
    assert_eq!(decoder.write(&[0xe4, 0xb8]), "");
    assert_eq!(decoder.write(&[0xad, 0x41]), "\u{4e2d}A");
    assert_eq!(decoder.write(&[0xf0, 0x9f]), "");
    assert_eq!(decoder.write(&[0x98]), "");
    assert_eq!(decoder.write(&[0x80]), "\u{1f600}");
    assert_eq!(decoder.end(), "");
}

#[test]
fn a_non_continuation_byte_ends_the_held_character() {
    let mut decoder = Utf8Decoder::new();
    assert_eq!(decoder.write(&[0xe4, 0xb8]), "");
    assert_eq!(decoder.write(&[0x41]), "\u{fffd}A");
    assert_eq!(decoder.write(&[0xf0, 0x9f, 0x98]), "");
    assert_eq!(decoder.end(), "\u{fffd}");
}

const NODE_SCRIPT: &str = r#"
const [, chunksJson] = process.argv.slice(1);
const { StringDecoder } = await import("node:string_decoder");
const out = [];
for (const chunks of JSON.parse(chunksJson)) {
  const decoder = new StringDecoder("utf8");
  const results = chunks.map((hex) => decoder.write(Buffer.from(hex, "hex")));
  results.push(decoder.end());
  out.push(results);
}
process.stdout.write(JSON.stringify(out));
"#;

const PIECES: &[&[u8]] = &[
    b"a",
    b"\r\n",
    &[0xc3, 0xa9],
    &[0xe4, 0xb8, 0xad],
    &[0xf0, 0x9f, 0x98, 0x80],
    &[0xff],
    &[0x80],
    &[0xbf, 0xbf],
    &[0xc0, 0xaf],
    &[0xed, 0xa0, 0x80],
    &[0xf4, 0x90, 0x80, 0x80],
    &[0xe4, 0xb8],
    &[0xf0, 0x9f, 0x98],
    &[0xc3],
    &[0xf8, 0x88, 0x80, 0x80, 0x80],
    &[0x1b, 0x5b, 0x33, 0x31, 0x6d],
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// 300 streams, each the concatenation of random pieces cut at random points.
fn generated_streams() -> Vec<Vec<Vec<u8>>> {
    let mut state: u64 = 0x00de_c0de;
    let mut next = |bound: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((state >> 33) % u64::try_from(bound).expect("bound")).expect("index")
    };
    (0..300)
        .map(|_| {
            let mut stream = Vec::new();
            for _ in 0..=next(12) {
                stream.extend_from_slice(PIECES[next(PIECES.len())]);
            }
            let mut chunks = Vec::new();
            let mut rest = stream.as_slice();
            while !rest.is_empty() {
                let take = 1 + next(rest.len().min(6));
                chunks.push(rest[..take].to_vec());
                rest = &rest[take..];
            }
            chunks
        })
        .collect()
}

#[test]
fn decoding_matches_pinned_node_string_decoder() {
    let Some(pinned) = support::pinned("utf8 decoder differential") else {
        return;
    };
    let streams = generated_streams();
    let input = stringify(&JsValue::Array(
        streams
            .iter()
            .map(|chunks| JsValue::Array(chunks.iter().map(|c| JsValue::String(hex(c))).collect()))
            .collect(),
    ));
    let expected = support::run_node(&pinned, NODE_SCRIPT, &[&input]);
    let actual = stringify(&JsValue::Array(
        streams
            .iter()
            .map(|chunks| {
                let mut decoder = Utf8Decoder::new();
                let mut results: Vec<JsValue> = chunks
                    .iter()
                    .map(|chunk| JsValue::String(decoder.write(chunk)))
                    .collect();
                results.push(JsValue::String(decoder.end()));
                JsValue::Array(results)
            })
            .collect(),
    ));
    assert_eq!(actual, expected);
}
