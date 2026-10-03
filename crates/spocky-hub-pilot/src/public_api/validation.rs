//! The slice of zod 4 behavior the Hub request schemas use: issue wording, issue order and the
//! checks that still run on values of the wrong type.

use spocky_contracts::js::js_string;
use spocky_contracts::js_value::JsObject;
use spocky_contracts::text::{js_length, js_trim};

use super::value::{JsValueExt as _, Json};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathPart {
    Key(String),
    Index(usize),
}

impl PathPart {
    #[must_use]
    pub fn to_json(&self) -> Json {
        match self {
            Self::Key(key) => Json::string(key),
            #[allow(clippy::cast_precision_loss)]
            Self::Index(index) => Json::Number(*index as f64),
        }
    }
}

/// One schema failure: where it happened and zod's message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Issue {
    pub path: Vec<PathPart>,
    pub message: String,
}

impl Issue {
    #[must_use]
    pub fn new(path: &[PathPart], message: String) -> Self {
        Self {
            path: path.to_vec(),
            message,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::object([
            (
                "path",
                Json::Array(self.path.iter().map(PathPart::to_json).collect()),
            ),
            ("message", Json::string(&self.message)),
        ])
    }
}

/// Schema issues in zod order, plus whether validation threw instead (a `RangeError` from a
/// value too deeply nested to coerce), which the baseline reports as an internal error.
#[derive(Debug, Default)]
pub struct Issues {
    list: Vec<Issue>,
    thrown: bool,
}

impl Issues {
    pub fn push(&mut self, issue: Issue) {
        self.list.push(issue);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn throw(&mut self) {
        self.thrown = true;
    }

    /// The failure these issues amount to.
    #[must_use]
    pub fn into_failure(self) -> ParseFailure {
        if self.thrown {
            ParseFailure::Thrown
        } else {
            ParseFailure::Invalid(self.list)
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn thrown(&self) -> bool {
        self.thrown
    }
}

/// Why a request body did not parse into a typed input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseFailure {
    /// The schema rejected the body; the baseline answers 400 `invalid_request`.
    Invalid(Vec<Issue>),
    /// Validation threw (stack overflow while coercing a `length`); the baseline answers 500.
    Thrown,
}

pub(super) fn key(path: &[PathPart], name: &str) -> Vec<PathPart> {
    let mut next = path.to_vec();
    next.push(PathPart::Key(name.to_owned()));
    next
}

pub(super) fn index(path: &[PathPart], position: usize) -> Vec<PathPart> {
    let mut next = path.to_vec();
    next.push(PathPart::Index(position));
    next
}

/// zod 4 `z.uuid()`: versions 1 to 8 with a variant nibble of 8 to b, plus the nil and the
/// lower-case max UUID.
#[must_use]
pub fn is_uuid(text: &str) -> bool {
    if text == "00000000-0000-0000-0000-000000000000"
        || text == "ffffffff-ffff-ffff-ffff-ffffffffffff"
    {
        return true;
    }
    let groups: Vec<&str> = text.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == 5
        && groups.iter().zip(lengths).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && matches!(groups[2].as_bytes()[0], b'1'..=b'8')
        && matches!(
            groups[3].as_bytes()[0],
            b'8' | b'9' | b'a' | b'b' | b'A' | b'B'
        )
}

fn received(value: Option<&Json>) -> &'static str {
    value.map_or("undefined", Json::type_name)
}

pub(super) fn invalid_type(
    path: &[PathPart],
    expected: &str,
    value: Option<&Json>,
    issues: &mut Issues,
) {
    issues.push(Issue::new(
        path,
        format!(
            "Invalid input: expected {expected}, received {}",
            received(value)
        ),
    ));
}

/// Nesting depth at which V8's recursive `Array.prototype.join` overflows its stack in the pinned
/// Hub: a `length` array nested 4,400 deep is joined, 4,600 deep throws a `RangeError`.
const MAX_JOIN_DEPTH: usize = 4_500;

/// The `ToNumber` conversion the baseline applies to a `length` property of any JSON type.
/// `None` is the `RangeError` the baseline throws when an array is nested too deeply to join.
fn to_number(value: &Json) -> Option<f64> {
    Some(match value {
        Json::Null => 0.0,
        Json::Bool(flag) => f64::from(u8::from(*flag)),
        Json::Number(number) => *number,
        Json::String(text) => string_to_number(text),
        Json::Array(items) => string_to_number(&array_to_string(items)?),
        Json::Undefined | Json::Object(_) => f64::NAN,
    })
}

/// `Array.prototype.join` as `ToPrimitive` applies it to an array value, with an explicit stack.
/// `None` when the nesting is deeper than [`MAX_JOIN_DEPTH`].
///
/// Kept here because `spocky_contracts::js::js_string` joins arrays recursively and has no
/// `RangeError` depth, so a body nested a megabyte deep would overflow the Rust stack there. Scalars
/// still go through `js_string`.
fn array_to_string(items: &[Json]) -> Option<String> {
    let mut out = String::new();
    let mut stack: Vec<(&[Json], usize)> = vec![(items, 0)];
    while let Some((slice, next)) = stack.last_mut() {
        let Some(item) = slice.get(*next) else {
            stack.pop();
            continue;
        };
        if *next > 0 {
            out.push(',');
        }
        *next += 1;
        match item {
            Json::Array(nested) => {
                if stack.len() >= MAX_JOIN_DEPTH {
                    return None;
                }
                stack.push((nested, 0));
            }
            Json::Null | Json::Undefined => {}
            scalar => out.push_str(&js_string(Some(scalar))),
        }
    }
    Some(out)
}

/// `StringToNumber`; `spocky_contracts` has no `ToNumber`, so it stays here. Trimmed decimal literals, `Infinity` and the `0x`, `0o` and `0b` forms.
fn string_to_number(text: &str) -> f64 {
    let text = js_trim(text);
    if text.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = text.strip_prefix(prefix) {
            return if digits.is_empty() || !digits.chars().all(|ch| ch.is_digit(radix)) {
                f64::NAN
            } else {
                digits.chars().fold(0.0, |total, ch| {
                    total * f64::from(radix) + f64::from(ch.to_digit(radix).unwrap_or(0))
                })
            };
        }
    }
    let (sign, body) = match text.as_bytes()[0] {
        b'-' => (-1.0, &text[1..]),
        b'+' => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    let digits = body.bytes().filter(u8::is_ascii_digit).count();
    let valid = digits > 0
        && body
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'))
        && !body.starts_with(['e', 'E', '+', '-'])
        && body.matches('.').count() <= 1;
    if !valid {
        return f64::NAN;
    }
    body.parse::<f64>().map_or(f64::NAN, |value| sign * value)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StringRule {
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub trim: bool,
    pub uuid: bool,
}

/// What zod calls the origin of a length check: the wording depends on the value's type.
#[derive(Clone, Copy)]
enum Origin {
    String,
    Array,
    Unknown,
}

fn length_issues(
    origin: Origin,
    length: f64,
    min: Option<usize>,
    max: Option<usize>,
    path: &[PathPart],
    issues: &mut Issues,
) {
    let (name, tail) = match origin {
        Origin::String => ("string", Some("characters")),
        Origin::Array => ("array", Some("items")),
        Origin::Unknown => ("unknown", None),
    };
    #[allow(clippy::cast_precision_loss)]
    if let Some(min) = min
        && (length.is_nan() || length < min as f64)
    {
        issues.push(Issue::new(
            path,
            match tail {
                Some(unit) => format!("Too small: expected {name} to have >={min} {unit}"),
                None => format!("Too small: expected {name} to be >={min}"),
            },
        ));
    }
    #[allow(clippy::cast_precision_loss)]
    if let Some(max) = max
        && (length.is_nan() || length > max as f64)
    {
        issues.push(Issue::new(
            path,
            match tail {
                Some(unit) => format!("Too big: expected {name} to have <={max} {unit}"),
                None => format!("Too big: expected {name} to be <={max}"),
            },
        ));
    }
}

/// zod runs length checks on any non-nullish value that has a `length`, even after a type
/// failure: arrays, strings and objects with a `length` property of any type. The check fails
/// unless `length >= min` (`length <= max`), so a `length` that is not a number fails both.
#[allow(clippy::cast_precision_loss)]
fn length_after_type_failure(
    value: &Json,
    min: Option<usize>,
    max: Option<usize>,
    path: &[PathPart],
    issues: &mut Issues,
) {
    match value {
        Json::Array(items) => {
            length_issues(Origin::Array, items.len() as f64, min, max, path, issues);
        }
        Json::String(text) => {
            length_issues(
                Origin::String,
                js_length(text) as f64,
                min,
                max,
                path,
                issues,
            );
        }
        Json::Object(_) => {
            if let Some(length) = value.get("length") {
                match to_number(length) {
                    Some(length) => length_issues(Origin::Unknown, length, min, max, path, issues),
                    None => issues.throw(),
                }
            }
        }
        _ => {}
    }
}

/// Validates a string property. Returns the (trimmed) value when it passed every check.
pub fn string_field(
    value: Option<&Json>,
    optional: bool,
    path: &[PathPart],
    rule: StringRule,
    issues: &mut Issues,
) -> Option<String> {
    let before = issues.len();
    match value {
        None if optional => return None,
        Some(Json::String(text)) => {
            let text = if rule.trim { js_trim(text) } else { text };
            #[allow(clippy::cast_precision_loss)]
            length_issues(
                Origin::String,
                js_length(text) as f64,
                rule.min,
                rule.max,
                path,
                issues,
            );
            if rule.uuid && !is_uuid(text) {
                issues.push(Issue::new(path, "Invalid UUID".to_owned()));
            }
            return (issues.len() == before).then(|| text.to_owned());
        }
        other => {
            invalid_type(path, "string", other, issues);
            if let Some(other) = other {
                length_after_type_failure(other, rule.min, rule.max, path, issues);
            }
        }
    }
    None
}

/// Validates an array property, returning its items when it is an array. Length checks run after
/// the caller has validated the items, through [`array_length`].
pub fn array_field<'a>(
    value: Option<&'a Json>,
    path: &[PathPart],
    min: usize,
    max: usize,
    issues: &mut Issues,
) -> Option<&'a [Json]> {
    if let Some(Json::Array(items)) = value {
        return Some(items);
    }
    invalid_type(path, "array", value, issues);
    if let Some(other) = value {
        length_after_type_failure(other, Some(min), Some(max), path, issues);
    }
    None
}

/// The length checks of an array that had the right type.
pub fn array_length(length: usize, path: &[PathPart], min: usize, max: usize, issues: &mut Issues) {
    #[allow(clippy::cast_precision_loss)]
    length_issues(
        Origin::Array,
        length as f64,
        Some(min),
        Some(max),
        path,
        issues,
    );
}

/// Returns the properties of an object value, or records the type failure. A strict object also
/// needs [`unrecognized_keys`] after its properties.
pub fn object_fields<'a>(
    value: Option<&'a Json>,
    path: &[PathPart],
    issues: &mut Issues,
) -> Option<&'a JsObject> {
    if let Some(Json::Object(fields)) = value {
        return Some(fields);
    }
    invalid_type(path, "object", value, issues);
    None
}

/// The strict object check. `__proto__` is never reported, matching zod on the baseline runtime.
pub fn unrecognized_keys(
    fields: &JsObject,
    known: &[&str],
    path: &[PathPart],
    issues: &mut Issues,
) {
    let unknown: Vec<String> = fields
        .iter()
        .map(|(name, _)| name)
        .filter(|name| *name != "__proto__" && !known.contains(name))
        .map(|name| format!("\"{name}\""))
        .collect();
    match unknown.len() {
        0 => {}
        1 => issues.push(Issue::new(
            path,
            format!("Unrecognized key: {}", unknown[0]),
        )),
        _ => issues.push(Issue::new(
            path,
            format!("Unrecognized keys: {}", unknown.join(", ")),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{Issue, Issues, ParseFailure, PathPart, StringRule, js_trim, string_field};
    use crate::public_api::value::{JsValueExt as _, Json};

    #[test]
    fn trims_like_javascript() {
        assert_eq!(js_trim("\u{feff}\u{a0} a \u{2028}"), "a");
        assert_eq!(js_trim("\u{85}a\u{85}"), "\u{85}a\u{85}");
    }

    fn slug_rule() -> StringRule {
        StringRule {
            min: Some(1),
            max: Some(100),
            trim: true,
            uuid: false,
        }
    }

    #[test]
    fn wrong_typed_arrays_get_both_issues() {
        let mut issues = Issues::default();
        let path = [PathPart::Key("projectSlug".to_owned())];
        let value = Json::Array(Vec::new());
        assert_eq!(
            string_field(Some(&value), false, &path, slug_rule(), &mut issues),
            None
        );
        assert_eq!(
            issues.into_failure(),
            ParseFailure::Invalid(vec![
                Issue::new(
                    &path,
                    "Invalid input: expected string, received array".to_owned()
                ),
                Issue::new(
                    &path,
                    "Too small: expected array to have >=1 items".to_owned()
                ),
            ])
        );
    }

    /// `{"length": [[[...]]]}` with `depth` nested arrays, built without recursion.
    fn deep_length_object(depth: usize) -> Json {
        let mut nested = Json::Array(Vec::new());
        for _ in 1..depth {
            nested = Json::Array(vec![nested]);
        }
        Json::object([("length", nested)])
    }

    #[test]
    fn a_deep_length_array_is_joined_up_to_the_baseline_depth_and_throws_beyond() {
        let path = [PathPart::Key("projectSlug".to_owned())];
        for (depth, thrown) in [
            (1_000, false),
            (4_500, false),
            (4_501, true),
            (500_000, true),
        ] {
            let mut issues = Issues::default();
            let value = deep_length_object(depth);
            assert_eq!(
                string_field(Some(&value), false, &path, slug_rule(), &mut issues),
                None
            );
            assert_eq!(issues.thrown(), thrown, "depth {depth}");
            // A joined empty array is "", which is 0 and fails the minimum length.
            assert_eq!(issues.len(), if thrown { 1 } else { 2 }, "depth {depth}");
        }
    }
}
