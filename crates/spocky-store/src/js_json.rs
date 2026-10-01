//! JavaScript object semantics for parsed JSON.
//!
//! `JSON.parse` builds ordinary objects, and ordinary objects enumerate
//! array-index keys (`"0"` up to `"4294967294"`, canonical form) first in
//! ascending numeric order, then every other key in insertion order. A later
//! duplicate key replaces the value but keeps the first position, which
//! `serde_json` with `preserve_order` already does. Applying this order
//! reproduces what `JSON.stringify(JSON.parse(text))` emits.

use serde_json::{Map, Value};

/// Reorders every object in `value` into JavaScript property order.
#[must_use]
pub fn js_property_order(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(order_map(map)),
        Value::Array(items) => Value::Array(items.into_iter().map(js_property_order).collect()),
        other => other,
    }
}

fn order_map(map: Map<String, Value>) -> Map<String, Value> {
    if !map.keys().any(|key| array_index(key).is_some()) {
        return map
            .into_iter()
            .map(|(key, value)| (key, js_property_order(value)))
            .collect();
    }
    let mut indexed = Vec::new();
    let mut named = Vec::new();
    for (key, value) in map {
        match array_index(&key) {
            Some(index) => indexed.push((index, key, value)),
            None => named.push((key, value)),
        }
    }
    indexed.sort_by_key(|(index, _, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(named)
        .map(|(key, value)| (key, js_property_order(value)))
        .collect()
}

/// An ECMAScript array index: the canonical decimal form of an integer below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index < u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::js_property_order;

    #[test]
    fn matches_json_stringify_of_json_parse() {
        // node -e 'console.log(JSON.stringify(JSON.parse(text)))'
        let text = r#"{"b":1,"10":2,"a":{"2":0,"x":1,"1":2},"01":3,"2":4,"4294967295":5,"4294967294":6,"-1":7,"b":8}"#;
        let parsed = serde_json::from_str(text).expect("valid JSON");
        let rendered = serde_json::to_string(&js_property_order(parsed)).expect("render");
        assert_eq!(
            rendered,
            r#"{"2":4,"10":2,"4294967294":6,"b":8,"a":{"1":2,"2":0,"x":1},"01":3,"4294967295":5,"-1":7}"#
        );
    }
}
