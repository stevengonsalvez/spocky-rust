//! `partial-json.ts`: reads the complete prefix of a streamed tool input
//! object (`input_json_delta` fragments joined so far).
//!
//! The parser walks UTF-16 code units, as the baseline indexes JavaScript
//! strings, so `\u` escapes that produce lone surrogates survive. A member
//! named `__proto__` is assigned with `value[key] = member` in the baseline,
//! which runs the prototype setter instead of creating an own property; the
//! port drops the member the same way. Properties the baseline would then
//! inherit from that prototype are not modeled.

use spocky_contracts::js_value::{JsObject, JsValue, js_text_from_utf16, js_text_utf16};
use spocky_contracts::text::is_js_whitespace;

/// The parsed prefix and whether the whole input was one complete object.
#[derive(Debug, Clone, PartialEq)]
pub struct PartialJsonObject {
    pub value: JsObject,
    pub complete: bool,
}

struct Parsed<T> {
    value: T,
    next_index: usize,
    complete: bool,
}

const fn parsed<T>(value: T, next_index: usize, complete: bool) -> Parsed<T> {
    Parsed {
        value,
        next_index,
        complete,
    }
}

/// `/\s/u.test(input[index])` for one code unit.
fn is_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(is_js_whitespace)
}

fn skip_whitespace(input: &[u16], index: usize) -> usize {
    let mut current = index;
    while current < input.len() && is_space(input[current]) {
        current += 1;
    }
    current
}

fn unit_is(input: &[u16], index: usize, character: u8) -> bool {
    input.get(index) == Some(&u16::from(character))
}

fn starts_with(input: &[u16], index: usize, literal: &str) -> bool {
    let bytes = literal.as_bytes();
    input
        .get(index..index + bytes.len())
        .is_some_and(|slice| slice.iter().zip(bytes).all(|(a, b)| *a == u16::from(*b)))
}

fn is_digit(unit: Option<&u16>) -> bool {
    unit.is_some_and(|unit| (u16::from(b'0')..=u16::from(b'9')).contains(unit))
}

fn parse_string(input: &[u16], index: usize) -> Option<Parsed<Vec<u16>>> {
    if !unit_is(input, index, b'"') {
        return None;
    }
    let mut current = index + 1;
    let mut value: Vec<u16> = Vec::new();
    while current < input.len() {
        let character = input[current];
        if character == u16::from(b'"') {
            return Some(parsed(value, current + 1, true));
        }
        if character == u16::from(b'\\') {
            let Some(&escape) = input.get(current + 1) else {
                return Some(parsed(value, input.len(), false));
            };
            let simple = match u8::try_from(escape).unwrap_or(0) {
                b'"' => Some(u16::from(b'"')),
                b'\\' => Some(u16::from(b'\\')),
                b'/' => Some(u16::from(b'/')),
                b'b' => Some(0x08),
                b'f' => Some(0x0c),
                b'n' => Some(u16::from(b'\n')),
                b'r' => Some(u16::from(b'\r')),
                b't' => Some(u16::from(b'\t')),
                _ => None,
            };
            if let Some(unit) = simple {
                value.push(unit);
                current += 2;
                continue;
            }
            if escape == u16::from(b'u') {
                let hex = input.get(current + 2..current + 6);
                let code = hex.and_then(|hex| {
                    String::from_utf16(hex)
                        .ok()
                        .filter(|text| text.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        .and_then(|text| u16::from_str_radix(&text, 16).ok())
                });
                let Some(code) = code else {
                    return Some(parsed(value, input.len(), false));
                };
                value.push(code);
                current += 6;
                continue;
            }
            return Some(parsed(value, input.len(), false));
        }
        value.push(character);
        current += 1;
    }
    Some(parsed(value, input.len(), false))
}

fn parse_literal(input: &[u16], index: usize) -> Option<Parsed<JsValue>> {
    if starts_with(input, index, "true") {
        return Some(parsed(JsValue::Bool(true), index + 4, true));
    }
    if starts_with(input, index, "false") {
        return Some(parsed(JsValue::Bool(false), index + 5, true));
    }
    if starts_with(input, index, "null") {
        return Some(parsed(JsValue::Null, index + 4, true));
    }
    None
}

/// `/^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/u` then `Number(match)`.
fn parse_number(input: &[u16], index: usize) -> Option<Parsed<JsValue>> {
    let mut end = index;
    if unit_is(input, end, b'-') {
        end += 1;
    }
    if unit_is(input, end, b'0') {
        end += 1;
    } else if is_digit(input.get(end)) {
        while is_digit(input.get(end)) {
            end += 1;
        }
    } else {
        return None;
    }
    if unit_is(input, end, b'.') && is_digit(input.get(end + 1)) {
        end += 1;
        while is_digit(input.get(end)) {
            end += 1;
        }
    }
    if unit_is(input, end, b'e') || unit_is(input, end, b'E') {
        let mut exponent = end + 1;
        if unit_is(input, exponent, b'+') || unit_is(input, exponent, b'-') {
            exponent += 1;
        }
        if is_digit(input.get(exponent)) {
            while is_digit(input.get(exponent)) {
                exponent += 1;
            }
            end = exponent;
        }
    }
    let text = String::from_utf16(&input[index..end]).ok()?;
    let number: f64 = text.parse().ok()?;
    Some(parsed(JsValue::Number(number), end, true))
}

fn parse_array(input: &[u16], index: usize) -> Option<Parsed<JsValue>> {
    if !unit_is(input, index, b'[') {
        return None;
    }
    let mut values = Vec::new();
    let mut current = index + 1;
    while current <= input.len() {
        current = skip_whitespace(input, current);
        match input.get(current) {
            Some(&unit) if unit == u16::from(b']') => {
                return Some(parsed(JsValue::Array(values), current + 1, true));
            }
            None => return Some(parsed(JsValue::Array(values), input.len(), false)),
            Some(_) => {}
        }
        let Some(member) = parse_value(input, current) else {
            return Some(parsed(JsValue::Array(values), current, false));
        };
        values.push(member.value);
        current = skip_whitespace(input, member.next_index);
        if !member.complete {
            return Some(parsed(JsValue::Array(values), current, false));
        }
        match input.get(current) {
            Some(&unit) if unit == u16::from(b',') => current += 1,
            Some(&unit) if unit == u16::from(b']') => {
                return Some(parsed(JsValue::Array(values), current + 1, true));
            }
            None => return Some(parsed(JsValue::Array(values), input.len(), false)),
            Some(_) => return Some(parsed(JsValue::Array(values), current, false)),
        }
    }
    Some(parsed(JsValue::Array(values), input.len(), false))
}

fn parse_object(input: &[u16], index: usize) -> Option<Parsed<JsObject>> {
    if !unit_is(input, index, b'{') {
        return None;
    }
    let mut value = JsObject::new();
    let mut current = index + 1;
    while current <= input.len() {
        current = skip_whitespace(input, current);
        match input.get(current) {
            Some(&unit) if unit == u16::from(b'}') => {
                return Some(parsed(value, current + 1, true));
            }
            None => return Some(parsed(value, input.len(), false)),
            Some(_) => {}
        }
        let key = parse_string(input, current);
        let key = match key {
            Some(key) if key.complete => key,
            other => {
                let next = other.map_or(current, |key| key.next_index);
                return Some(parsed(value, next, false));
            }
        };
        current = skip_whitespace(input, key.next_index);
        if !unit_is(input, current, b':') {
            return Some(parsed(value, current, false));
        }
        current = skip_whitespace(input, current + 1);
        let Some(member) = parse_value(input, current) else {
            return Some(parsed(value, current, false));
        };
        if !member.complete {
            return Some(parsed(
                value,
                skip_whitespace(input, member.next_index),
                false,
            ));
        }
        let key = js_text_from_utf16(&key.value);
        if key != "__proto__" {
            value.insert(key, member.value);
        }
        current = skip_whitespace(input, member.next_index);
        match input.get(current) {
            Some(&unit) if unit == u16::from(b',') => current += 1,
            Some(&unit) if unit == u16::from(b'}') => {
                return Some(parsed(value, current + 1, true));
            }
            None => return Some(parsed(value, input.len(), false)),
            Some(_) => return Some(parsed(value, current, false)),
        }
    }
    Some(parsed(value, input.len(), false))
}

fn parse_value(input: &[u16], index: usize) -> Option<Parsed<JsValue>> {
    let current = skip_whitespace(input, index);
    let character = *input.get(current)?;
    match u8::try_from(character).unwrap_or(0) {
        b'"' => parse_string(input, current).map(|string| Parsed {
            value: JsValue::String(js_text_from_utf16(&string.value)),
            next_index: string.next_index,
            complete: string.complete,
        }),
        b'{' => parse_object(input, current).map(|object| Parsed {
            value: JsValue::Object(object.value),
            next_index: object.next_index,
            complete: object.complete,
        }),
        b'[' => parse_array(input, current),
        b't' | b'f' | b'n' => parse_literal(input, current),
        b'-' | b'0'..=b'9' => parse_number(input, current),
        _ => None,
    }
}

/// `parsePartialJsonObject(input)`: `None` when the input does not start
/// with an object.
#[must_use]
pub fn parse_partial_json_object(input: &str) -> Option<PartialJsonObject> {
    let units: Vec<u16> = js_text_utf16(input).collect();
    let start = skip_whitespace(&units, 0);
    let object = parse_object(&units, start)?;
    Some(PartialJsonObject {
        value: object.value,
        complete: object.complete && skip_whitespace(&units, object.next_index) == units.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_partial_json_object;
    use spocky_contracts::js_value::{JsValue, stringify};

    fn render(input: &str) -> Option<(String, bool)> {
        parse_partial_json_object(input)
            .map(|parsed| (stringify(&JsValue::Object(parsed.value)), parsed.complete))
    }

    // Ported from partial-json.test.ts.
    #[test]
    fn parses_complete_objects() {
        assert_eq!(
            render(r#"{"command":"pwd","cwd":"/tmp/repo"}"#),
            Some((r#"{"command":"pwd","cwd":"/tmp/repo"}"#.to_owned(), true))
        );
    }

    #[test]
    fn does_not_emit_incomplete_string_values() {
        assert_eq!(
            render(r#"{"command":"echo "#),
            Some(("{}".to_owned(), false))
        );
    }

    #[test]
    fn returns_only_complete_prefix_fields_from_incomplete_objects() {
        assert_eq!(
            render(r#"{"file_path":"src/message.tsx","old_string":"before"#),
            Some((r#"{"file_path":"src/message.tsx"}"#.to_owned(), false))
        );
    }

    #[test]
    fn does_not_emit_incomplete_nested_values() {
        assert_eq!(
            render(r#"{"payload":{"path":"src/index.ts","content":"hello"#),
            Some(("{}".to_owned(), false))
        );
    }

    #[test]
    fn returns_none_for_non_object_payloads() {
        assert_eq!(render(r#""text""#), None);
    }

    // node: parsePartialJsonObject on each input, JSON.stringify of value.
    #[test]
    fn edge_cases_follow_the_baseline() {
        assert_eq!(
            render(r#" {"a":[1,-0,2.5e3,true,null,"\ud800xé"],"b":1e} "#),
            Some((
                r#"{"a":[1,0,2500,true,null,"\ud800xé"],"b":1}"#.to_owned(),
                false
            ))
        );
        assert_eq!(
            render(r#"{"a":1,"__proto__":{"x":1},"b":"\/"}"#),
            Some((r#"{"a":1,"b":"/"}"#.to_owned(), true))
        );
        assert_eq!(
            render(r#"{"a":1} x"#),
            Some((r#"{"a":1}"#.to_owned(), false))
        );
        assert_eq!(render(r#"{"a":"\q"}"#), Some(("{}".to_owned(), false)));
        assert_eq!(render(r#"{"a":[1,2"#), Some(("{}".to_owned(), false)));
        assert_eq!(render(r#"{"a":-}"#), Some(("{}".to_owned(), false)));
        assert_eq!(
            render(r#"{"a":01}"#),
            Some((r#"{"a":0}"#.to_owned(), false))
        );
        assert_eq!(render(""), None);
    }
}
