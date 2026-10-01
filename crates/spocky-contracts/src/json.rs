//! Opaque JSON with JavaScript object semantics, and the bridge from
//! [`crate::js_value`] to serde.
//!
//! Paseo holds `z.unknown()`, `z.json()`, and `z.record()` values as plain
//! JavaScript objects. Two engine rules then shape `JSON.stringify` output:
//!
//! - Property order: array-index keys (`"0"` to `"4294967294"` in canonical
//!   form) come first in ascending numeric order, then every other key in
//!   insertion order. `JSON.parse('{"b":1,"2":1}')` writes `{"2":1,"b":1}`.
//! - A repeated key keeps its first position and takes its last value.
//!
//! Numbers are doubles written with `Number.prototype.toString`; strings are
//! JavaScript text and may hold lone surrogates (see [`crate::js_value`]).

use std::fmt;

use indexmap::IndexMap;
use serde::de::value::{Error as ValueError, MapDeserializer, SeqDeserializer, StrDeserializer};
use serde::de::{Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

pub use crate::js_value::array_index;

/// zod 4 drops an own `__proto__` property from `z.record`, `.passthrough()`
/// extras, and `z.json()` at any depth, without validating its value.
pub const PROTO_KEY: &str = "__proto__";
use crate::js_value::{JsObject, JsTextUnit, JsValue, js_text_units};
use crate::number::JsNumber;

/// Any JSON value, written as `JSON.stringify` writes the parsed object.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonValue(pub JsValue);

impl Default for JsonValue {
    fn default() -> Self {
        Self(JsValue::Null)
    }
}

impl JsonValue {
    #[must_use]
    pub fn as_value(&self) -> &JsValue {
        &self.0
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl From<JsValue> for JsonValue {
    fn from(value: JsValue) -> Self {
        Self(value)
    }
}

struct JsValueOut<'a>(&'a JsValue);

impl Serialize for JsValueOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            JsValue::Null => serializer.serialize_unit(),
            JsValue::Bool(flag) => serializer.serialize_bool(*flag),
            // JSON.stringify writes a non-finite number as null.
            JsValue::Number(number) => match JsNumber::new(*number) {
                Some(number) => number.serialize(serializer),
                None => serializer.serialize_unit(),
            },
            JsValue::String(text) => serializer.serialize_str(text),
            JsValue::Array(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(&JsValueOut(item))?;
                }
                sequence.end()
            }
            JsValue::Object(object) => {
                let mut map = serializer.serialize_map(Some(object.len()))?;
                for (key, value) in object.iter() {
                    map.serialize_entry(key, &JsValueOut(value))?;
                }
                map.end()
            }
        }
    }
}

impl Serialize for JsonValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        JsValueOut(&self.0).serialize(serializer)
    }
}

struct JsValueVisitor;

impl<'de> Visitor<'de> for JsValueVisitor {
    type Value = JsValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<JsValue, E> {
        Ok(JsValue::Bool(value))
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_i64<E>(self, value: i64) -> Result<JsValue, E> {
        Ok(JsValue::Number(value as f64))
    }

    #[allow(clippy::cast_precision_loss)]
    fn visit_u64<E>(self, value: u64) -> Result<JsValue, E> {
        Ok(JsValue::Number(value as f64))
    }

    fn visit_f64<E>(self, value: f64) -> Result<JsValue, E> {
        Ok(JsValue::Number(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<JsValue, E> {
        Ok(JsValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<JsValue, E> {
        Ok(JsValue::String(value))
    }

    fn visit_unit<E>(self) -> Result<JsValue, E> {
        Ok(STASH
            .with(|stash| stash.borrow_mut().take())
            .unwrap_or(JsValue::Null))
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<JsValue, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_none<E>(self) -> Result<JsValue, E> {
        Ok(JsValue::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<JsValue, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<JsValue, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = access.next_element::<JsonValue>()? {
            items.push(item.0);
        }
        Ok(JsValue::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<JsValue, A::Error> {
        let mut object = JsObject::new();
        while let Some((key, value)) = access.next_entry::<String, JsonValue>()? {
            object.insert(key, value.0);
        }
        Ok(JsValue::Object(object))
    }
}

/// Newtype name that lets [`JsValueDeserializer`] hand a [`JsonValue`] its
/// subtree as one clone. Walking a 100k-deep `JSON.parse` result through
/// serde visitors would overflow the stack.
const JSON_VALUE_TOKEN: &str = "$spocky_contracts::JsonValue";

thread_local! {
    static STASH: std::cell::RefCell<Option<JsValue>> = const { std::cell::RefCell::new(None) };
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A repeated key keeps its first position and its last value, as
        // JSON.parse does (`JsObject::insert`).
        deserializer
            .deserialize_newtype_struct(JSON_VALUE_TOKEN, JsValueVisitor)
            .map(Self)
    }
}

/// Splits a discriminated-union object into its tag and the whole object,
/// without serde's recursive buffering of internally tagged enums.
///
/// # Errors
///
/// Rejects a non-object or a missing or non-string tag, as zod does.
pub fn tagged_object<'de, D: Deserializer<'de>>(
    deserializer: D,
    tag: &'static str,
) -> Result<(String, JsValue), D::Error> {
    let JsonValue(value) = JsonValue::deserialize(deserializer)?;
    let name = value
        .as_object()
        .and_then(|object| object.get(tag))
        .and_then(JsValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| serde::de::Error::custom(format_args!("expected a string `{tag}`")))?;
    Ok((name, value))
}

/// Reads a zod `.passthrough()` object: `K` takes the shape keys and every
/// other key lands in the returned record, in JavaScript property order.
///
/// # Errors
///
/// Rejects a non-object or shape keys `K` does not accept.
pub fn split_passthrough<'de, D: Deserializer<'de>, K: serde::de::DeserializeOwned>(
    deserializer: D,
    shape_keys: &[&str],
) -> Result<(K, JsRecord<JsonValue>), D::Error> {
    let JsonValue(value) = JsonValue::deserialize(deserializer)?;
    let object = value
        .as_object()
        .ok_or_else(|| serde::de::Error::custom("expected an object"))?;
    let known = K::deserialize(JsValueDeserializer(&value)).map_err(serde::de::Error::custom)?;
    let extra = object
        .iter()
        .filter(|(key, _)| *key != PROTO_KEY && !shape_keys.contains(key))
        .map(|(key, item)| (key.to_owned(), JsonValue(item.clone())))
        .collect();
    Ok((known, extra))
}

/// Implements `Deserialize` for a zod discriminated union through
/// [`tagged_object`]. Each arm maps a tag to a function of the object's
/// [`JsValueDeserializer`].
macro_rules! deserialize_tagged {
    ($ty:ty, $tag:literal, { $($name:literal => $arm:expr),+ $(,)? }) => {
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let (name, value) = $crate::json::tagged_object(deserializer, $tag)?;
                let input = $crate::json::JsValueDeserializer(&value);
                let parsed: Result<Self, serde::de::value::Error> = match name.as_str() {
                    $($name => ($arm)(input),)+
                    other => Err(serde::de::Error::unknown_variant(other, &[$($name),+])),
                };
                parsed.map_err(serde::de::Error::custom)
            }
        }
    };
}

pub(crate) use deserialize_tagged;

/// `z.json()`: any JSON value whose numbers are all finite, with every own
/// `__proto__` key dropped unvalidated at any depth.
///
/// Divergence (DIV-001 family, `porting/inventory-summary.md`): pinned zod
/// 4.4.3 validates `z.json()` recursively and throws `RangeError` between
/// 1,000 and 5,000 levels on Node 22.20.0, at a stack-dependent depth. This
/// type validates iteratively and accepts any depth `JSON.parse` reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ZodJson(pub JsonValue);

/// Checks finiteness outside `__proto__` subtrees and reports whether any
/// `__proto__` key must be dropped.
fn inspect_zod_json(value: &JsValue) -> (bool, bool) {
    let mut has_proto = false;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            JsValue::Number(number) if !number.is_finite() => return (false, has_proto),
            JsValue::Array(items) => stack.extend(items.iter()),
            JsValue::Object(object) => {
                for (key, item) in object.iter() {
                    if key == PROTO_KEY {
                        has_proto = true;
                    } else {
                        stack.push(item);
                    }
                }
            }
            _ => {}
        }
    }
    (true, has_proto)
}

enum Rebuild<'a> {
    Visit(&'a JsValue),
    Array(usize),
    Object(Vec<&'a str>),
}

/// Copies `value` without `__proto__` keys, iteratively so any depth works.
fn without_proto(value: &JsValue) -> JsValue {
    let mut work = vec![Rebuild::Visit(value)];
    let mut built: Vec<JsValue> = Vec::new();
    while let Some(step) = work.pop() {
        match step {
            Rebuild::Visit(JsValue::Array(items)) => {
                work.push(Rebuild::Array(items.len()));
                work.extend(items.iter().rev().map(Rebuild::Visit));
            }
            Rebuild::Visit(JsValue::Object(object)) => {
                let entries: Vec<(&str, &JsValue)> =
                    object.iter().filter(|(key, _)| *key != PROTO_KEY).collect();
                work.push(Rebuild::Object(
                    entries.iter().map(|(key, _)| *key).collect(),
                ));
                work.extend(entries.iter().rev().map(|(_, item)| Rebuild::Visit(item)));
            }
            Rebuild::Visit(scalar) => built.push(scalar.clone()),
            Rebuild::Array(count) => {
                let items = built.split_off(built.len() - count);
                built.push(JsValue::Array(items));
            }
            Rebuild::Object(keys) => {
                let values = built.split_off(built.len() - keys.len());
                let mut object = JsObject::new();
                for (key, item) in keys.into_iter().zip(values) {
                    object.insert(key, item);
                }
                built.push(JsValue::Object(object));
            }
        }
    }
    built.pop().unwrap_or(JsValue::Null)
}

impl Serialize for ZodJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ZodJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = JsonValue::deserialize(deserializer)?;
        match inspect_zod_json(&value.0) {
            (false, _) => Err(serde::de::Error::custom(
                "expected JSON with finite numbers",
            )),
            (true, false) => Ok(Self(value)),
            (true, true) => Ok(Self(JsonValue(without_proto(&value.0)))),
        }
    }
}

/// A serde [`Deserializer`] over a parsed [`JsValue`], presenting object
/// keys in JavaScript enumeration order. Numbers arrive as `f64`.
#[derive(Debug, Clone, Copy)]
pub struct JsValueDeserializer<'a>(pub &'a JsValue);

impl IntoDeserializer<'_, ValueError> for JsValueDeserializer<'_> {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de> Deserializer<'de> for JsValueDeserializer<'_> {
    type Error = ValueError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ValueError> {
        match self.0 {
            JsValue::Null => visitor.visit_unit(),
            JsValue::Bool(flag) => visitor.visit_bool(*flag),
            JsValue::Number(number) => visitor.visit_f64(*number),
            JsValue::String(text) => visitor.visit_str(text),
            JsValue::Array(items) => {
                let mut access = SeqDeserializer::new(items.iter().map(JsValueDeserializer));
                let value = visitor.visit_seq(&mut access)?;
                access.end()?;
                Ok(value)
            }
            JsValue::Object(object) => {
                let mut access = MapDeserializer::new(
                    object
                        .iter()
                        .map(|(key, value)| (key, JsValueDeserializer(value))),
                );
                let value = visitor.visit_map(&mut access)?;
                access.end()?;
                Ok(value)
            }
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ValueError> {
        if self.0.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ValueError> {
        if name == JSON_VALUE_TOKEN {
            STASH.with(|stash| *stash.borrow_mut() = Some(self.0.clone()));
            return visitor.visit_unit();
        }
        visitor.visit_newtype_struct(self)
    }

    /// Skips an unknown key's value without walking it, as zod strips it.
    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ValueError> {
        visitor.visit_unit()
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ValueError> {
        match self.0 {
            JsValue::String(text) => {
                visitor.visit_enum(StrDeserializer::<ValueError>::new(text.as_str()))
            }
            _ => Err(serde::de::Error::custom(format_args!(
                "expected one of {variants:?} for {name}"
            ))),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier
    }
}

/// Rewrites `serde_json` output so lone surrogates held as JavaScript text
/// appear as `JSON.stringify` writes them: `\udXXX` escapes.
#[must_use]
pub fn js_wire_text(serialized: &str) -> String {
    let mut out = String::with_capacity(serialized.len());
    for unit in js_text_units(serialized) {
        match unit {
            JsTextUnit::Char(character) => out.push(character),
            JsTextUnit::LoneSurrogate(unit) => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// `z.record(z.string(), V)`: keys are JavaScript text (see
/// [`crate::text::JsText`]) in JavaScript property order. An
/// own `__proto__` key is dropped unvalidated, as zod 4 drops it.
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
                while let Some(key) = access.next_key::<String>()? {
                    if key == PROTO_KEY {
                        // zod 4 never validates or keeps an own `__proto__`.
                        access.next_value::<serde::de::IgnoredAny>()?;
                    } else {
                        record.insert(key, access.next_value::<V>()?);
                    }
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
    use serde::{Deserialize, Serialize};

    use super::{JsRecord, JsonValue, array_index, js_wire_text, serialize_passthrough};
    use crate::js_value::parse;

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
    fn js_value_bridge_keeps_lone_surrogates_and_writes_infinity_as_null() {
        // Expected text from node v22.20.0: JSON.stringify(JSON.parse(input)).
        let input = r#"{"s":"a\ud800b","n":1e400,"k\udfff":[-1e400,"\ud83d\ude00"]}"#;
        let expected = r#"{"s":"a\ud800b","n":null,"k\udfff":[null,"😀"]}"#;
        let parsed = parse(input).unwrap();
        let value = JsonValue::deserialize(super::JsValueDeserializer(&parsed)).unwrap();
        let written = js_wire_text(&serde_json::to_string(&value).unwrap());
        assert_eq!(written, expected);
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
