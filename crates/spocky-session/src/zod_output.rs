//! zod 4.4.3 `schema.parse(value)` output for the schemas a creation
//! receipt is read back through (`crate::creation_schema`, generated from
//! the pinned `messages.js`).
//!
//! A parse rebuilds every object in schema key order, drops keys outside a
//! stripping shape, fills defaults, and applies the few transforms these
//! schemas use, following `zod/v4/core/schemas.js`. Values are checked as far
//! as zod's output depends on them (union options are tried in order, so
//! each must be accepted or rejected exactly). A rejected value is reported
//! only as rejected, with the path of the first value that failed.

use spocky_contracts::js::spread;
use spocky_store::js_value::{JsObject, JsValue, parse};

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A check on `z.number()`.
#[derive(Debug, Clone, Copy)]
pub enum NumberCheck {
    /// `.int()`: the `safeint` format.
    Int,
    /// `.gt(n)`, as `.positive()` uses.
    Gt(f64),
    /// `.gte(n)`, as `.nonnegative()` uses.
    Gte(f64),
}

/// Keys outside an object's shape.
#[derive(Debug, Clone)]
pub enum Catchall {
    /// No catchall: the key is dropped.
    Strip,
    /// `.strict()`: the key rejects the object.
    Never,
    /// `.catchall(schema)`, and `.passthrough()` as `z.unknown()`: the key
    /// is parsed and kept after the shape's keys.
    Keep(Box<Shape>),
}

/// A `.transform()` these schemas use.
#[derive(Debug, Clone, Copy)]
pub enum Transform {
    /// `(value) => value ?? null`.
    NullishToNull,
    /// `(value) => ({ ...value, [key]: null })`.
    SetNull(&'static str),
    /// `(value) => ({ ...value, [key]: value[key] ?? value[fallback] })`.
    SetOrFallback(&'static str, &'static str),
}

/// The zod schema kinds the creation receipt schema reaches.
#[derive(Debug, Clone)]
pub enum Shape {
    String,
    Number(Vec<NumberCheck>),
    Boolean,
    Null,
    Unknown,
    Literal(Vec<JsValue>),
    Enum(&'static [&'static str]),
    Array(Box<Shape>),
    Object(Vec<(&'static str, Shape)>, Catchall),
    /// `z.record(z.string(), value)`.
    Record(Box<Shape>),
    Union(Vec<Shape>),
    /// `z.discriminatedUnion(key, options)`, keyed by discriminator value.
    Discriminated(&'static str, Vec<(&'static str, Shape)>),
    Optional(Box<Shape>),
    Nullable(Box<Shape>),
    /// `.default(value)`, the value as JSON text.
    Default(&'static str, Box<Shape>),
    /// `.catch(value)`, the value as JSON text.
    Catch(&'static str, Box<Shape>),
    Pipe(Box<Shape>, Box<Shape>),
    Transform(Transform),
    /// `z.lazy()`, and any schema shared by reference.
    Lazy(fn() -> &'static Shape),
}

impl Shape {
    /// `_zod.optin === "optional"`.
    fn optional_in(&self) -> bool {
        match self {
            Self::Optional(_) | Self::Default(..) | Self::Catch(..) | Self::Transform(_) => true,
            Self::Nullable(inner) | Self::Pipe(inner, _) => inner.optional_in(),
            Self::Lazy(target) => target().optional_in(),
            Self::Union(options) => options.iter().any(Self::optional_in),
            _ => false,
        }
    }

    /// `_zod.optout === "optional"`.
    fn optional_out(&self) -> bool {
        match self {
            Self::Optional(_) => true,
            Self::Nullable(inner) | Self::Catch(_, inner) | Self::Pipe(_, inner) => {
                inner.optional_out()
            }
            Self::Lazy(target) => target().optional_out(),
            Self::Union(options) => options.iter().any(Self::optional_out),
            _ => false,
        }
    }
}

/// The path of the first value a parse rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected(pub Vec<String>);

/// One run: the output (`None` is `undefined`), or the rejection, and
/// whether a transform or catch produced the value (`payload.fallback`).
struct Run {
    value: Result<Option<JsValue>, Rejected>,
    fallback: bool,
}

impl Run {
    const fn ok(value: Option<JsValue>) -> Self {
        Self {
            value: Ok(value),
            fallback: false,
        }
    }

    fn rejected() -> Self {
        Self {
            value: Err(Rejected(Vec::new())),
            fallback: false,
        }
    }

    fn at(mut self, segment: &str) -> Self {
        if let Err(Rejected(path)) = &mut self.value {
            path.insert(0, segment.to_owned());
        }
        self
    }
}

/// `schema.parse(value)`: zod's output, or where it rejected the value.
///
/// # Errors
///
/// Returns the path of the first rejected value.
pub fn parse_output(shape: &Shape, value: &JsValue) -> Result<JsValue, Rejected> {
    run(shape, Some(value))
        .value
        .map(|output| output.unwrap_or(JsValue::Undefined))
}

fn json(text: &str) -> JsValue {
    parse(text).unwrap_or_else(|_| unreachable!("generated JSON literal {text}"))
}

fn number_ok(value: f64, checks: &[NumberCheck]) -> bool {
    value.is_finite()
        && checks.iter().all(|check| match *check {
            NumberCheck::Int => value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER,
            NumberCheck::Gt(bound) => value > bound,
            NumberCheck::Gte(bound) => value >= bound,
        })
}

fn leaf(accepted: bool, value: Option<&JsValue>) -> Run {
    if accepted {
        Run::ok(value.cloned())
    } else {
        Run::rejected()
    }
}

#[allow(clippy::too_many_lines)]
fn run(shape: &Shape, value: Option<&JsValue>) -> Run {
    match shape {
        Shape::String => leaf(matches!(value, Some(JsValue::String(_))), value),
        Shape::Number(checks) => leaf(
            matches!(value, Some(JsValue::Number(number)) if number_ok(*number, checks)),
            value,
        ),
        Shape::Boolean => leaf(matches!(value, Some(JsValue::Bool(_))), value),
        Shape::Null => leaf(matches!(value, Some(JsValue::Null)), value),
        Shape::Unknown => Run::ok(value.cloned()),
        Shape::Literal(values) => leaf(value.is_some_and(|v| values.contains(v)), value),
        Shape::Enum(values) => leaf(
            value
                .and_then(JsValue::as_str)
                .is_some_and(|text| values.contains(&text)),
            value,
        ),
        Shape::Array(element) => {
            let Some(JsValue::Array(items)) = value else {
                return Run::rejected();
            };
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match run(element, Some(item)).value {
                    Ok(output) => out.push(output.unwrap_or(JsValue::Undefined)),
                    Err(rejected) => {
                        return Run {
                            value: Err(rejected),
                            fallback: false,
                        }
                        .at(&index.to_string());
                    }
                }
            }
            Run::ok(Some(JsValue::Array(out)))
        }
        Shape::Object(fields, catchall) => object(fields, catchall, value),
        Shape::Record(element) => {
            let Some(JsValue::Object(input)) = value else {
                return Run::rejected();
            };
            let mut out = JsObject::new();
            for (key, item) in input.iter() {
                match run(element, Some(item)).value {
                    Ok(output) => out.insert(key, output.unwrap_or(JsValue::Undefined)),
                    Err(rejected) => {
                        return Run {
                            value: Err(rejected),
                            fallback: false,
                        }
                        .at(key);
                    }
                }
            }
            Run::ok(Some(JsValue::Object(out)))
        }
        Shape::Union(options) => options
            .iter()
            .map(|option| run(option, value))
            .find(|result| result.value.is_ok())
            .unwrap_or_else(Run::rejected),
        Shape::Discriminated(key, options) => {
            let Some(JsValue::Object(input)) = value else {
                return Run::rejected();
            };
            let selected = input.get(key).and_then(JsValue::as_str).and_then(|tag| {
                options
                    .iter()
                    .find(|(option, _)| *option == tag)
                    .map(|(_, shape)| shape)
            });
            match selected {
                Some(shape) => run(shape, value),
                None => Run::rejected().at(key),
            }
        }
        Shape::Optional(inner) => {
            if inner.optional_in() {
                let result = run(inner, value);
                if value.is_none() && (result.value.is_err() || result.fallback) {
                    return Run::ok(None);
                }
                return result;
            }
            if value.is_none() {
                return Run::ok(None);
            }
            run(inner, value)
        }
        Shape::Nullable(inner) => {
            if matches!(value, Some(JsValue::Null)) {
                return Run::ok(value.cloned());
            }
            run(inner, value)
        }
        Shape::Default(default, inner) => {
            if value.is_none() {
                return Run::ok(Some(json(default)));
            }
            let mut result = run(inner, value);
            if matches!(result.value, Ok(None)) {
                result.value = Ok(Some(json(default)));
            }
            result
        }
        Shape::Catch(caught, inner) => {
            let result = run(inner, value);
            if result.value.is_ok() {
                return result;
            }
            Run {
                value: Ok(Some(json(caught))),
                fallback: true,
            }
        }
        Shape::Pipe(input, output) => {
            let left = run(input, value);
            match left.value {
                Ok(left_value) => {
                    let mut result = run(output, left_value.as_ref());
                    result.fallback |= left.fallback;
                    result
                }
                Err(rejected) => Run {
                    value: Err(rejected),
                    fallback: left.fallback,
                },
            }
        }
        Shape::Transform(transform) => Run {
            value: Ok(Some(apply(*transform, value))),
            fallback: true,
        },
        Shape::Lazy(target) => run(target(), value),
    }
}

fn object(fields: &[(&'static str, Shape)], catchall: &Catchall, value: Option<&JsValue>) -> Run {
    let Some(JsValue::Object(input)) = value else {
        return Run::rejected();
    };
    let mut out = JsObject::new();
    let mut property = |key: &str, shape: &Shape, item: Option<&JsValue>| -> Result<(), Rejected> {
        let present = item.is_some();
        let result = run(shape, item);
        match result.value {
            Err(_) if shape.optional_in() && shape.optional_out() && !present => Ok(()),
            Err(Rejected(mut path)) => {
                path.insert(0, key.to_owned());
                Err(Rejected(path))
            }
            Ok(_) if !present && !shape.optional_in() => Err(Rejected(vec![key.to_owned()])),
            Ok(None) => {
                if present {
                    out.insert(key, JsValue::Undefined);
                }
                Ok(())
            }
            Ok(Some(output)) => {
                out.insert(key, output);
                Ok(())
            }
        }
    };
    for (key, shape) in fields {
        if let Err(rejected) = property(key, shape, input.get(key)) {
            return Run {
                value: Err(rejected),
                fallback: false,
            };
        }
    }
    for (key, item) in input.iter() {
        if key == "__proto__" || fields.iter().any(|(field, _)| *field == key) {
            continue;
        }
        let outcome = match catchall {
            Catchall::Strip => Ok(()),
            Catchall::Never => Err(Rejected(vec![key.to_owned()])),
            Catchall::Keep(shape) => property(key, shape, Some(item)),
        };
        if let Err(rejected) = outcome {
            return Run {
                value: Err(rejected),
                fallback: false,
            };
        }
    }
    Run::ok(Some(JsValue::Object(out)))
}

/// `value ?? fallback` on JavaScript values.
fn nullish(value: Option<&JsValue>) -> Option<&JsValue> {
    value.filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
}

fn apply(transform: Transform, value: Option<&JsValue>) -> JsValue {
    match transform {
        Transform::NullishToNull => nullish(value).cloned().unwrap_or(JsValue::Null),
        Transform::SetNull(key) => {
            let mut out = spread(value);
            out.insert(key, JsValue::Null);
            JsValue::Object(out)
        }
        Transform::SetOrFallback(key, fallback) => {
            let mut out = spread(value);
            let next = nullish(value.and_then(|v| v.get(key)))
                .or_else(|| value.and_then(|v| v.get(fallback)))
                .cloned()
                .unwrap_or(JsValue::Undefined);
            out.insert(key, next);
            JsValue::Object(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use spocky_store::js_value::{parse, stringify};

    use super::{Catchall, Shape, Transform, parse_output};

    fn output(shape: &Shape, text: &str) -> String {
        stringify(&parse_output(shape, &parse(text).expect("json")).expect("accepted"))
    }

    #[test]
    fn objects_take_schema_order_defaults_and_strip_unknown_keys() {
        let shape = Shape::Object(
            vec![
                ("b", Shape::String),
                ("a", Shape::Optional(Box::new(Shape::String))),
                (
                    "d",
                    Shape::Default("[]", Box::new(Shape::Array(Box::new(Shape::String)))),
                ),
                (
                    "n",
                    Shape::Pipe(
                        Box::new(Shape::Optional(Box::new(Shape::Nullable(Box::new(
                            Shape::String,
                        ))))),
                        Box::new(Shape::Transform(Transform::NullishToNull)),
                    ),
                ),
            ],
            Catchall::Strip,
        );
        assert_eq!(
            output(&shape, r#"{"x":1,"a":"y","b":"z"}"#),
            r#"{"b":"z","a":"y","d":[],"n":null}"#
        );
        assert!(parse_output(&shape, &parse(r#"{"a":"y"}"#).expect("json")).is_err());
    }

    #[test]
    fn unions_take_the_first_accepting_option() {
        let option = |tag: bool, transform| {
            Shape::Pipe(
                Box::new(Shape::Object(
                    vec![
                        (
                            "isGit",
                            Shape::Literal(vec![spocky_store::js_value::JsValue::Bool(tag)]),
                        ),
                        ("cwd", Shape::String),
                    ],
                    Catchall::Strip,
                )),
                Box::new(Shape::Transform(transform)),
            )
        };
        let shape = Shape::Union(vec![
            option(false, Transform::SetNull("root")),
            option(true, Transform::SetOrFallback("root", "cwd")),
        ]);
        assert_eq!(
            output(&shape, r#"{"cwd":"/w","isGit":true}"#),
            r#"{"isGit":true,"cwd":"/w","root":"/w"}"#
        );
        assert_eq!(
            output(&shape, r#"{"cwd":"/w","isGit":false}"#),
            r#"{"isGit":false,"cwd":"/w","root":null}"#
        );
    }
}
