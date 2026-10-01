//! JavaScript operators the baseline applies to plain values: truthiness,
//! object spread, and `String(value)`, over [`JsValue`].

use spocky_store::js_value::{JsObject, JsValue, js_number, js_text_from_utf16, js_text_utf16};

/// JavaScript truthiness; a missing value is `undefined`.
#[must_use]
pub fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => false,
        Some(JsValue::Bool(flag)) => *flag,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
        Some(JsValue::Array(_) | JsValue::Object(_)) => true,
    }
}

/// `{ ...into, ...value }`: copies the own enumerable properties of `value`.
/// A string spreads one property per UTF-16 code unit and an array one per
/// element; other primitives spread nothing.
pub fn spread_into(into: &mut JsObject, value: Option<&JsValue>) {
    match value {
        Some(JsValue::Object(object)) => {
            for (key, item) in object.iter() {
                into.insert(key, item.clone());
            }
        }
        Some(JsValue::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                into.insert(index.to_string(), item.clone());
            }
        }
        Some(JsValue::String(text)) => {
            for (index, unit) in js_text_utf16(text).enumerate() {
                into.insert(
                    index.to_string(),
                    JsValue::String(js_text_from_utf16(&[unit])),
                );
            }
        }
        _ => {}
    }
}

/// `{ ...value }`.
#[must_use]
pub fn spread(value: Option<&JsValue>) -> JsObject {
    let mut object = JsObject::new();
    spread_into(&mut object, value);
    object
}

/// `String(value)`, as template literals use it; a missing value is
/// `undefined`.
#[must_use]
pub fn js_string(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".to_owned(),
        Some(JsValue::Null) => "null".to_owned(),
        Some(JsValue::Bool(flag)) => flag.to_string(),
        Some(JsValue::Number(number)) => {
            if number.is_nan() {
                "NaN".to_owned()
            } else if number.is_infinite() {
                if *number > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                }
                .to_owned()
            } else {
                js_number(*number)
            }
        }
        Some(JsValue::String(text)) => text.clone(),
        Some(JsValue::Array(items)) => items
            .iter()
            .map(|item| match item {
                JsValue::Undefined | JsValue::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(JsValue::Object(_)) => "[object Object]".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{js_string, spread, truthy};
    use spocky_store::js_value::{JsValue, parse, stringify};

    #[test]
    fn spread_and_string_follow_javascript() {
        let value = |text: &str| parse(text).expect("JSON");
        // node: JSON.stringify({ ..."a😀" }) and JSON.stringify({ ...[1, null] })
        let text = JsValue::String("a😀".to_owned());
        assert_eq!(
            stringify(&JsValue::Object(spread(Some(&text)))),
            r#"{"0":"a","1":"\ud83d","2":"\ude00"}"#
        );
        assert_eq!(
            stringify(&JsValue::Object(spread(Some(&value("[1,null]"))))),
            r#"{"0":1,"1":null}"#
        );
        assert_eq!(
            stringify(&JsValue::Object(spread(Some(&value(r#"{"b":1,"2":0}"#))))),
            r#"{"2":0,"b":1}"#
        );
        assert!(spread(Some(&value("7"))).is_empty());
        assert!(spread(None).is_empty());
        // node: String([1, null, [2, undefined]]) === "1,,2,"
        assert_eq!(js_string(Some(&value("[1,null,[2,null]]"))), "1,,2,");
        assert_eq!(js_string(Some(&value("{}"))), "[object Object]");
        assert_eq!(js_string(None), "undefined");
        assert!(!truthy(Some(&value("0"))) && truthy(Some(&value("[]"))));
    }
}
