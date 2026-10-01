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
    entries: Vec<(String, V)>,
}

impl<V> Default for JsRecord<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
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
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|(existing, _)| *existing == key)
        {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in JavaScript enumeration order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        let keys: Vec<&String> = self.entries.iter().map(|(key, _)| key).collect();
        js_key_order(keys.into_iter())
            .into_iter()
            .filter_map(|key| self.get(key).map(|value| (key, value)))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<V> FromIterator<(String, V)> for JsRecord<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut record = Self::new();
        for (key, value) in iter {
            record.insert(key, value);
        }
        record
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

#[cfg(test)]
mod tests {
    use super::{JsRecord, JsonValue, array_index};

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
