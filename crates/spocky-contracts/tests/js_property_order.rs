//! JavaScript property order: array-index keys (canonical decimal below
//! 2^32 - 1) first and ascending, then the other keys in insertion order,
//! through every path that writes or lists an object: `JsObject`,
//! `JsRecord`, `stringify` and `stringify_pretty`, the serde wire writer, and
//! zod record output.
//!
//! Expected values were printed by node v22.20.0 for `v = JSON.parse(text)`:
//! `JSON.stringify(v)`, `JSON.stringify(v, null, 2)`, and `Object.keys(v)`.

use spocky_contracts::js_value::{parse, stringify, stringify_pretty};
use spocky_contracts::json::{JsRecord, JsonValue};
use spocky_contracts::zod::{Schema, Verdict, verdict};

struct Case {
    text: &'static str,
    json: &'static str,
    pretty: &'static str,
    keys: &'static [&'static str],
}

const CASES: [Case; 13] = [
    Case {
        text: "{\"10\":1,\"2\":2,\"a\":3,\"1\":4}",
        json: "{\"1\":4,\"2\":2,\"10\":1,\"a\":3}",
        pretty: "{\n  \"1\": 4,\n  \"2\": 2,\n  \"10\": 1,\n  \"a\": 3\n}",
        keys: &["1", "2", "10", "a"],
    },
    Case {
        text: "{\"b\":1,\"01\":2,\"1\":3,\"0\":4}",
        json: "{\"0\":4,\"1\":3,\"b\":1,\"01\":2}",
        pretty: "{\n  \"0\": 4,\n  \"1\": 3,\n  \"b\": 1,\n  \"01\": 2\n}",
        keys: &["0", "1", "b", "01"],
    },
    Case {
        text: "{\"4294967295\":1,\"4294967294\":2,\"4294967296\":3,\"0\":4}",
        json: "{\"0\":4,\"4294967294\":2,\"4294967295\":1,\"4294967296\":3}",
        pretty: "{\n  \"0\": 4,\n  \"4294967294\": 2,\n  \"4294967295\": 1,\n  \"4294967296\": 3\n}",
        keys: &["0", "4294967294", "4294967295", "4294967296"],
    },
    Case {
        text: "{\"-0\":1,\"1.0\":2,\"1\":3,\"-1\":4,\"+1\":5,\" 1\":6,\"1e3\":7,\"1000\":8}",
        json: "{\"1\":3,\"1000\":8,\"-0\":1,\"1.0\":2,\"-1\":4,\"+1\":5,\" 1\":6,\"1e3\":7}",
        pretty: "{\n  \"1\": 3,\n  \"1000\": 8,\n  \"-0\": 1,\n  \"1.0\": 2,\n  \"-1\": 4,\n  \"+1\": 5,\n  \" 1\": 6,\n  \"1e3\": 7\n}",
        keys: &["1", "1000", "-0", "1.0", "-1", "+1", " 1", "1e3"],
    },
    Case {
        text: "{\"a\":1,\"10\":2,\"b\":3,\"9\":4,\"c\":5,\"100\":6}",
        json: "{\"9\":4,\"10\":2,\"100\":6,\"a\":1,\"b\":3,\"c\":5}",
        pretty: "{\n  \"9\": 4,\n  \"10\": 2,\n  \"100\": 6,\n  \"a\": 1,\n  \"b\": 3,\n  \"c\": 5\n}",
        keys: &["9", "10", "100", "a", "b", "c"],
    },
    Case {
        text: "{\"x\":{\"10\":1,\"2\":2,\"y\":3},\"9\":[{\"3\":1,\"1\":2}]}",
        json: "{\"9\":[{\"1\":2,\"3\":1}],\"x\":{\"2\":2,\"10\":1,\"y\":3}}",
        pretty: "{\n  \"9\": [\n    {\n      \"1\": 2,\n      \"3\": 1\n    }\n  ],\n  \"x\": {\n    \"2\": 2,\n    \"10\": 1,\n    \"y\": 3\n  }\n}",
        keys: &["9", "x"],
    },
    Case {
        text: "{\"2\":1,\"2\":2,\"1\":3,\"1\":4}",
        json: "{\"1\":4,\"2\":2}",
        pretty: "{\n  \"1\": 4,\n  \"2\": 2\n}",
        keys: &["1", "2"],
    },
    Case {
        text: "{\"__proto__\":1,\"5\":2,\"constructor\":3,\"4\":4}",
        json: "{\"4\":4,\"5\":2,\"__proto__\":1,\"constructor\":3}",
        pretty: "{\n  \"4\": 4,\n  \"5\": 2,\n  \"__proto__\": 1,\n  \"constructor\": 3\n}",
        keys: &["4", "5", "__proto__", "constructor"],
    },
    Case {
        text: "{\"b\":1,\"4294967295\":2,\"1\":3}",
        json: "{\"1\":3,\"b\":1,\"4294967295\":2}",
        pretty: "{\n  \"1\": 3,\n  \"b\": 1,\n  \"4294967295\": 2\n}",
        keys: &["1", "b", "4294967295"],
    },
    Case {
        text: "{\"4294967296\":1,\"4294967295\":2,\"c\":3}",
        json: "{\"4294967296\":1,\"4294967295\":2,\"c\":3}",
        pretty: "{\n  \"4294967296\": 1,\n  \"4294967295\": 2,\n  \"c\": 3\n}",
        keys: &["4294967296", "4294967295", "c"],
    },
    Case {
        text: "{\"c\":1,\"4294967294\":2,\"4294967295\":3,\"4294967293\":4}",
        json: "{\"4294967293\":4,\"4294967294\":2,\"c\":1,\"4294967295\":3}",
        pretty: "{\n  \"4294967293\": 4,\n  \"4294967294\": 2,\n  \"c\": 1,\n  \"4294967295\": 3\n}",
        keys: &["4294967293", "4294967294", "c", "4294967295"],
    },
    Case {
        text: "{\"007\":1,\"7\":2,\"08\":3,\"8\":4}",
        json: "{\"7\":2,\"8\":4,\"007\":1,\"08\":3}",
        pretty: "{\n  \"7\": 2,\n  \"8\": 4,\n  \"007\": 1,\n  \"08\": 3\n}",
        keys: &["7", "8", "007", "08"],
    },
    Case {
        text: "{\"9007199254740993\":1,\"4294967293\":2,\"4294967292\":3}",
        json: "{\"4294967292\":3,\"4294967293\":2,\"9007199254740993\":1}",
        pretty: "{\n  \"4294967292\": 3,\n  \"4294967293\": 2,\n  \"9007199254740993\": 1\n}",
        keys: &["4294967292", "4294967293", "9007199254740993"],
    },
];

#[test]
fn objects_list_and_write_keys_in_property_order() {
    for Case {
        text,
        json,
        pretty,
        keys,
    } in CASES
    {
        let value = parse(text).expect(text);
        let listed: Vec<&str> = value
            .as_object()
            .expect("object")
            .iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(listed, keys, "{text}");
        assert_eq!(stringify(&value), json, "{text}");
        assert_eq!(stringify_pretty(&value), pretty, "{text}");
        // And again from the written text.
        assert_eq!(stringify(&parse(json).expect(json)), json, "{text}");
    }
}

#[test]
fn json_records_list_and_serialize_keys_in_property_order() {
    for Case {
        text, json, keys, ..
    } in CASES
    {
        // A record drops an own `__proto__` key, as zod does.
        if text.contains("__proto__") {
            continue;
        }
        let record: JsRecord<JsonValue> = serde_json::from_str(text).expect(text);
        let listed: Vec<&str> = record.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(listed, keys, "{text}");
        assert_eq!(
            serde_json::to_string(&record).expect("serialize"),
            json,
            "{text}"
        );
    }
}

#[test]
fn wire_values_and_zod_records_follow_property_order() {
    let schema = Schema::Record(
        Box::new(Schema::String(Vec::new())),
        Box::new(Schema::Unknown),
    );
    for Case { text, json, .. } in CASES {
        let value = parse(text).expect(text);
        let wire = serde_json::to_string(&JsonValue(value.clone())).expect("serialize");
        assert_eq!(wire, json, "{text}");
        if text.contains("__proto__") {
            continue;
        }
        let Verdict::Valid(output) = verdict(&schema, &value) else {
            panic!("a record accepts {text}");
        };
        assert_eq!(stringify(&output), json, "{text}");
    }
}
