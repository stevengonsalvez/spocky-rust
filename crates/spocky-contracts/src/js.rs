//! JavaScript operators the baseline applies to plain values: truthiness,
//! object spread, and `String(value)`, over [`JsValue`].

use std::fmt::{self, Display, Formatter};

use crate::js_value::{
    JsObject, JsValue, js_number, js_text_eq, js_text_from_utf16, js_text_utf16,
};

/// A JavaScript `TypeError` carrying V8's exact message, such as
/// `Cannot read properties of undefined (reading 'type')`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsTypeError(pub String);

impl Display for JsTypeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JsTypeError {}

/// JavaScript strict equality (`===`). A missing value is `undefined`.
/// Numbers compare as doubles, so `NaN !== NaN` and `0 === -0`; strings
/// compare by UTF-16 code units, which the `js_value` text encoding keeps
/// canonical. Objects and arrays have identity: each equals only itself,
/// so two separately built objects with the same contents differ.
#[must_use]
pub fn strict_equals(left: Option<&JsValue>, right: Option<&JsValue>) -> bool {
    fn defined(value: Option<&JsValue>) -> Option<&JsValue> {
        value.filter(|value| !matches!(value, JsValue::Undefined))
    }
    match (defined(left), defined(right)) {
        (None, None) | (Some(JsValue::Null), Some(JsValue::Null)) => true,
        (Some(JsValue::Bool(a)), Some(JsValue::Bool(b))) => a == b,
        #[allow(clippy::float_cmp, reason = "JavaScript === on numbers")]
        (Some(JsValue::Number(a)), Some(JsValue::Number(b))) => a == b,
        (Some(JsValue::String(a)), Some(JsValue::String(b))) => js_text_eq(a, b),
        (Some(a @ (JsValue::Array(_) | JsValue::Object(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

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
///
/// The result is JavaScript text in the [`crate::js_value`] encoding, not
/// UTF-8: it is the value of a JavaScript string, so it can go back into a
/// [`JsValue::String`] or be concatenated with other such text. A literal
/// U+10FFFF is doubled in it, and a lone surrogate is escaped. Text that
/// leaves the value domain (a path, a process argument, a log line) goes
/// through [`crate::js_value::js_text_to_utf8`] first.
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
    use super::{JsTypeError, js_string, spread, strict_equals, truthy};
    use crate::js_value::{JsValue, parse, stringify};

    // Expected values printed by node v22.20.0, for example `NaN === NaN`.
    #[test]
    fn strict_equals_follows_javascript() {
        let parsed = parse(r#"[null, 1.5, "a", "\ud800", true, {}, [], 0]"#).unwrap();
        let items = parsed.as_array().unwrap();
        let (null, number, text, lone, flag, object, array, zero) = (
            &items[0], &items[1], &items[2], &items[3], &items[4], &items[5], &items[6], &items[7],
        );
        let nan = JsValue::Number(f64::NAN);
        let negative_zero = JsValue::Number(-0.0);
        let infinity = JsValue::Number(f64::INFINITY);
        let undefined = JsValue::Undefined;

        assert!(!strict_equals(Some(&nan), Some(&nan)), "NaN === NaN");
        assert!(strict_equals(Some(zero), Some(&negative_zero)), "0 === -0");
        assert!(strict_equals(
            Some(&infinity),
            Some(&JsValue::Number(f64::INFINITY))
        ));
        assert!(
            !strict_equals(Some(null), Some(&undefined)),
            "null === undefined"
        );
        assert!(!strict_equals(Some(null), None), "null === missing");
        assert!(
            strict_equals(Some(&undefined), None),
            "undefined === missing"
        );
        assert!(strict_equals(None, None));
        assert!(strict_equals(Some(null), Some(null)));
        assert!(strict_equals(Some(number), Some(&JsValue::Number(1.5))));
        assert!(strict_equals(
            Some(text),
            Some(&JsValue::String("a".to_owned()))
        ));
        assert!(strict_equals(Some(lone), Some(lone)), "lone surrogate text");
        assert!(
            !strict_equals(Some(flag), Some(&JsValue::Number(1.0))),
            "true === 1"
        );
        assert!(
            !strict_equals(Some(text), Some(&JsValue::Number(1.0))),
            "\"a\" === 1"
        );
        assert!(strict_equals(Some(object), Some(object)), "o === o");
        assert!(strict_equals(Some(array), Some(array)), "a === a");
        let other_object = parse("{}").unwrap();
        let other_array = parse("[]").unwrap();
        assert!(
            !strict_equals(Some(object), Some(&other_object)),
            "{{}} === {{}}"
        );
        assert!(!strict_equals(Some(array), Some(&other_array)), "[] === []");
        let copy = object.clone();
        assert!(
            !strict_equals(Some(object), Some(&copy)),
            "a clone is a new object"
        );
    }

    #[test]
    fn type_error_displays_its_v8_message() {
        let message = "Cannot read properties of undefined (reading 'type')";
        let error = JsTypeError(message.to_owned());
        assert_eq!(error.to_string(), message);
        let boxed: Box<dyn std::error::Error> = Box::new(error);
        assert_eq!(boxed.to_string(), message);
        assert!(boxed.source().is_none());
    }

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
