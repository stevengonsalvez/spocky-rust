//! zod 4.4.3 issue reporting, for `Invalid message: ${error.message}`.
//!
//! The pinned daemon sends that text when an inbound frame fails
//! `WSInboundMessageSchema.safeParse` (`websocket-server.ts:2146-2192`).
//! `error.message` is `JSON.stringify(issues, jsonStringifyReplacer, 2)`,
//! so each issue's keys, their order, and the English message must match zod
//! exactly.
//!
//! This ports only the schema kinds, checks, and issue messages that the
//! inbound schemas reach (`crate::zod_schemas` is generated from them),
//! following `zod/v4/core/schemas.js`, `checks.js`, `util.js`, and
//! `zod/v4/locales/en.js`. [`check`] tracks values only as far as issues
//! depend on them; [`verdict`] also builds the parsed output: shape-ordered
//! objects, passthrough keys, defaults, and transform results.

pub mod output;

use std::borrow::Cow;

use crate::js_value::{JsObject, JsValue, js_number, parse, stringify_pretty};
use crate::text::{js_length, js_trim};
use crate::ws::BROWSER_AUTOMATION_COMMAND_NAMES;

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// One path segment: an object key or an array index.
#[derive(Debug, Clone)]
enum Segment {
    Key(String),
    Index(usize),
}

/// A zod issue before `util.finalizeIssue`: its own keys in creation order,
/// with `input`, `inst`, and `continue` held apart because finalizing drops
/// them.
#[derive(Debug, Clone)]
struct Issue {
    fields: Vec<(&'static str, JsValue)>,
    /// `Some` once the issue has a `path` key, whose place in key order is the
    /// `PATH` entry in `fields`.
    path: Option<Vec<Segment>>,
    /// `continue`: `None` when the issue does not set it.
    continues: Option<bool>,
    /// `util.parsedType(issue.input)`, for `invalid_type` messages.
    received: &'static str,
    /// The message a check's error map gives, ahead of the locale.
    error: Option<&'static str>,
}

const PATH: &str = "path";

/// `util.parsedType` for a value `JSON.parse` produced.
fn parsed_type(value: &JsValue) -> &'static str {
    match value {
        JsValue::Undefined => "undefined",
        JsValue::Null => "null",
        JsValue::Bool(_) => "boolean",
        JsValue::Number(number) if number.is_nan() => "nan",
        JsValue::Number(_) => "number",
        JsValue::String(_) => "string",
        JsValue::Array(_) => "array",
        JsValue::Object(_) => "object",
    }
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[allow(clippy::cast_precision_loss)]
fn count(value: usize) -> JsValue {
    JsValue::Number(value as f64)
}

impl Issue {
    fn new(input: &JsValue) -> Self {
        Self {
            fields: Vec::new(),
            path: None,
            continues: None,
            received: parsed_type(input),
            error: None,
        }
    }

    fn field(mut self, key: &'static str, value: JsValue) -> Self {
        self.fields.push((key, value));
        self
    }

    fn with_path(self, segment: Segment) -> Self {
        self.with_segments(vec![segment])
    }

    fn with_segments(mut self, segments: Vec<Segment>) -> Self {
        self.fields.push((PATH, JsValue::Null));
        self.path = Some(segments);
        self
    }

    const fn with_error(mut self, error: Option<&'static str>) -> Self {
        self.error = error;
        self
    }

    const fn continuing(mut self, continues: bool) -> Self {
        self.continues = Some(continues);
        self
    }

    fn get(&self, key: &str) -> Option<&JsValue> {
        self.fields
            .iter()
            .find(|(field, _)| *field == key)
            .map(|(_, value)| value)
    }

    fn str_field(&self, key: &str) -> &str {
        self.get(key).and_then(JsValue::as_str).unwrap_or("")
    }

    fn values_field(&self, key: &str) -> &[JsValue] {
        self.get(key).and_then(JsValue::as_array).unwrap_or(&[])
    }

    /// `util.prefixIssues`, and the object fast path's
    /// `{ ...iss, path: iss.path ? [key, ...iss.path] : [key] }`: both
    /// prepend in place or append a new `path` key.
    fn prefix(mut self, segment: Segment) -> Self {
        if let Some(path) = &mut self.path {
            path.insert(0, segment);
            self
        } else {
            self.with_path(segment)
        }
    }

    /// `util.finalizeIssue` with the English locale and no custom error map.
    fn finalize(self) -> JsValue {
        let message = self.message();
        let mut object = JsObject::new();
        for (key, value) in self.fields {
            if key == PATH {
                object.insert(PATH, path_value(self.path.as_deref().unwrap_or(&[])));
            } else {
                object.insert(key, value);
            }
        }
        if self.path.is_none() {
            object.insert(PATH, JsValue::Array(Vec::new()));
        }
        object.insert("message", text(&message));
        JsValue::Object(object)
    }

    /// `iss.message`, else the `en` locale message for the issue.
    fn message(&self) -> String {
        if let Some(message) = self.get("message").and_then(JsValue::as_str)
            && !message.is_empty()
        {
            return message.to_owned();
        }
        if let Some(error) = self.error {
            return error.to_owned();
        }
        match self.str_field("code") {
            "invalid_type" => {
                let received = if self.received == "nan" {
                    "NaN"
                } else {
                    self.received
                };
                format!(
                    "Invalid input: expected {}, received {received}",
                    self.str_field("expected")
                )
            }
            "invalid_value" => {
                let values = self.values_field("values");
                if values.len() == 1 {
                    format!(
                        "Invalid input: expected {}",
                        stringify_primitive(&values[0])
                    )
                } else {
                    format!(
                        "Invalid option: expected one of {}",
                        join_values(values, "|")
                    )
                }
            }
            "too_big" => self.size_message("Too big", "maximum", "<=", "<"),
            "too_small" => self.size_message("Too small", "minimum", ">=", ">"),
            "invalid_format" => match self.str_field("format") {
                "regex" => format!(
                    "Invalid string: must match pattern {}",
                    self.str_field("pattern")
                ),
                "uuid" => "Invalid UUID".to_owned(),
                "url" => "Invalid URL".to_owned(),
                other => format!("Invalid {other}"),
            },
            "invalid_key" => format!("Invalid key in {}", self.str_field("origin")),
            "unrecognized_keys" => {
                let keys = self.values_field("keys");
                let plural = if keys.len() > 1 { "s" } else { "" };
                format!("Unrecognized key{plural}: {}", join_values(keys, ", "))
            }
            "invalid_union" => {
                let options = self.values_field("options");
                if options.is_empty() {
                    "Invalid input".to_owned()
                } else {
                    let options: Vec<String> = options
                        .iter()
                        .map(|option| format!("'{}'", option.as_str().unwrap_or("")))
                        .collect();
                    format!(
                        "Invalid discriminator value. Expected {}",
                        options.join(" | ")
                    )
                }
            }
            _ => "Invalid input".to_owned(),
        }
    }

    fn size_message(&self, label: &str, bound: &str, inclusive: &str, exclusive: &str) -> String {
        let adjective = if self.get("inclusive").and_then(JsValue::as_bool) == Some(true) {
            inclusive
        } else {
            exclusive
        };
        let bound = self
            .get(bound)
            .and_then(JsValue::as_f64)
            .map_or_else(String::new, js_number);
        let origin = self.str_field("origin");
        let unit = match origin {
            "string" => "characters",
            "array" => "items",
            _ => return format!("{label}: expected {origin} to be {adjective}{bound}"),
        };
        format!("{label}: expected {origin} to have {adjective}{bound} {unit}")
    }
}

fn path_value(path: &[Segment]) -> JsValue {
    JsValue::Array(
        path.iter()
            .map(|segment| match segment {
                Segment::Key(key) => text(key),
                Segment::Index(index) => count(*index),
            })
            .collect(),
    )
}

/// `util.stringifyPrimitive` for the strings, booleans, and finite numbers
/// these schemas list.
fn stringify_primitive(value: &JsValue) -> String {
    match value {
        JsValue::String(value) => format!("\"{value}\""),
        JsValue::Bool(flag) => flag.to_string(),
        JsValue::Number(number) => js_number(*number),
        _ => String::new(),
    }
}

fn join_values(values: &[JsValue], separator: &str) -> String {
    values
        .iter()
        .map(stringify_primitive)
        .collect::<Vec<_>>()
        .join(separator)
}

/// `StringToNumber`, for the relational comparisons a length check makes.
///
/// ponytail: a radix literal is folded in `f64`, which can misround past
/// 2^53; the only bounds compared against are below 1000, where order is
/// unaffected.
fn string_to_number(value: &str) -> f64 {
    let value = js_trim(value);
    if value.is_empty() {
        return 0.0;
    }
    let radix = match value.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = &value[2..];
        return digits
            .chars()
            .try_fold(0.0_f64, |total, digit| {
                digit
                    .to_digit(radix)
                    .map(|digit| total.mul_add(f64::from(radix), f64::from(digit)))
            })
            .filter(|_| !digits.is_empty())
            .unwrap_or(f64::NAN);
    }
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    if unsigned == "Infinity" {
        return if value.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let exponent_ok = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    if whole.len() + fraction.len() == 0 || !digits(whole) || !digits(fraction) || !exponent_ok {
        return f64::NAN;
    }
    value.parse().unwrap_or(f64::NAN)
}

/// `ToNumber` for a value `JSON.parse` produced.
fn to_number(value: &JsValue) -> f64 {
    match value {
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(flag) => f64::from(u8::from(*flag)),
        JsValue::Number(number) => *number,
        JsValue::String(value) => string_to_number(value),
        JsValue::Array(_) | JsValue::Object(_) => {
            string_to_number(&crate::js::js_string(Some(value)))
        }
    }
}

/// `input.length` where a length check's `when` lets it run (`input` is not
/// nullish and its `length` is not `undefined`), with
/// `util.getLengthableOrigin(input)`.
#[allow(clippy::cast_precision_loss)]
fn lengthable(value: &JsValue) -> Option<(f64, &'static str)> {
    match value {
        JsValue::String(value) => Some((js_length(value) as f64, "string")),
        JsValue::Array(items) => Some((items.len() as f64, "array")),
        JsValue::Object(object) => object
            .get("length")
            .map(|length| (to_number(length), "unknown")),
        _ => None,
    }
}

/// A check on `z.string()`, in the order zod runs them.
#[derive(Debug, Clone, Copy)]
pub enum StringCheck {
    /// `.trim()`, an `overwrite` check.
    Trim,
    /// `.min(n)`, on `.length`.
    Min(usize),
    /// `.max(n)`, on `.length`.
    Max(usize),
    /// `.toLowerCase()`, an `overwrite` check.
    Lower,
    /// A `string_format` check: `.regex()` (format `regex`) or `z.uuid()`.
    /// `pattern` is `RegExp.prototype.toString()`; `test` is its Rust form;
    /// `message` is the check's own error message, if any.
    Format {
        format: &'static str,
        pattern: &'static str,
        test: fn(&str) -> bool,
        message: Option<&'static str>,
    },
    /// `z.url()`: `new URL(input.trim())` must not throw; the value becomes
    /// the trimmed input.
    Url,
}

/// A check on any schema, run after it parses (`runChecks`).
#[derive(Debug, Clone, Copy)]
pub enum Refinement {
    /// `.min(n)` on `.length`, as `z.array().min(n)` uses.
    MinLength(usize),
    /// `.refine(test, message)`: a `custom` issue when `test` fails.
    Refine {
        test: fn(&JsValue) -> bool,
        message: &'static str,
    },
    /// `.superRefine(fn)`: the issues `fn` adds through `ctx.addIssue`.
    SuperRefine(fn(&JsValue) -> Vec<CustomIssue>),
}

/// An issue a `.superRefine()` adds: `{ code: "custom", path, message }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomIssue {
    pub path: Vec<String>,
    pub message: String,
}

/// What a value-producing transform does.
#[derive(Debug, Clone, PartialEq)]
pub enum Mapped {
    /// Returns its input.
    Same,
    /// Returns a new value, which the rest of the pipe validates.
    Value(JsValue),
    /// Throws an error with this message out of `safeParse`.
    Throws(String),
}

/// A check on `z.number()`, in the order zod runs them.
#[derive(Debug, Clone, Copy)]
pub enum NumberCheck {
    /// `.int()`: the `safeint` number format.
    Int,
    /// `.gt(n)`, as `.positive()` uses.
    Gt(f64),
    /// `.gte(n)`, as `.min(n)` and `.nonnegative()` use.
    Gte(f64),
    /// `.lte(n)`, as `.max(n)` uses.
    Lte(f64),
}

/// How an object schema treats keys outside its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownKeys {
    /// The default: never an issue, and dropped from the output.
    Strip,
    /// `.passthrough()`: never an issue, and kept after the shape keys.
    Passthrough,
    /// `.strict()`: one `unrecognized_keys` issue.
    Strict,
}

/// A `.transform()` the inbound schemas use, by the issues it adds.
#[derive(Debug, Clone, Copy)]
pub enum Transform {
    /// Adds no issue (`normalizeAgentAttachments`, attachment mapping).
    NoIssue,
    /// `BrowserAutomationHostCapabilitySchema.supportedCommands`: a custom
    /// issue when no known command name is listed.
    BrowserHostCommands,
    /// A transform that adds no issue but whose output the pipe validates
    /// next, such as a `z.preprocess()` step.
    Map(fn(&JsValue) -> Mapped),
}

/// The zod schema kinds the inbound schemas reach.
#[derive(Debug, Clone)]
pub enum Schema {
    String(Vec<StringCheck>),
    Number(Vec<NumberCheck>),
    Boolean,
    Null,
    Unknown,
    Literal(Vec<JsValue>),
    Enum(&'static [&'static str]),
    Array(Box<Schema>),
    Object(Vec<(&'static str, Schema)>, UnknownKeys),
    /// `z.object(shape).catchall(schema)`: keys outside the shape are parsed
    /// by `schema`.
    ObjectCatchall(Vec<(&'static str, Schema)>, Box<Schema>),
    /// `z.record(key, value)` with a string key schema.
    Record(Box<Schema>, Box<Schema>),
    /// A schema with checks on its parsed value.
    Refined(Box<Schema>, Vec<Refinement>),
    Union(Vec<Schema>),
    /// `z.discriminatedUnion(key, options)`, keyed by each option's
    /// discriminator value.
    Discriminated(&'static str, Vec<(&'static str, Schema)>),
    Optional(Box<Schema>),
    Nullable(Box<Schema>),
    /// `.default(value)`, with the default as JSON text.
    Default(Box<Schema>, &'static str),
    /// `.catch(value)`, with the fallback as JSON text: any issue of the
    /// inner schema is dropped and the value becomes the fallback.
    Catch(Box<Schema>, &'static str),
    Pipe(Box<Schema>, Box<Schema>),
    Transform(Transform),
    /// `z.lazy()`, and any schema shared by reference.
    Lazy(fn() -> &'static Schema),
    /// A discriminated-union option that is not ported.
    Unmodeled,
}

impl Schema {
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
            Self::Nullable(inner) | Self::Pipe(_, inner) | Self::Catch(inner, _) => {
                inner.optional_out()
            }
            Self::Lazy(target) => target().optional_out(),
            Self::Union(options) => options.iter().any(Self::optional_out),
            _ => false,
        }
    }
}

#[derive(Debug)]
struct Payload<'a> {
    value: Cow<'a, JsValue>,
    issues: Vec<Issue>,
    aborted: bool,
    /// Set by a transform; `$ZodOptional` drops the result for an absent
    /// value when it is set.
    fallback: bool,
}

impl<'a> Payload<'a> {
    const fn new(value: Cow<'a, JsValue>) -> Self {
        Self {
            value,
            issues: Vec::new(),
            aborted: false,
            fallback: false,
        }
    }

    /// `util.aborted(payload, start)`.
    fn aborted_from(&self, start: usize) -> bool {
        self.aborted
            || self.issues[start..]
                .iter()
                .any(|issue| issue.continues != Some(true))
    }

    /// `util.explicitlyAborted(payload)`.
    fn explicitly_aborted(&self) -> bool {
        self.aborted
            || self
                .issues
                .iter()
                .any(|issue| issue.continues == Some(false))
    }

    fn type_issue(&mut self, expected: &str) {
        let issue = Issue::new(&self.value)
            .field("expected", text(expected))
            .field("code", text("invalid_type"));
        self.issues.push(issue);
    }
}

/// How deep `z.lazy()` may nest before the port stops, where pinned zod
/// (`z.json()` recursing) is close to a `RangeError`: DIV-001,
/// `porting/inventory-summary.md`, measures that between 1,000 and 5,000
/// levels on node v22.20.0.
const LAZY_DEPTH_LIMIT: usize = 1_000;

#[derive(Debug, Default)]
struct Context {
    /// An unported option decided part of the outcome.
    unmodeled: bool,
    lazy_depth: usize,
    /// `LAZY_DEPTH_LIMIT` was reached.
    too_deep: bool,
    /// A transform threw this message out of `safeParse`.
    thrown: Option<String>,
    /// Build the parsed output as well as the issues.
    output: bool,
}

fn run<'a>(schema: &Schema, mut payload: Payload<'a>, context: &mut Context) -> Payload<'a> {
    match schema {
        Schema::String(checks) => {
            if !payload.value.is_string() {
                payload.type_issue("string");
            }
            run_string_checks(&mut payload, checks);
        }
        Schema::Number(checks) => run_number(&mut payload, checks),
        Schema::Boolean => {
            if payload.value.as_bool().is_none() {
                payload.type_issue("boolean");
            }
        }
        Schema::Null => {
            if !payload.value.is_null() {
                payload.type_issue("null");
            }
        }
        Schema::Unknown => {}
        Schema::Literal(values) => {
            if !values.contains(&payload.value) {
                let issue = Issue::new(&payload.value)
                    .field("code", text("invalid_value"))
                    .field("values", JsValue::Array(values.clone()));
                payload.issues.push(issue);
            }
        }
        Schema::Enum(values) => {
            if !payload
                .value
                .as_str()
                .is_some_and(|value| values.contains(&value))
            {
                let values = values.iter().map(|value| text(value)).collect();
                let issue = Issue::new(&payload.value)
                    .field("code", text("invalid_value"))
                    .field("values", JsValue::Array(values));
                payload.issues.push(issue);
            }
        }
        Schema::Array(element) => run_array(&mut payload, element, context),
        Schema::Object(shape, unknown) => {
            run_object(&mut payload, shape, *unknown, None, context);
        }
        Schema::ObjectCatchall(shape, extra) => {
            run_object(
                &mut payload,
                shape,
                UnknownKeys::Strip,
                Some(extra),
                context,
            );
        }
        Schema::Record(key, value) => run_record(&mut payload, key, value, context),
        Schema::Refined(inner, refinements) => {
            let mut result = run(inner, payload, context);
            run_refinements(&mut result, refinements);
            return result;
        }
        Schema::Union(options) => return run_union(payload, options, context),
        Schema::Discriminated(key, options) => {
            return run_discriminated(payload, key, options, context);
        }
        Schema::Optional(inner) => return run_optional(inner, payload, context),
        Schema::Nullable(inner) => {
            if !payload.value.is_null() {
                return run(inner, payload, context);
            }
        }
        Schema::Default(inner, default) => return run_default(inner, default, payload, context),
        Schema::Catch(inner, caught) => return run_catch(inner, caught, payload, context),
        Schema::Pipe(input, output) => {
            let mut left = run(input, payload, context);
            if !left.issues.is_empty() {
                left.aborted = true;
                return left;
            }
            return run(output, left, context);
        }
        Schema::Transform(transform) => run_transform(&mut payload, *transform, context),
        Schema::Lazy(target) => {
            if context.lazy_depth >= LAZY_DEPTH_LIMIT {
                context.too_deep = true;
            } else {
                context.lazy_depth += 1;
                let result = run(target(), payload, context);
                context.lazy_depth -= 1;
                return result;
            }
        }
        Schema::Unmodeled => context.unmodeled = true,
    }
    payload
}

/// `$ZodOptional`: an absent value passes, or the inner schema's own
/// handling of it, when that is optional too.
fn run_optional<'a>(inner: &Schema, payload: Payload<'a>, context: &mut Context) -> Payload<'a> {
    if inner.optional_in() {
        let absent = matches!(*payload.value, JsValue::Undefined);
        let result = run(inner, payload, context);
        if absent && (!result.issues.is_empty() || result.fallback) {
            return Payload::new(Cow::Owned(JsValue::Undefined));
        }
        return result;
    }
    if matches!(*payload.value, JsValue::Undefined) {
        payload
    } else {
        run(inner, payload, context)
    }
}

/// `$ZodCatch`: the inner schema's issues are dropped and the value becomes
/// the fallback.
fn run_catch<'a>(
    inner: &Schema,
    caught: &str,
    payload: Payload<'a>,
    context: &mut Context,
) -> Payload<'a> {
    let mut result = run(inner, payload, context);
    if !result.issues.is_empty() {
        result.issues.clear();
        result.fallback = true;
        result.value = Cow::Owned(parse(caught).unwrap_or(JsValue::Undefined));
    }
    result
}

/// `$ZodDefault`: an absent value becomes the default without parsing; a
/// parsed `undefined` becomes it too.
fn run_default<'a>(
    inner: &Schema,
    default: &str,
    mut payload: Payload<'a>,
    context: &mut Context,
) -> Payload<'a> {
    if matches!(*payload.value, JsValue::Undefined) {
        if context.output {
            payload.value = Cow::Owned(parse(default).unwrap_or(JsValue::Undefined));
        }
        return payload;
    }
    let mut result = run(inner, payload, context);
    if context.output && matches!(*result.value, JsValue::Undefined) {
        result.value = Cow::Owned(parse(default).unwrap_or(JsValue::Undefined));
    }
    result
}

/// A transform's added issues; `fallback` marks the value as produced.
fn run_transform(payload: &mut Payload<'_>, transform: Transform, context: &mut Context) {
    if let Transform::Map(map) = transform {
        match map(&payload.value) {
            Mapped::Same => {}
            Mapped::Value(value) => payload.value = Cow::Owned(value),
            Mapped::Throws(message) => {
                context.thrown.get_or_insert(message);
            }
        }
    } else if let Transform::BrowserHostCommands = transform
        && !payload
            .value
            .as_array()
            .unwrap_or(&[])
            .iter()
            .any(|command| {
                command
                    .as_str()
                    .is_some_and(|command| BROWSER_AUTOMATION_COMMAND_NAMES.contains(&command))
            })
    {
        // `context.addIssue({ code: "custom", message })`.
        let message =
            "supportedCommands must include at least one known browser automation command";
        let issue = Issue::new(&payload.value)
            .field("code", text("custom"))
            .field("message", text(message));
        payload.issues.push(issue);
    }
    payload.fallback = true;
}

/// A `.min()` or `.max()` length check's issue, when its `when` lets it
/// run on `value` (it has a `length`) and the length is out of bounds.
fn length_issue(value: &JsValue, bound: usize, minimum: bool) -> Option<Issue> {
    let (length, origin) = lengthable(value)?;
    #[allow(clippy::cast_precision_loss)]
    let bound = bound as f64;
    // `length >= minimum` and `length <= maximum` are false for NaN.
    let ok = if minimum {
        length >= bound
    } else {
        length <= bound
    };
    if ok {
        return None;
    }
    let (code, key) = if minimum {
        ("too_small", "minimum")
    } else {
        ("too_big", "maximum")
    };
    Some(
        Issue::new(value)
            .field("origin", text(origin))
            .field("code", text(code))
            .field(key, JsValue::Number(bound))
            .field("inclusive", JsValue::Bool(true))
            .continuing(true),
    )
}

/// `runChecks` for string checks: a length check has a `when`, so it still
/// runs after a non-aborting issue, on any value with a `length`.
fn run_string_checks(payload: &mut Payload<'_>, checks: &[StringCheck]) {
    let mut aborted = payload.aborted_from(0);
    for check in checks {
        let before = payload.issues.len();
        match *check {
            StringCheck::Min(bound) | StringCheck::Max(bound) => {
                if payload.explicitly_aborted() {
                    continue;
                }
                let minimum = matches!(check, StringCheck::Min(_));
                if let Some(issue) = length_issue(&payload.value, bound, minimum) {
                    payload.issues.push(issue);
                }
            }
            _ if aborted => continue,
            StringCheck::Trim => {
                if let JsValue::String(value) = &*payload.value {
                    payload.value = Cow::Owned(text(js_trim(value)));
                }
            }
            StringCheck::Lower => {
                if let JsValue::String(value) = &*payload.value {
                    payload.value = Cow::Owned(JsValue::String(value.to_lowercase()));
                }
            }
            StringCheck::Format {
                format,
                pattern,
                test,
                message,
            } => {
                if !test(payload.value.as_str().unwrap_or("")) {
                    let issue = Issue::new(&payload.value)
                        .field("origin", text("string"))
                        .field("code", text("invalid_format"))
                        .field("format", text(format))
                        .field("pattern", text(pattern))
                        .continuing(true)
                        .with_error(message);
                    payload.issues.push(issue);
                }
            }
            StringCheck::Url => {
                let trimmed = js_trim(payload.value.as_str().unwrap_or("")).to_owned();
                if url::Url::parse(&trimmed).is_ok() {
                    payload.value = Cow::Owned(JsValue::String(trimmed));
                } else {
                    let issue = Issue::new(&payload.value)
                        .field("code", text("invalid_format"))
                        .field("format", text("url"))
                        .continuing(true);
                    payload.issues.push(issue);
                }
            }
        }
        if payload.issues.len() > before && !aborted {
            aborted = payload.aborted_from(before);
        }
    }
}

/// `$ZodNumber` accepts finite numbers only; then its checks run.
fn run_number(payload: &mut Payload<'_>, checks: &[NumberCheck]) {
    match *payload.value {
        JsValue::Number(number) if number.is_finite() => {}
        JsValue::Number(number) => {
            let received = if number.is_nan() { "NaN" } else { "Infinity" };
            let issue = Issue::new(&payload.value)
                .field("expected", text("number"))
                .field("code", text("invalid_type"))
                .field("received", text(received));
            payload.issues.push(issue);
        }
        _ => payload.type_issue("number"),
    }
    run_number_checks(payload, checks);
}

/// `runChecks` for number checks, none of which has a `when`; they only see
/// finite numbers, since a type issue aborts them.
fn run_number_checks(payload: &mut Payload<'_>, checks: &[NumberCheck]) {
    let mut aborted = payload.aborted_from(0);
    for check in checks {
        if aborted {
            continue;
        }
        let before = payload.issues.len();
        let value = payload.value.as_f64().unwrap_or(f64::NAN);
        let issue = match *check {
            NumberCheck::Int if value.fract() != 0.0 => Some(
                Issue::new(&payload.value)
                    .field("expected", text("int"))
                    .field("format", text("safeint"))
                    .field("code", text("invalid_type"))
                    .continuing(false),
            ),
            NumberCheck::Int if value.abs() > MAX_SAFE_INTEGER => {
                let (code, key, bound) = if value > 0.0 {
                    ("too_big", "maximum", MAX_SAFE_INTEGER)
                } else {
                    ("too_small", "minimum", -MAX_SAFE_INTEGER)
                };
                Some(
                    Issue::new(&payload.value)
                        .field("code", text(code))
                        .field(key, JsValue::Number(bound))
                        .field(
                            "note",
                            text("Integers must be within the safe integer range."),
                        )
                        .field("origin", text("int"))
                        .field("inclusive", JsValue::Bool(true))
                        .continuing(true),
                )
            }
            NumberCheck::Int => None,
            NumberCheck::Gt(bound) | NumberCheck::Gte(bound) => {
                let inclusive = matches!(check, NumberCheck::Gte(_));
                let ok = if inclusive {
                    value >= bound
                } else {
                    value > bound
                };
                (!ok).then(|| {
                    Issue::new(&payload.value)
                        .field("origin", text("number"))
                        .field("code", text("too_small"))
                        .field("minimum", JsValue::Number(bound))
                        .field("inclusive", JsValue::Bool(inclusive))
                        .continuing(true)
                })
            }
            NumberCheck::Lte(bound) => (value > bound).then(|| {
                Issue::new(&payload.value)
                    .field("origin", text("number"))
                    .field("code", text("too_big"))
                    .field("maximum", JsValue::Number(bound))
                    .field("inclusive", JsValue::Bool(true))
                    .continuing(true)
            }),
        };
        if let Some(issue) = issue {
            payload.issues.push(issue);
            aborted = payload.aborted_from(before);
        }
    }
}

fn run_array(payload: &mut Payload<'_>, element: &Schema, context: &mut Context) {
    let JsValue::Array(items) = &*payload.value else {
        payload.type_issue("array");
        return;
    };
    let mut issues = Vec::new();
    let mut output = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let result = run(element, Payload::new(Cow::Borrowed(item)), context);
        if context.output {
            output.push(result.value.into_owned());
        }
        issues.extend(
            result
                .issues
                .into_iter()
                .map(|issue| issue.prefix(Segment::Index(index))),
        );
    }
    payload.issues.extend(issues);
    if context.output {
        payload.value = Cow::Owned(JsValue::Array(output));
    }
}

/// The `$ZodObjectJIT` fast path that `safeParse` takes, then the
/// `.strict()` catchall pass.
fn run_object(
    payload: &mut Payload<'_>,
    shape: &[(&'static str, Schema)],
    unknown: UnknownKeys,
    catchall: Option<&Schema>,
    context: &mut Context,
) {
    let JsValue::Object(object) = &*payload.value else {
        payload.type_issue("object");
        return;
    };
    let mut issues = Vec::new();
    let mut output = JsObject::new();
    for (key, schema) in shape {
        let present = object.get(key);
        let value = present.map_or(Cow::Owned(JsValue::Undefined), Cow::Borrowed);
        let mut result = run(schema, Payload::new(value), context);
        // An undefined result is kept only for a key the input has.
        if context.output && (present.is_some() || !matches!(*result.value, JsValue::Undefined)) {
            let value = std::mem::replace(&mut result.value, Cow::Owned(JsValue::Undefined));
            output.insert(*key, value.into_owned());
        }
        let optional_in = schema.optional_in();
        if result.issues.is_empty() {
            if present.is_none() && !optional_in {
                let issue = Issue::new(&JsValue::Undefined)
                    .field("code", text("invalid_type"))
                    .field("expected", text("nonoptional"))
                    .with_path(Segment::Key((*key).to_owned()));
                issues.push(issue);
            }
        } else if !(optional_in && schema.optional_out() && present.is_none()) {
            let segment = || Segment::Key((*key).to_owned());
            issues.extend(
                result
                    .issues
                    .into_iter()
                    .map(|issue| issue.prefix(segment())),
            );
        }
    }
    if let Some(extra) = catchall {
        let extras = object
            .iter()
            .filter(|(key, _)| *key != "__proto__" && !shape.iter().any(|(name, _)| name == key));
        for (key, value) in extras {
            let result = run(extra, Payload::new(Cow::Borrowed(value)), context);
            if context.output {
                output.insert(key, result.value.into_owned());
            }
            let segment = || Segment::Key(key.to_owned());
            issues.extend(
                result
                    .issues
                    .into_iter()
                    .map(|issue| issue.prefix(segment())),
            );
        }
    }
    if unknown == UnknownKeys::Passthrough && context.output {
        let extra = object
            .iter()
            .filter(|(key, _)| *key != "__proto__" && !shape.iter().any(|(name, _)| name == key));
        for (key, value) in extra {
            output.insert(key, value.clone());
        }
    }
    if unknown == UnknownKeys::Strict {
        let keys: Vec<JsValue> = object
            .iter()
            .map(|(key, _)| key)
            .filter(|key| *key != "__proto__" && !shape.iter().any(|(name, _)| name == key))
            .map(text)
            .collect();
        if !keys.is_empty() {
            let issue = Issue::new(&payload.value)
                .field("code", text("unrecognized_keys"))
                .field("keys", JsValue::Array(keys));
            issues.push(issue);
        }
    }
    payload.issues.extend(issues);
    if context.output {
        payload.value = Cow::Owned(JsValue::Object(output));
    }
}

/// `$ZodRecord` for a string key schema: a key the key schema rejects is
/// one `invalid_key` issue and its value is not checked.
fn run_record(payload: &mut Payload<'_>, key: &Schema, value: &Schema, context: &mut Context) {
    let JsValue::Object(object) = &*payload.value else {
        payload.type_issue("record");
        return;
    };
    let mut issues = Vec::new();
    let mut output = JsObject::new();
    for (name, item) in object.iter().filter(|(name, _)| *name != "__proto__") {
        let segment = || Segment::Key(name.to_owned());
        let key_result = run(key, Payload::new(Cow::Owned(text(name))), context);
        if !key_result.issues.is_empty() {
            let key_issues = key_result.issues.into_iter().map(Issue::finalize).collect();
            let issue = Issue::new(&JsValue::Undefined)
                .field("code", text("invalid_key"))
                .field("origin", text("record"))
                .field("issues", JsValue::Array(key_issues))
                .with_path(segment());
            issues.push(issue);
            continue;
        }
        let result = run(value, Payload::new(Cow::Borrowed(item)), context);
        if context.output {
            let key = key_result.value.as_str().unwrap_or(name).to_owned();
            output.insert(key, result.value.into_owned());
        }
        issues.extend(
            result
                .issues
                .into_iter()
                .map(|issue| issue.prefix(segment())),
        );
    }
    payload.issues.extend(issues);
    if context.output {
        payload.value = Cow::Owned(JsValue::Object(output));
    }
}

/// `runChecks` for checks on a parsed value: none but a length check runs
/// once an issue aborted.
fn run_refinements(payload: &mut Payload<'_>, refinements: &[Refinement]) {
    let mut aborted = payload.aborted_from(0);
    for refinement in refinements {
        let before = payload.issues.len();
        match *refinement {
            Refinement::MinLength(bound) => {
                if payload.explicitly_aborted() {
                    continue;
                }
                if let Some(issue) = length_issue(&payload.value, bound, true) {
                    payload.issues.push(issue);
                }
            }
            _ if aborted => continue,
            Refinement::Refine { test, message } => {
                if !test(&payload.value) {
                    let issue = Issue::new(&payload.value)
                        .field("code", text("custom"))
                        .with_segments(Vec::new())
                        .continuing(true)
                        .with_error(Some(message));
                    payload.issues.push(issue);
                }
            }
            Refinement::SuperRefine(refine) => {
                for custom in refine(&payload.value) {
                    let path = custom.path.into_iter().map(Segment::Key).collect();
                    let issue = Issue::new(&payload.value)
                        .field("code", text("custom"))
                        .with_segments(path)
                        .field("message", JsValue::String(custom.message))
                        .continuing(true);
                    payload.issues.push(issue);
                }
            }
        }
        if payload.issues.len() > before && !aborted {
            aborted = payload.aborted_from(before);
        }
    }
}

fn run_union<'a>(
    mut payload: Payload<'a>,
    options: &[Schema],
    context: &mut Context,
) -> Payload<'a> {
    if let [only] = options {
        return run(only, payload, context);
    }
    let mut results = Vec::new();
    for option in options {
        let result = run(option, Payload::new(payload.value.clone()), context);
        if result.issues.is_empty() {
            return result;
        }
        results.push(result);
    }
    let nonaborted: Vec<usize> = (0..results.len())
        .filter(|index| !results[*index].aborted_from(0))
        .collect();
    if let [index] = nonaborted[..] {
        return results.swap_remove(index);
    }
    let errors = results
        .into_iter()
        .map(|result| JsValue::Array(result.issues.into_iter().map(Issue::finalize).collect()))
        .collect();
    let issue = Issue::new(&payload.value)
        .field("code", text("invalid_union"))
        .field("errors", JsValue::Array(errors));
    payload.issues.push(issue);
    payload
}

fn run_discriminated<'a>(
    mut payload: Payload<'a>,
    key: &'static str,
    options: &[(&'static str, Schema)],
    context: &mut Context,
) -> Payload<'a> {
    let JsValue::Object(object) = &*payload.value else {
        let issue = Issue::new(&payload.value)
            .field("code", text("invalid_type"))
            .field("expected", text("object"));
        payload.issues.push(issue);
        return payload;
    };
    let tag = object.get(key).and_then(JsValue::as_str);
    if let Some((_, option)) = options.iter().find(|(value, _)| Some(*value) == tag) {
        return run(option, payload, context);
    }
    let values = options.iter().map(|(value, _)| text(value)).collect();
    let issue = Issue::new(&payload.value)
        .field("code", text("invalid_union"))
        .field("errors", JsValue::Array(Vec::new()))
        .field("note", text("No matching discriminator"))
        .field("discriminator", text(key))
        .field("options", JsValue::Array(values))
        .with_path(Segment::Key(key.to_owned()));
    payload.issues.push(issue);
    payload
}

/// What `schema.safeParse(value)` concludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted.
    Valid,
    /// Rejected; the text is `error.message`.
    Invalid(String),
    /// Decided in part by a discriminated-union option that is not ported.
    Unmodeled,
    /// Nested past `LAZY_DEPTH_LIMIT`, near where pinned zod throws
    /// `RangeError` instead of reporting issues.
    TooDeep,
}

/// What `schema.safeParse(value)` concludes, with its issues finalized as
/// `error.issues` holds them.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Accepted, with the parsed output.
    Valid(JsValue),
    Invalid(Vec<JsValue>),
    Unmodeled,
    TooDeep,
    Throws(String),
}

/// Values nested deeper than this are checked on a thread with
/// `DEEP_STACK` bytes of stack: each `z.lazy()` level takes about 9 KiB in a
/// debug build, so `LAZY_DEPTH_LIMIT` levels exceed a 2 MiB thread stack.
const SHALLOW_DEPTH: usize = 32;
const DEEP_STACK: usize = 64 << 20;

/// Array and object nesting of `value`, counted iteratively.
fn nesting_depth(value: &JsValue) -> usize {
    let mut deepest = 0;
    let mut pending = vec![(value, 0)];
    while let Some((value, depth)) = pending.pop() {
        deepest = deepest.max(depth);
        match value {
            JsValue::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            JsValue::Object(object) => {
                pending.extend(object.iter().map(|(_, item)| (item, depth + 1)));
            }
            _ => {}
        }
    }
    deepest
}

/// Runs `schema.safeParse(value)` and reports its outcome.
#[must_use]
pub fn check(schema: &Schema, value: &JsValue) -> Outcome {
    match judge(schema, value, false) {
        Verdict::Valid(_) => Outcome::Valid,
        Verdict::Invalid(issues) => Outcome::Invalid(stringify_pretty(&JsValue::Array(issues))),
        // Only a `Transform::Map` throws, and no schema checked through
        // `check` has one; `verdict` reports the message.
        Verdict::Unmodeled | Verdict::Throws(_) => Outcome::Unmodeled,
        Verdict::TooDeep => Outcome::TooDeep,
    }
}

/// Runs `schema.safeParse(value)` and returns its output or its finalized
/// issues.
#[must_use]
pub fn verdict(schema: &Schema, value: &JsValue) -> Verdict {
    judge(schema, value, true)
}

fn judge(schema: &Schema, value: &JsValue, output: bool) -> Verdict {
    if nesting_depth(value) <= SHALLOW_DEPTH {
        return verdict_here(schema, value, output);
    }
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(DEEP_STACK)
            .spawn_scoped(scope, || verdict_here(schema, value, output))
            .ok()
            .and_then(|thread| thread.join().ok())
            .unwrap_or(Verdict::TooDeep)
    })
}

fn verdict_here(schema: &Schema, value: &JsValue, output: bool) -> Verdict {
    let mut context = Context {
        output,
        ..Context::default()
    };
    let result = run(schema, Payload::new(Cow::Borrowed(value)), &mut context);
    if let Some(message) = context.thrown {
        Verdict::Throws(message)
    } else if context.too_deep {
        Verdict::TooDeep
    } else if context.unmodeled {
        Verdict::Unmodeled
    } else if result.issues.is_empty() {
        Verdict::Valid(result.value.into_owned())
    } else {
        Verdict::Invalid(result.issues.into_iter().map(Issue::finalize).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{NumberCheck, Outcome, Schema, StringCheck, UnknownKeys, check, string_to_number};
    use crate::js_value::parse;

    fn message(schema: &Schema, input: &str) -> String {
        match check(schema, &parse(input).unwrap()) {
            Outcome::Invalid(message) => message,
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    // Expected text printed by node v22.20.0 with zod 4.4.3:
    // schema.safeParse(JSON.parse(input)).error.message.
    #[test]
    fn length_check_runs_after_a_type_issue_and_reads_object_length() {
        let schema = Schema::Object(
            vec![("a", Schema::String(vec![StringCheck::Min(1)]))],
            UnknownKeys::Strip,
        );
        assert_eq!(
            message(&schema, r#"{"a":[]}"#),
            "[\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected string, received array\"\n  },\n  {\n    \"origin\": \"array\",\n    \"code\": \"too_small\",\n    \"minimum\": 1,\n    \"inclusive\": true,\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Too small: expected array to have >=1 items\"\n  }\n]"
        );
        assert_eq!(
            message(&schema, r#"{"a":{"length":"x"}}"#),
            "[\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected string, received object\"\n  },\n  {\n    \"origin\": \"unknown\",\n    \"code\": \"too_small\",\n    \"minimum\": 1,\n    \"inclusive\": true,\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Too small: expected unknown to be >=1\"\n  }\n]"
        );
    }

    #[test]
    fn missing_key_is_nonoptional_and_strict_lists_extra_keys() {
        let schema = Schema::Object(vec![("a", Schema::Unknown)], UnknownKeys::Strip);
        assert_eq!(
            message(&schema, "{}"),
            "[\n  {\n    \"code\": \"invalid_type\",\n    \"expected\": \"nonoptional\",\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected nonoptional, received undefined\"\n  }\n]"
        );
        let strict = Schema::Object(vec![("a", Schema::String(Vec::new()))], UnknownKeys::Strict);
        assert_eq!(
            message(&strict, r#"{"a":1,"b":2,"1":3}"#),
            "[\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"a\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  },\n  {\n    \"code\": \"unrecognized_keys\",\n    \"keys\": [\n      \"1\",\n      \"b\"\n    ],\n    \"path\": [],\n    \"message\": \"Unrecognized keys: \\\"1\\\", \\\"b\\\"\"\n  }\n]"
        );
    }

    #[test]
    fn int_aborts_later_number_checks() {
        let schema = Schema::Number(vec![
            NumberCheck::Int,
            NumberCheck::Gt(0.0),
            NumberCheck::Lte(200.0),
        ]);
        assert_eq!(
            message(&schema, "1.5"),
            "[\n  {\n    \"expected\": \"int\",\n    \"format\": \"safeint\",\n    \"code\": \"invalid_type\",\n    \"path\": [],\n    \"message\": \"Invalid input: expected int, received number\"\n  }\n]"
        );
        assert_eq!(check(&schema, &parse("200").unwrap()), Outcome::Valid);
    }

    #[test]
    fn union_reports_every_branch_unless_one_did_not_abort() {
        let schema = Schema::Union(vec![Schema::String(Vec::new()), Schema::Null]);
        assert_eq!(
            message(&schema, "1"),
            "[\n  {\n    \"code\": \"invalid_union\",\n    \"errors\": [\n      [\n        {\n          \"expected\": \"string\",\n          \"code\": \"invalid_type\",\n          \"path\": [],\n          \"message\": \"Invalid input: expected string, received number\"\n        }\n      ],\n      [\n        {\n          \"expected\": \"null\",\n          \"code\": \"invalid_type\",\n          \"path\": [],\n          \"message\": \"Invalid input: expected null, received number\"\n        }\n      ]\n    ],\n    \"path\": [],\n    \"message\": \"Invalid input\"\n  }\n]"
        );
        let schema = Schema::Union(vec![
            Schema::String(vec![StringCheck::Min(2)]),
            Schema::Null,
        ]);
        assert_eq!(
            message(&schema, "\"a\""),
            "[\n  {\n    \"origin\": \"string\",\n    \"code\": \"too_small\",\n    \"minimum\": 2,\n    \"inclusive\": true,\n    \"path\": [],\n    \"message\": \"Too small: expected string to have >=2 characters\"\n  }\n]"
        );
    }

    #[test]
    fn string_to_number_follows_ecmascript() {
        assert_eq!(string_to_number(" 12 ").to_bits(), 12.0_f64.to_bits());
        assert_eq!(string_to_number("").to_bits(), 0.0_f64.to_bits());
        assert_eq!(string_to_number("0x1F").to_bits(), 31.0_f64.to_bits());
        assert_eq!(
            string_to_number("-Infinity").to_bits(),
            f64::NEG_INFINITY.to_bits()
        );
        assert_eq!(string_to_number(".5e1").to_bits(), 5.0_f64.to_bits());
        for text in ["inf", "1_0", "0x", "-0x1", "1e", "e5", ".", "NaN"] {
            assert!(string_to_number(text).is_nan(), "{text}");
        }
    }
}
