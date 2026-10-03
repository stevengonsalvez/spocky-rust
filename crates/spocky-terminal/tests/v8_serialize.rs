//! V8 structured clone of plain data against the pinned Node's
//! `v8.serialize` and `v8.deserialize`: the bytes for a corpus of values
//! (numbers around the Smi and double boundary, `-0`, `NaN`, infinities,
//! one-byte and two-byte strings with their alignment padding, lone
//! surrogates, index-like keys, `undefined` values, nested arrays and
//! objects) must be equal, and decoding what Node wrote, including forms the
//! encoder never writes (sparse arrays, shared references), must give the
//! value Node reads back.

mod support;

use std::fmt::Write as _;

use spocky_contracts::js_value::{JsObject, JsValue, js_text_from_utf16, parse, stringify};
use spocky_terminal::v8_serialize::{DecodeError, deserialize, serialize};

/// Each case is JSON; `{"$": ...}` objects stand for values JSON cannot
/// hold. Both sides build the values from this same text.
const CASES: &str = r#"[
  null, true, false, 0, {"$":"-0"}, 1, -1, 63, 64, 8191, 8192, 2147483647,
  2147483648, -2147483648, -2147483649, 1.5, -2.25, 1e21, 4294967296,
  {"$":"NaN"}, {"$":"Infinity"}, {"$":"-Infinity"}, {"$":"undefined"},
  "", "a", "é", "ÿ", "Ā", "中", "a中", "😀",
  {"$":"units","units":[55357]}, {"$":"units","units":[56832,97]},
  {"$":"repeat","text":"x","count":300}, {"$":"repeat","text":"中","count":300},
  {"$":"repeat","text":"y","count":127}, {"$":"repeat","text":"é","count":128},
  [], [1,2,3], [{"$":"undefined"}], [[]], [[1,[2,[3]]],"x",null],
  [1.5,{"$":"NaN"},{"$":"-0"}], ["中",1,"中"], ["a","中"],
  {}, {"a":1}, {"b":{"$":"undefined"},"a":null},
  {"1":1,"0":0,"b":2,"a":3,"10":4,"2":5},
  {"4294967294":1,"4294967295":2,"-1":3,"01":4,"1.5":5},
  {"ключ":"знач"},
  {"type":"terminalMessage","terminalId":"t","message":{"type":"output","data":"x\u001b[0m","revision":12}},
  {"cell":{"char":"a","fg":{"$":"undefined"},"bg":{"$":"undefined"},"bold":false}},
  {"state":{"rows":24,"cols":80,"grid":[[{"char":" "}]],"cursor":{"row":0,"col":0}}}
]"#;

/// Values only a decoder meets: Node writes these forms, the encoder does not.
const NODE_ONLY: &str = r#"
const holey = [1, , 3];
const shared = { a: 1 };
const list = [
  holey,
  new Array(4),
  [shared, shared],
  { x: shared, y: [shared] },
  [1, 2, 3, "x"],
];
list[3].extra = 1;
"#;

const NODE_SCRIPT: &str = r#"
import v8 from "node:v8";
const [, casesJson] = process.argv.slice(1);
const revive = (value) => {
  if (Array.isArray(value)) return value.map(revive);
  if (value !== null && typeof value === "object") {
    if ("$" in value) {
      switch (value.$) {
        case "-0": return -0;
        case "NaN": return NaN;
        case "Infinity": return Infinity;
        case "-Infinity": return -Infinity;
        case "undefined": return undefined;
        case "units": return String.fromCharCode(...value.units);
        case "repeat": return value.text.repeat(value.count);
      }
    }
    const object = {};
    for (const key of Object.keys(value)) object[key] = revive(value[key]);
    return object;
  }
  return value;
};
const cases = JSON.parse(casesJson).map(revive);
const encoded = cases.map((value) => v8.serialize(value).toString("hex"));
__NODE_ONLY__
const decodeOnly = list.map((value) => [v8.serialize(value).toString("hex"), JSON.stringify(value)]);
process.stdout.write(JSON.stringify({ encoded, decodeOnly }));
"#;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The case as the value Node builds from the same JSON.
fn revive(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(revive).collect()),
        JsValue::Object(object) => {
            if let Some(tag) = object.get("$").and_then(JsValue::as_str) {
                match tag {
                    "-0" => return JsValue::Number(-0.0),
                    "NaN" => return JsValue::Number(f64::NAN),
                    "Infinity" => return JsValue::Number(f64::INFINITY),
                    "-Infinity" => return JsValue::Number(f64::NEG_INFINITY),
                    "undefined" => return JsValue::Undefined,
                    "units" => {
                        let units: Vec<u16> = object
                            .get("units")
                            .and_then(JsValue::as_array)
                            .expect("units")
                            .iter()
                            .map(|unit| {
                                format!("{}", unit.as_f64().expect("unit"))
                                    .parse()
                                    .expect("u16")
                            })
                            .collect();
                        return JsValue::String(js_text_from_utf16(&units));
                    }
                    "repeat" => {
                        let text = object.get("text").and_then(JsValue::as_str).expect("text");
                        let count = format!(
                            "{}",
                            object
                                .get("count")
                                .and_then(JsValue::as_f64)
                                .expect("count")
                        )
                        .parse()
                        .expect("count");
                        return JsValue::String(text.repeat(count));
                    }
                    _ => {}
                }
            }
            let mut out = JsObject::new();
            for (key, value) in object.iter() {
                out.insert(key, revive(value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

#[test]
fn the_encoder_and_decoder_match_pinned_v8() {
    let Some(pinned) = support::pinned("v8 serialization differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let script = NODE_SCRIPT.replace("__NODE_ONLY__", NODE_ONLY);
    let output = support::run_node(&pinned, &script, &[CASES]);
    let reported = parse(&output).expect("node json");
    let cases = parse(CASES).expect("cases");
    let encoded = reported
        .get("encoded")
        .and_then(JsValue::as_array)
        .expect("encoded");
    let cases = cases.as_array().expect("array");
    assert_eq!(encoded.len(), cases.len());
    let mut failures = Vec::new();
    for (index, (case, expected)) in cases.iter().zip(encoded).enumerate() {
        let expected = expected.as_str().expect("hex");
        let actual = hex(&serialize(&revive(case)));
        if actual != expected {
            failures.push(format!(
                "case {index} {}: node {expected} rust {actual}",
                stringify(case)
            ));
        }
        // Reading Node's bytes and writing them again gives the bytes back.
        let decoded = deserialize(&unhex(expected)).expect("decode");
        let again = hex(&serialize(&decoded));
        if again != expected {
            failures.push(format!(
                "case {index} round trip: node {expected} again {again}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    for pair in reported
        .get("decodeOnly")
        .and_then(JsValue::as_array)
        .expect("decodeOnly")
    {
        let pair = pair.as_array().expect("pair");
        let bytes = unhex(pair[0].as_str().expect("hex"));
        let decoded = deserialize(&bytes).expect("decode node-only form");
        assert_eq!(stringify(&decoded), pair[1].as_str().expect("json"));
    }
}

#[test]
fn malformed_input_is_rejected() {
    assert_eq!(deserialize(&[]), Err(DecodeError::Truncated));
    assert_eq!(
        deserialize(&[0x01, 0x0f, b'0']),
        Err(DecodeError::BadHeader)
    );
    assert_eq!(deserialize(&[0xff, 0x0f]), Err(DecodeError::Truncated));
    assert_eq!(
        deserialize(&[0xff, 0x0f, b'0', b'0']),
        Err(DecodeError::TrailingBytes)
    );
    assert_eq!(
        deserialize(&[0xff, 0x0f, b'?']),
        Err(DecodeError::UnsupportedTag(b'?'))
    );
    assert_eq!(
        deserialize(&[0xff, 0x0f, b'"', 5, b'a']),
        Err(DecodeError::Truncated)
    );
    assert_eq!(
        deserialize(&[0xff, 0x0f, b'^', 0]),
        Err(DecodeError::BadReference)
    );
}
