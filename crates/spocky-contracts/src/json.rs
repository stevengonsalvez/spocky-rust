//! Opaque JSON with JavaScript object semantics.
//!
//! Paseo holds `z.unknown()`, `z.json()`, and `z.record()` values as plain
//! JavaScript objects. Two engine rules then shape `JSON.stringify` output:
//!
//! - Property order: array-index keys (`"0"` to `"4294967294"` in canonical
//!   form) come first in ascending numeric order, then every other key in
//!   insertion order. `JSON.parse('{"b":1,"2":1}')` writes `{"2":1,"b":1}`.
//! - A repeated key keeps its first position and takes its last value.
//!
//! Numbers are doubles written with `Number.prototype.toString`.

use std::fmt;

use indexmap::IndexMap;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::number::JsNumber;

/// Returns the numeric value of `key` when it is an ECMAScript array index.
#[must_use]
pub fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

/// Orders `keys` the way a JavaScript engine enumerates own properties.
fn js_key_order<'a, I: Iterator<Item = &'a String>>(keys: I) -> Vec<&'a String> {
    let mut indexed: Vec<(u32, &String)> = Vec::new();
    let mut named: Vec<&String> = Vec::new();
    for key in keys {
        match array_index(key) {
            Some(index) => indexed.push((index, key)),
            None => named.push(key),
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key)| key)
        .chain(named)
        .collect()
}

/// Any JSON value, written as `JSON.stringify` writes the parsed object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsonValue(pub Value);

impl JsonValue {
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

struct JsValueRef<'a>(&'a Value);

impl Serialize for JsValueRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(flag) => serializer.serialize_bool(*flag),
            Value::Number(number) => {
                let value = number.as_f64().and_then(JsNumber::new).ok_or_else(|| {
                    serde::ser::Error::custom("JSON numbers must be finite doubles")
                })?;
                value.serialize(serializer)
            }
            Value::String(text) => serializer.serialize_str(text),
            Value::Array(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(&JsValueRef(item))?;
                }
                sequence.end()
            }
            Value::Object(object) => serialize_js_object(object, serializer),
        }
    }
}

fn serialize_js_object<S: Serializer>(
    object: &Map<String, Value>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(object.len()))?;
    for key in js_key_order(object.keys()) {
        map.serialize_entry(key, &JsValueRef(&object[key]))?;
    }
    map.end()
}

impl Serialize for JsonValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        JsValueRef(&self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // serde_json's order-preserving map keeps the first position and the
        // last value for a repeated key, as JSON.parse does.
        Value::deserialize(deserializer).map(Self)
    }
}

/// `z.record(z.string(), V)`: string keys in JavaScript property order.
#[derive(Debug, Clone, PartialEq)]
pub struct JsRecord<V> {
    entries: IndexMap<String, V>,
}

impl<V> Default for JsRecord<V> {
    fn default() -> Self {
        Self {
            entries: IndexMap::new(),
        }
    }
}

impl<V> JsRecord<V> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns `record[key] = value` with JavaScript semantics: an existing key
    /// keeps its position.
    pub fn insert(&mut self, key: String, value: V) {
        self.entries.insert(key, value);
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.get(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Array-index entries in ascending numeric order.
    pub fn index_entries(&self) -> impl Iterator<Item = (&String, &V)> {
        let mut indexed: Vec<(u32, &String, &V)> = self
            .entries
            .iter()
            .filter_map(|(key, value)| array_index(key).map(|index| (index, key, value)))
            .collect();
        indexed.sort_by_key(|(index, _, _)| *index);
        indexed.into_iter().map(|(_, key, value)| (key, value))
    }

    /// Non-index entries in insertion order.
    pub fn named_entries(&self) -> impl Iterator<Item = (&String, &V)> {
        self.entries
            .iter()
            .filter(|(key, _)| array_index(key).is_none())
    }

    /// Entries in JavaScript enumeration order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.index_entries().chain(self.named_entries())
    }
}

impl<V: Serialize> Serialize for JsRecord<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for JsRecord<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RecordVisitor<V>(std::marker::PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for RecordVisitor<V> {
            type Value = JsRecord<V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<JsRecord<V>, A::Error> {
                let mut record = JsRecord::new();
                while let Some((key, value)) = access.next_entry::<String, V>()? {
                    record.insert(key, value);
                }
                Ok(record)
            }
        }

        deserializer.deserialize_map(RecordVisitor(std::marker::PhantomData))
    }
}

struct Entries<'a>(Vec<(&'a String, &'a JsonValue)>);

impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Writes a zod `.passthrough()` object as zod 4 builds it: shape keys are
/// assigned first, then unknown keys in input order, and the engine then
/// enumerates array-index keys before all others.
///
/// # Errors
///
/// Propagates serializer errors, including a non-finite number.
pub fn serialize_passthrough<S: Serializer, K: Serialize>(
    known: &K,
    extra: &JsRecord<JsonValue>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Ordered<'a, K> {
        #[serde(flatten)]
        index: Entries<'a>,
        #[serde(flatten)]
        known: &'a K,
        #[serde(flatten)]
        named: Entries<'a>,
    }

    Ordered {
        index: Entries(extra.index_entries().collect()),
        known,
        named: Entries(extra.named_entries().collect()),
    }
    .serialize(serializer)
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::{JsRecord, JsonValue, array_index, serialize_passthrough};

    #[test]
    fn passthrough_puts_index_keys_first_and_named_extras_last() {
        #[derive(Serialize)]
        struct Known {
            a: bool,
            b: bool,
        }
        struct Probe(Known, JsRecord<JsonValue>);
        impl Serialize for Probe {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serialize_passthrough(&self.0, &self.1, serializer)
            }
        }
        let extra: JsRecord<JsonValue> =
            serde_json::from_str(r#"{"z":1.0,"5":-0,"x":1e21,"0":true}"#).unwrap();
        let text = serde_json::to_string(&Probe(Known { a: true, b: false }, extra)).unwrap();
        assert_eq!(
            text,
            r#"{"0":true,"5":0,"a":true,"b":false,"z":1,"x":1e+21}"#
        );
    }

    #[test]
    fn array_index_is_canonical_u32_below_max() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("4294967294"), Some(4_294_967_294));
        for key in ["4294967295", "01", "-1", "", "1.0", " 1", "a"] {
            assert_eq!(array_index(key), None, "{key:?}");
        }
    }

    #[test]
    fn value_writes_like_json_stringify_of_json_parse() {
        // Expected text captured from node v22.20.0:
        // JSON.stringify(JSON.parse(input)).
        let input = r#"{"b":1.0,"10":[1e21,0.0000001],"2":{"z":null,"1":true},"01":-0,"b":"last","4294967295":1}"#;
        let expected =
            r#"{"2":{"1":true,"z":null},"10":[1e+21,1e-7],"b":"last","01":0,"4294967295":1}"#;
        let parsed: JsonValue = serde_json::from_str(input).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), expected);
    }

    #[test]
    fn record_keeps_first_position_and_last_value() {
        let parsed: JsRecord<String> =
            serde_json::from_str(r#"{"x":"1","7":"2","x":"3"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"7":"2","x":"3"}"#
        );
        assert!(serde_json::from_str::<JsRecord<String>>(r#"{"x":1}"#).is_err());
    }
}
