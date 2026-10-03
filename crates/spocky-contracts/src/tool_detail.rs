//! The tool call detail primitives of pinned Paseo
//! `tool-call-detail-primitives.ts` and the pure helpers of
//! `tool-call-mapper-utils.ts` (`nonEmptyString`, `extractCodexShellOutput`,
//! `flattenReadContent`, `stripReadLineNumberGutter`, `truncateDiffText`) that
//! provider detail parsers share.
//!
//! The baseline parses with zod 4.4.3 schemas. Each schema here is a function
//! that returns `Ok(None)` where zod reports issues and `Ok(Some(..))` with
//! the transformed value where it succeeds. zod unions try their options in
//! order and keep the first success; objects check every shape key, so a
//! branch whose `name` literal does not match still parses its input and
//! output. Two baseline throws escape `safeParse` and are reproduced as
//! `Err`: an intersection whose sides disagree on `filePath` throws
//! `Unmergable intersection. Error path: ["filePath"]` (reachable from any
//! tool name whose earlier branches fail, since the write and edit branches
//! are tried in order), and an XML read result with a numeric entity above
//! U+10FFFF throws `RangeError: Invalid code point N`.

use crate::js_value::{JsObject, JsValue, js_text_from_utf16, js_text_utf16};
use crate::text::{is_js_whitespace, js_trim};

/// A JavaScript `Error` a baseline parser throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thrown {
    /// `error.name`.
    pub name: String,
    /// `error.message`.
    pub message: String,
}

/// A parse that zod either rejects (`None`) or accepts with a value.
pub type Parse<T> = Result<Option<T>, Thrown>;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_DIFF_CHARS: usize = 12_000;

fn unmergeable() -> Thrown {
    Thrown {
        name: "Error".to_owned(),
        message: r#"Unmergable intersection. Error path: ["filePath"]"#.to_owned(),
    }
}

#[must_use]
pub fn object(value: &JsValue) -> Option<&JsObject> {
    value.as_object()
}

/// An own property, with `undefined` read as absent.
#[must_use]
pub fn field<'a>(record: &'a JsObject, key: &str) -> Option<&'a JsValue> {
    record
        .get(key)
        .filter(|value| !matches!(value, JsValue::Undefined))
}

/// A member zod rejected.
struct Invalid;

/// A zod `.optional()` member: absent is `Ok(None)`.
fn optional<'a, T>(
    record: &'a JsObject,
    key: &str,
    parse: impl Fn(&'a JsValue) -> Option<T>,
) -> Result<Option<T>, Invalid> {
    match field(record, key) {
        None => Ok(None),
        Some(value) => parse(value).map(Some).ok_or(Invalid),
    }
}

/// A `.nullable()` number.
#[derive(Clone, Copy)]
enum NullableNumber {
    Null,
    Number(f64),
}

impl NullableNumber {
    const fn number(member: Option<Self>) -> Option<f64> {
        match member {
            Some(Self::Number(number)) => Some(number),
            Some(Self::Null) | None => None,
        }
    }
}

#[must_use]
pub fn string(value: &JsValue) -> Option<&str> {
    value.as_str()
}

/// `z.number()` (zod 4 rejects non-finite numbers, so `.finite()` adds nothing).
fn finite(value: &JsValue) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

/// `z.number().int()`: a safe integer.
fn int(value: &JsValue) -> Option<f64> {
    finite(value).filter(|number| number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER)
}

/// `z.number().int().nonnegative()`.
fn nonnegative_int(value: &JsValue) -> Option<f64> {
    int(value).filter(|number| *number >= 0.0)
}

fn string_array(value: &JsValue) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// `nonEmptyString(value)`.
#[must_use]
pub fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.filter(|text| !text.is_empty()).map(str::to_owned)
}

fn non_empty_field(record: &JsObject, key: &str) -> Option<String> {
    non_empty_string(field(record, key).and_then(JsValue::as_str))
}

fn first_non_empty(record: &JsObject, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| non_empty_field(record, key))
}

fn utf16_len(text: &str) -> usize {
    js_text_utf16(text).count()
}

/// `truncateDiffText(text)` with the default 12,000 code unit limit.
#[must_use]
pub fn truncate_diff_text(text: Option<String>) -> Option<String> {
    let text = text?;
    let length = utf16_len(&text);
    if length <= MAX_DIFF_CHARS {
        return Some(text);
    }
    let units: Vec<u16> = js_text_utf16(&text).take(MAX_DIFF_CHARS).collect();
    Some(format!(
        "{}\n...[truncated {} chars]",
        js_text_from_utf16(&units),
        length - MAX_DIFF_CHARS
    ))
}

/// Matches one `READ_GUTTER_LINE` (`/^\s*(\d+)\t(.*)$/`): the line number
/// digits and the rest of the line.
fn read_gutter_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start_matches(is_js_whitespace);
    // `\s*` is greedy, but backtracking never helps: digits are not spaces.
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let (number, after) = rest.split_at(digits);
    let body = after.strip_prefix('\t')?;
    // `.` stops at line terminators, and `$` without the `m` flag needs the
    // end of the input.
    if body.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some((number, body))
}

/// `stripReadLineNumberGutter(content)`: the stripped text and first line.
#[must_use]
pub fn strip_read_line_number_gutter(content: Option<&str>) -> Option<(String, Option<f64>)> {
    let text = non_empty_string(content)?;
    let normalized = text.replace("\r\n", "\n");
    let mut stripped: Vec<&str> = Vec::new();
    let mut non_empty = 0_u32;
    let mut matched = 0_u32;
    let mut start_line: Option<f64> = None;
    let mut previous: Option<f64> = None;
    let mut sequential = true;
    let mut first_non_empty_matched = false;
    let mut saw_non_empty = false;
    for line in normalized.split('\n') {
        if line.is_empty() {
            stripped.push(line);
            continue;
        }
        non_empty += 1;
        let Some((number, body)) = read_gutter_line(line) else {
            if !saw_non_empty {
                return None;
            }
            stripped.push(line);
            saw_non_empty = true;
            continue;
        };
        if !saw_non_empty {
            first_non_empty_matched = true;
        }
        saw_non_empty = true;
        matched += 1;
        let line_number: f64 = number.parse().unwrap_or(f64::NAN);
        if start_line.is_none() {
            start_line = Some(line_number);
        }
        #[allow(clippy::float_cmp)] // `lineNumber !== prevNumber + 1` compares doubles.
        if previous.is_some_and(|previous| line_number != previous + 1.0) {
            sequential = false;
        }
        previous = Some(line_number);
        stripped.push(body);
    }
    if !first_non_empty_matched || !sequential || non_empty == 0 {
        return None;
    }
    if f64::from(matched) / f64::from(non_empty) < 0.5 {
        return None;
    }
    Some((stripped.join("\n"), start_line))
}

/// `flattenReadContent(value)` over a parsed read content value.
fn flatten_read_content(value: Option<&JsValue>) -> Option<String> {
    let chunk_text = |chunk: &JsObject| first_non_empty(chunk, &["text", "content", "output"]);
    match value? {
        JsValue::String(text) => non_empty_string(Some(text)),
        JsValue::Array(chunks) => {
            let parts: Vec<String> = chunks
                .iter()
                .filter_map(|chunk| object(chunk).and_then(chunk_text))
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        JsValue::Object(chunk) => chunk_text(chunk),
        _ => None,
    }
}

// ---- shell ----------------------------------------------------------------

pub struct ShellInput {
    command: Option<String>,
    cwd: Option<String>,
}

/// `CommandValueSchema`: a string or an array of strings.
fn command_value(value: &JsValue) -> Option<&JsValue> {
    match value {
        JsValue::String(_) => Some(value),
        JsValue::Array(items) if items.iter().all(JsValue::is_string) => Some(value),
        _ => None,
    }
}

pub fn shell_input(value: &JsValue) -> Option<ShellInput> {
    let record = object(value)?;
    let branch = |key: &str| {
        command_value(field(record, key)?)?;
        optional(record, "cwd", string).ok()?;
        optional(record, "directory", string).ok()?;
        Some(())
    };
    branch("command").or_else(|| branch("cmd"))?;
    // `"command" in value ? value.command : value.cmd`; passthrough keeps an
    // invalid `command` that sent parsing to the `cmd` branch.
    let source = if record.get("command").is_some() {
        record.get("command")
    } else {
        record.get("cmd")
    };
    let command = match source.and_then(command_value) {
        Some(JsValue::String(text)) => non_empty_string(Some(text)),
        Some(JsValue::Array(tokens)) => {
            let joined = tokens
                .iter()
                .filter_map(JsValue::as_str)
                .map(js_trim)
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    };
    Some(ShellInput {
        command,
        cwd: non_empty_field(record, "cwd").or_else(|| non_empty_field(record, "directory")),
    })
}

pub struct ShellOutput {
    command: Option<String>,
    output: Option<String>,
    exit_code: Option<f64>,
}

fn optional_strings(record: &JsObject, keys: &[&str]) -> Option<()> {
    for key in keys {
        optional(record, key, string).ok()?;
    }
    Some(())
}

fn nullable_finite(value: &JsValue) -> Option<NullableNumber> {
    match value {
        JsValue::Null => Some(NullableNumber::Null),
        other => finite(other).map(NullableNumber::Number),
    }
}

pub fn shell_output(value: &JsValue) -> Option<ShellOutput> {
    if let JsValue::String(text) = value {
        return Some(ShellOutput {
            command: None,
            output: extract_codex_shell_output(Some(text)),
            exit_code: None,
        });
    }
    let record = object(value)?;
    optional_strings(
        record,
        &[
            "command",
            "output",
            "text",
            "content",
            "aggregated_output",
            "aggregatedOutput",
        ],
    )?;
    let exit_code = NullableNumber::number(optional(record, "exitCode", nullable_finite).ok()?);
    let exit_code_snake =
        NullableNumber::number(optional(record, "exit_code", nullable_finite).ok()?);
    let metadata = optional(record, "metadata", object).ok()?;
    let (meta_exit_code, meta_exit_code_snake) = match metadata {
        Some(metadata) => (
            NullableNumber::number(optional(metadata, "exitCode", nullable_finite).ok()?),
            NullableNumber::number(optional(metadata, "exit_code", nullable_finite).ok()?),
        ),
        None => (None, None),
    };
    let nested = |key: &str, keys: &[&str]| -> Option<Option<&JsObject>> {
        let nested = optional(record, key, object).ok()?;
        if let Some(nested) = nested {
            optional_strings(nested, keys)?;
        }
        Some(nested)
    };
    let structured = nested("structuredContent", &["output", "text", "content"])?;
    let structured_snake = nested("structured_content", &["output", "text", "content"])?;
    let result = nested("result", &["command", "output", "text", "content"])?;
    let from = |nested: Option<&JsObject>, key: &str| {
        nested.and_then(|nested| non_empty_field(nested, key))
    };
    let raw_text = first_non_empty(
        record,
        &[
            "output",
            "text",
            "content",
            "aggregated_output",
            "aggregatedOutput",
        ],
    )
    .or_else(|| from(structured, "output"))
    .or_else(|| from(structured, "text"))
    .or_else(|| from(structured, "content"))
    .or_else(|| from(structured_snake, "output"))
    .or_else(|| from(structured_snake, "text"))
    .or_else(|| from(structured_snake, "content"))
    .or_else(|| from(result, "output"))
    .or_else(|| from(result, "text"))
    .or_else(|| from(result, "content"));
    Some(ShellOutput {
        command: non_empty_field(record, "command").or_else(|| from(result, "command")),
        output: extract_codex_shell_output(raw_text.as_deref()),
        exit_code: exit_code
            .or(exit_code_snake)
            .or(meta_exit_code)
            .or(meta_exit_code_snake),
    })
}

#[must_use]
pub fn shell_detail(input: Option<ShellInput>, output: Option<ShellOutput>) -> Option<JsValue> {
    let command = input
        .as_ref()
        .and_then(|input| input.command.clone())
        .or_else(|| output.as_ref().and_then(|output| output.command.clone()))?;
    let mut detail = JsObject::new();
    detail.insert("type", text("shell"));
    detail.insert("command", text(&command));
    if let Some(cwd) = input.and_then(|input| input.cwd) {
        detail.insert("cwd", text(&cwd));
    }
    if let Some(output) = output {
        if let Some(body) = output.output {
            detail.insert("output", text(&body));
        }
        if let Some(code) = output.exit_code {
            detail.insert("exitCode", JsValue::Number(code));
        }
    }
    Some(JsValue::Object(detail))
}

// ---- read -----------------------------------------------------------------

pub struct ReadInput {
    file_path: String,
    offset: Option<f64>,
    limit: Option<f64>,
}

#[must_use]
pub fn read_input(value: &JsValue) -> Option<ReadInput> {
    let record = object(value)?;
    ["path", "file_path", "filePath"].iter().find_map(|key| {
        let file_path = field(record, key).and_then(string)?;
        let offset = optional(record, "offset", finite).ok()?;
        let limit = optional(record, "limit", finite).ok()?;
        Some(ReadInput {
            file_path: file_path.to_owned(),
            offset,
            limit,
        })
    })
}

/// `ToolReadChunkSchema`: `text`, `content`, `output` are absent or strings,
/// and at least one is a string.
fn is_read_chunk(value: &JsValue) -> bool {
    object(value).is_some_and(|record| {
        let keys = ["text", "content", "output"];
        keys.iter().all(|key| optional(record, key, string).is_ok())
            && keys.iter().any(|key| field(record, key).is_some())
    })
}

/// `ToolReadContentSchema`.
fn is_read_content(value: &JsValue) -> bool {
    match value {
        JsValue::String(_) => true,
        JsValue::Array(chunks) => chunks.iter().all(is_read_chunk),
        other => is_read_chunk(other),
    }
}

/// `ToolReadPayloadSchema`.
fn read_payload(value: &JsValue) -> Option<&JsObject> {
    let record = object(value)?;
    let keys = ["content", "text", "output"];
    let valid = keys
        .iter()
        .all(|key| field(record, key).is_none_or(is_read_content))
        && keys.iter().any(|key| field(record, key).is_some());
    valid.then_some(record)
}

fn payload_content(record: &JsObject) -> Option<String> {
    flatten_read_content(field(record, "content"))
        .or_else(|| flatten_read_content(field(record, "text")))
        .or_else(|| flatten_read_content(field(record, "output")))
}

pub struct ReadOutput {
    file_path: Option<String>,
    content: Option<String>,
}

/// `readXmlTag(value, tag)`: the first lazy `<tag>...</tag>` match, ASCII
/// case-insensitive.
fn read_xml_tag(value: &str, tag: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = lower[start..].find(&close)? + start;
    Some(value[start..end].to_owned())
}

/// `decodeXmlText(value)`.
fn decode_xml_text(value: &str) -> Result<String, Thrown> {
    decode_xml_entities(value).map(|decoded| rejoin_surrogates(&decoded))
}

/// Re-pairs surrogate halves that entities produced separately, as JavaScript
/// string concatenation does.
fn rejoin_surrogates(text: &str) -> String {
    let units: Vec<u16> = js_text_utf16(text).collect();
    js_text_from_utf16(&units)
}

fn decode_xml_entities(value: &str) -> Result<String, Thrown> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest[1..].find(';').map(|end| end + 1) else {
            out.push_str(rest);
            return Ok(out);
        };
        let body = &rest[1..end];
        let decoded = if let Some(decimal) = body
            .strip_prefix('#')
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        {
            Some(from_code_point(
                decimal.parse::<f64>().unwrap_or(f64::INFINITY),
            )?)
        } else if let Some(hex) = body
            .strip_prefix("#x")
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            Some(from_code_point(parse_hex(hex))?)
        } else {
            match body {
                "amp" => Some("&".to_owned()),
                "lt" => Some("<".to_owned()),
                "gt" => Some(">".to_owned()),
                "quot" => Some("\"".to_owned()),
                "apos" => Some("'".to_owned()),
                _ => None,
            }
        };
        if let Some(decoded) = decoded {
            out.push_str(&decoded);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// `Number.parseInt(digits, 16)`: exact through 128 bits, which covers any
/// value whose formatting can differ in the thrown message.
fn parse_hex(digits: &str) -> f64 {
    #[allow(clippy::cast_precision_loss)] // `as` rounds to nearest, as V8 does.
    u128::from_str_radix(digits, 16).map_or_else(
        |_| {
            digits.bytes().fold(0.0, |total, byte| {
                total * 16.0 + f64::from(char::from(byte).to_digit(16).unwrap_or(0))
            })
        },
        |value| value as f64,
    )
}

/// `String.fromCodePoint(n)`.
fn from_code_point(code: f64) -> Result<String, Thrown> {
    if !(0.0..=1_114_111.0).contains(&code) || code.fract() != 0.0 {
        return Err(Thrown {
            name: "RangeError".to_owned(),
            message: format!(
                "Invalid code point {}",
                crate::js::js_string(Some(&JsValue::Number(code)))
            ),
        });
    }
    // In range, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = code as u32;
    Ok(match char::from_u32(code) {
        Some(character) => {
            let mut buffer = [0_u16; 2];
            js_text_from_utf16(character.encode_utf16(&mut buffer))
        }
        None => js_text_from_utf16(&[u16::try_from(code).unwrap_or(0xFFFD)]),
    })
}

/// `trimOuterLineBreaks(value)`.
fn trim_outer_line_breaks(value: &str) -> &str {
    let value = value
        .strip_prefix("\r\n")
        .or_else(|| value.strip_prefix('\n'))
        .unwrap_or(value);
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value)
}

fn parse_xml_read_output(value: &str) -> Result<Option<ReadOutput>, Thrown> {
    let trimmed = js_trim(value);
    if !trimmed.starts_with("<path>") || !trimmed.to_ascii_lowercase().contains("<content>") {
        return Ok(None);
    }
    let file_path =
        read_xml_tag(trimmed, "path").and_then(|path| non_empty_string(Some(js_trim(&path))));
    let content = read_xml_tag(trimmed, "content");
    if file_path.is_none() && content.is_none() {
        return Ok(None);
    }
    Ok(Some(ReadOutput {
        file_path: file_path.map(|path| decode_xml_text(&path)).transpose()?,
        content: content
            .map(|content| decode_xml_text(trim_outer_line_breaks(&content)))
            .transpose()?,
    }))
}

/// `ToolReadOutputSchema`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn read_output(value: &JsValue) -> Parse<ReadOutput> {
    if let JsValue::String(text) = value {
        if let Some(xml) = parse_xml_read_output(text)? {
            return Ok(Some(xml));
        }
        return Ok(Some(ReadOutput {
            file_path: None,
            content: non_empty_string(Some(text)),
        }));
    }
    let content_only = |content| ReadOutput {
        file_path: None,
        content,
    };
    if is_read_chunk(value) {
        return Ok(Some(content_only(flatten_read_content(Some(value)))));
    }
    if let JsValue::Array(chunks) = value
        && chunks.iter().all(is_read_chunk)
    {
        return Ok(Some(content_only(flatten_read_content(Some(value)))));
    }
    if let Some(payload) = read_payload(value) {
        return Ok(Some(content_only(payload_content(payload))));
    }
    for key in ["data", "structuredContent", "structured_content"] {
        if let Some(payload) = object(value)
            .and_then(|record| field(record, key))
            .and_then(read_payload)
        {
            return Ok(Some(content_only(payload_content(payload))));
        }
    }
    Ok(None)
}

/// `ToolReadOutputPathSchema`: an object with a string `path`, `file_path`,
/// or `filePath` (tried in that order) and optional read content under
/// `content`, `text`, and `output`.
fn read_output_path(value: &JsValue) -> Option<ReadOutput> {
    let record = object(value)?;
    let keys = ["content", "text", "output"];
    ["path", "file_path", "filePath"].iter().find_map(|key| {
        let file_path = field(record, key).and_then(string)?;
        keys.iter()
            .all(|key| field(record, key).is_none_or(is_read_content))
            .then(|| ReadOutput {
                file_path: Some(file_path.to_owned()),
                content: payload_content(record),
            })
    })
}

/// `ToolReadOutputWithPathSchema`: [`read_output`], then the path-keyed
/// object the Codex parser also accepts.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn read_output_with_path(value: &JsValue) -> Parse<ReadOutput> {
    Ok(match read_output(value)? {
        Some(output) => Some(output),
        None => read_output_path(value),
    })
}

fn normalize_detail_path(file_path: Option<String>) -> Option<String> {
    let path = file_path?;
    let trimmed = js_trim(&path);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[must_use]
pub fn read_detail(input: Option<ReadInput>, output: Option<ReadOutput>) -> Option<JsValue> {
    let file_path = normalize_detail_path(
        input
            .as_ref()
            .map(|input| input.file_path.clone())
            .or_else(|| output.as_ref().and_then(|output| output.file_path.clone())),
    )?;
    let output_content = output.and_then(|output| output.content);
    let stripped = output_content
        .as_deref()
        .filter(|content| !content.is_empty())
        .and_then(|content| strip_read_line_number_gutter(Some(content)));
    let content = stripped
        .as_ref()
        .map(|(content, _)| content.clone())
        .or(output_content);
    let offset = input
        .as_ref()
        .and_then(|input| input.offset)
        .or_else(|| stripped.as_ref().and_then(|(_, start)| *start));
    let mut detail = JsObject::new();
    detail.insert("type", text("read"));
    detail.insert("filePath", text(&file_path));
    if let Some(content) = content.filter(|content| !content.is_empty()) {
        detail.insert("content", text(&content));
    }
    if let Some(offset) = offset {
        detail.insert("offset", JsValue::Number(offset));
    }
    if let Some(limit) = input.and_then(|input| input.limit) {
        detail.insert("limit", JsValue::Number(limit));
    }
    Some(JsValue::Object(detail))
}

// ---- write and edit -------------------------------------------------------

const WRITE_CONTENT_KEYS: [&str; 3] = ["content", "new_content", "newContent"];
const EDIT_TEXT_KEYS: [&str; 13] = [
    "old_string",
    "old_str",
    "oldContent",
    "old_content",
    "new_string",
    "new_str",
    "newContent",
    "new_content",
    "content",
    "patch",
    "diff",
    "unified_diff",
    "unifiedDiff",
];
const DIFF_KEYS: [&str; 4] = ["patch", "diff", "unified_diff", "unifiedDiff"];

/// `ToolPathInputSchema`: the first of `path`, `file_path`, `filePath`
/// holding a string.
fn path_input(record: &JsObject) -> Option<&str> {
    ["path", "file_path", "filePath"]
        .iter()
        .find_map(|key| field(record, key).and_then(string))
}

/// `z.intersection(ToolPathInputSchema, <text schema>)`: both sides must
/// parse, then `mergeValues` compares the `filePath` both sides carry.
fn path_intersection<'a>(value: &'a JsValue, text_keys: &[&str]) -> Parse<(String, &'a JsObject)> {
    let Some(record) = object(value) else {
        return Ok(None);
    };
    let Some(file_path) = path_input(record) else {
        return Ok(None);
    };
    if optional_strings(record, text_keys).is_none() {
        return Ok(None);
    }
    if let Some(right) = record.get("filePath")
        && right.as_str() != Some(file_path)
    {
        return Err(unmergeable());
    }
    Ok(Some((file_path.to_owned(), record)))
}

pub struct WriteValue {
    file_path: Option<String>,
    content: Option<String>,
}

fn write_content(record: &JsObject) -> Option<String> {
    first_non_empty(record, &WRITE_CONTENT_KEYS)
}

/// `ToolWriteInputSchema`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn write_input(value: &JsValue) -> Parse<WriteValue> {
    Ok(
        path_intersection(value, &WRITE_CONTENT_KEYS)?.map(|(file_path, record)| WriteValue {
            file_path: Some(file_path),
            content: write_content(record),
        }),
    )
}

/// `ToolWriteOutputSchema`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn write_output(value: &JsValue) -> Parse<WriteValue> {
    if value.is_string() {
        return Ok(Some(WriteValue {
            file_path: None,
            content: None,
        }));
    }
    if let Some((file_path, record)) = path_intersection(value, &WRITE_CONTENT_KEYS)? {
        return Ok(Some(WriteValue {
            file_path: Some(file_path),
            content: write_content(record),
        }));
    }
    Ok(object(value)
        .filter(|record| optional_strings(record, &WRITE_CONTENT_KEYS).is_some())
        .map(|record| WriteValue {
            file_path: None,
            content: write_content(record),
        }))
}

#[must_use]
pub fn write_detail(input: Option<WriteValue>, output: Option<WriteValue>) -> Option<JsValue> {
    let file_path = normalize_detail_path(
        input
            .as_ref()
            .and_then(|input| input.file_path.clone())
            .or_else(|| output.as_ref().and_then(|output| output.file_path.clone())),
    )?;
    let content = input
        .and_then(|input| input.content)
        .or_else(|| output.and_then(|output| output.content));
    let mut detail = JsObject::new();
    detail.insert("type", text("write"));
    detail.insert("filePath", text(&file_path));
    if let Some(content) = content {
        detail.insert("content", text(&content));
    }
    Some(JsValue::Object(detail))
}

#[derive(Default)]
pub struct EditValue {
    file_path: Option<String>,
    old_string: Option<String>,
    new_string: Option<String>,
    unified_diff: Option<String>,
}

fn diff_of(record: &JsObject) -> Option<String> {
    truncate_diff_text(first_non_empty(record, &DIFF_KEYS))
}

/// `ToolEditInputSchema`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn edit_input(value: &JsValue) -> Parse<EditValue> {
    Ok(
        path_intersection(value, &EDIT_TEXT_KEYS)?.map(|(file_path, record)| EditValue {
            file_path: Some(file_path),
            old_string: first_non_empty(
                record,
                &["old_string", "old_str", "oldContent", "old_content"],
            ),
            new_string: first_non_empty(
                record,
                &[
                    "new_string",
                    "new_str",
                    "newContent",
                    "new_content",
                    "content",
                ],
            ),
            unified_diff: diff_of(record),
        }),
    )
}

/// `ToolEditOutputFileSchema`.
fn edit_output_file(value: &JsValue) -> Option<EditValue> {
    let record = object(value)?;
    ["path", "file_path", "filePath"].iter().find_map(|key| {
        let file_path = field(record, key).and_then(string)?;
        optional_strings(record, &DIFF_KEYS)?;
        Some(EditValue {
            file_path: Some(file_path.to_owned()),
            unified_diff: diff_of(record),
            ..EditValue::default()
        })
    })
}

/// `ToolEditOutputSchema`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn edit_output(value: &JsValue) -> Parse<EditValue> {
    let output_new_string =
        |record: &JsObject| first_non_empty(record, &["newContent", "new_content", "content"]);
    if let Some((file_path, record)) = path_intersection(value, &EDIT_TEXT_KEYS)? {
        return Ok(Some(EditValue {
            file_path: Some(file_path),
            new_string: output_new_string(record),
            unified_diff: diff_of(record),
            old_string: None,
        }));
    }
    if let Some(files) = object(value)
        .and_then(|record| field(record, "files"))
        .and_then(JsValue::as_array)
        .filter(|files| !files.is_empty())
    {
        let parsed: Option<Vec<EditValue>> = files.iter().map(edit_output_file).collect();
        if let Some(first) = parsed.and_then(|parsed| parsed.into_iter().next()) {
            return Ok(Some(EditValue {
                file_path: first.file_path,
                unified_diff: first.unified_diff,
                ..EditValue::default()
            }));
        }
    }
    Ok(object(value)
        .filter(|record| optional_strings(record, &EDIT_TEXT_KEYS).is_some())
        .map(|record| EditValue {
            file_path: None,
            new_string: output_new_string(record),
            unified_diff: diff_of(record),
            old_string: None,
        }))
}

#[must_use]
pub fn edit_detail(input: Option<EditValue>, output: Option<&EditValue>) -> Option<JsValue> {
    let file_path = normalize_detail_path(
        input
            .as_ref()
            .and_then(|input| input.file_path.clone())
            .or_else(|| output.and_then(|output| output.file_path.clone())),
    )?;
    let new_string = input
        .as_ref()
        .and_then(|input| input.new_string.clone())
        .or_else(|| output.and_then(|output| output.new_string.clone()));
    let unified_diff = input
        .as_ref()
        .and_then(|input| input.unified_diff.clone())
        .or_else(|| {
            output
                .as_ref()
                .and_then(|output| output.unified_diff.clone())
        });
    let mut detail = JsObject::new();
    detail.insert("type", text("edit"));
    detail.insert("filePath", text(&file_path));
    if let Some(old) = input.and_then(|input| input.old_string) {
        detail.insert("oldString", text(&old));
    }
    if let Some(new_string) = new_string {
        detail.insert("newString", text(&new_string));
    }
    if let Some(diff) = unified_diff {
        detail.insert("unifiedDiff", text(&diff));
    }
    Some(JsValue::Object(detail))
}

// ---- search, fetch, skill, speak -------------------------------------------

/// `ToolSearchInputSchema`.
#[must_use]
pub fn search_input(value: &JsValue) -> Option<String> {
    let record = object(value)?;
    ["query", "q", "pattern"]
        .iter()
        .find_map(|key| field(record, key).and_then(string).map(str::to_owned))
}

/// `ToolGrepOutputSchema`.
pub fn is_grep_output(record: &JsObject) -> bool {
    let mode = optional(record, "mode", |value| {
        matches!(
            value.as_str(),
            Some("content" | "files_with_matches" | "count")
        )
        .then_some(())
    });
    mode.is_ok()
        && field(record, "numFiles")
            .and_then(nonnegative_int)
            .is_some()
        && field(record, "filenames").and_then(string_array).is_some()
        && optional(record, "content", string).is_ok()
        && ["numLines", "numMatches", "appliedLimit", "appliedOffset"]
            .iter()
            .all(|key| optional(record, key, nonnegative_int).is_ok())
}

/// `ToolGlobOutputSchema`.
pub fn glob_output(value: &JsValue) -> Option<JsObject> {
    let record = object(value)?;
    let valid = field(record, "durationMs").and_then(finite).is_some()
        && field(record, "numFiles")
            .and_then(nonnegative_int)
            .is_some()
        && field(record, "filenames").and_then(string_array).is_some()
        && field(record, "truncated")
            .and_then(JsValue::as_bool)
            .is_some();
    valid.then(|| record.clone())
}

/// A passthrough object with its shape keys moved first, as zod writes it.
/// zod 4.4.3 drops an own `__proto__` key instead of copying it.
fn passthrough(record: &JsObject, shape: &[&str]) -> JsValue {
    let mut out = JsObject::new();
    for key in shape {
        if let Some(value) = record.get(key) {
            out.insert(*key, value.clone());
        }
    }
    for (key, value) in record.iter() {
        if key != crate::json::PROTO_KEY && !shape.contains(&key) {
            out.insert(key, value.clone());
        }
    }
    JsValue::Object(out)
}

/// `ToolWebSearchOutputSchema`, with each hit in zod's passthrough order.
pub fn web_search_output(value: &JsValue) -> Option<JsObject> {
    let record = object(value)?;
    field(record, "query").and_then(string)?;
    field(record, "durationSeconds").and_then(finite)?;
    let mut results = Vec::new();
    for entry in field(record, "results")?.as_array()? {
        if entry.is_string() {
            results.push(entry.clone());
            continue;
        }
        let entry_record = object(entry)?;
        field(entry_record, "tool_use_id").and_then(string)?;
        let mut hits = Vec::new();
        for hit in field(entry_record, "content")?.as_array()? {
            let hit_record = object(hit)?;
            field(hit_record, "title").and_then(string)?;
            field(hit_record, "url").and_then(string)?;
            hits.push(passthrough(hit_record, &["title", "url"]));
        }
        let mut parsed = object(&passthrough(entry_record, &["tool_use_id", "content"]))?.clone();
        parsed.insert("content", JsValue::Array(hits));
        results.push(JsValue::Object(parsed));
    }
    let mut parsed = record.clone();
    parsed.insert("results", JsValue::Array(results));
    Some(parsed)
}

fn get<'a>(record: &'a JsObject, key: &str) -> Option<&'a JsValue> {
    record.get(key)
}

/// `buildSearchToolDetailOutputFields(output)`.
fn search_output_fields(detail: &mut JsObject, output: Option<&JsObject>) {
    let Some(output) = output else {
        return;
    };
    let has = |key: &str| output.get(key).is_some();
    let truthy = |key: &str| crate::js::truthy(output.get(key));
    let set = |detail: &mut JsObject, key: &str, value: Option<&JsValue>| {
        detail.insert(key, value.cloned().unwrap_or(JsValue::Undefined));
    };
    let file_paths = || {
        get(output, "filenames")
            .filter(|names| names.as_array().is_some_and(|names| !names.is_empty()))
            .cloned()
    };
    if has("filenames") && !has("truncated") {
        if truthy("content") {
            set(detail, "content", get(output, "content"));
        }
        if let Some(paths) = file_paths() {
            detail.insert("filePaths", paths);
        }
        set(detail, "numFiles", get(output, "numFiles"));
        if get(output, "numMatches").is_some_and(|value| !matches!(value, JsValue::Undefined)) {
            set(detail, "numMatches", get(output, "numMatches"));
        }
        if truthy("mode") {
            set(detail, "mode", get(output, "mode"));
        }
    } else if has("truncated") {
        if let Some(paths) = file_paths() {
            detail.insert("filePaths", paths);
        }
        set(detail, "numFiles", get(output, "numFiles"));
        set(detail, "durationMs", get(output, "durationMs"));
        set(detail, "truncated", get(output, "truncated"));
    } else if has("results") {
        let results = get(output, "results")
            .and_then(JsValue::as_array)
            .unwrap_or_default();
        let web_results: Vec<JsValue> = results
            .iter()
            .filter(|entry| !entry.is_string())
            .flat_map(|entry| {
                entry
                    .get("content")
                    .and_then(JsValue::as_array)
                    .map(<[JsValue]>::to_vec)
                    .unwrap_or_default()
            })
            .collect();
        let annotations: Vec<JsValue> = results
            .iter()
            .filter(|entry| entry.is_string())
            .cloned()
            .collect();
        if !web_results.is_empty() {
            detail.insert("webResults", JsValue::Array(web_results));
        }
        if !annotations.is_empty() {
            detail.insert("annotations", JsValue::Array(annotations));
        }
        set(detail, "durationSeconds", get(output, "durationSeconds"));
    }
}

#[must_use]
pub fn search_detail(
    query: Option<String>,
    output: Option<&JsObject>,
    tool: &str,
) -> Option<JsValue> {
    let query = query.filter(|query| !query.is_empty())?;
    let mut detail = JsObject::new();
    detail.insert("type", text("search"));
    detail.insert("query", text(&query));
    detail.insert("toolName", text(tool));
    search_output_fields(&mut detail, output);
    Some(JsValue::Object(detail))
}

/// `ToolWebFetchInputSchema`.
pub fn fetch_input(value: &JsValue) -> Option<JsObject> {
    let record = object(value)?;
    field(record, "url").and_then(string)?;
    field(record, "prompt").and_then(string)?;
    Some(record.clone())
}

/// `ToolWebFetchOutputSchema`.
pub fn fetch_output(value: &JsValue) -> Option<JsObject> {
    let record = object(value)?;
    let valid = field(record, "bytes").and_then(nonnegative_int).is_some()
        && field(record, "code").and_then(int).is_some()
        && field(record, "codeText").and_then(string).is_some()
        && field(record, "result").and_then(string).is_some()
        && field(record, "durationMs").and_then(finite).is_some()
        && field(record, "url").and_then(string).is_some();
    valid.then(|| record.clone())
}

fn member<'a>(record: Option<&'a JsObject>, key: &str) -> Option<&'a JsValue> {
    record.and_then(|record| record.get(key))
}

#[must_use]
pub fn fetch_detail(input: Option<&JsObject>, output: Option<&JsObject>) -> Option<JsValue> {
    let read = member;
    let url = read(input, "url").or_else(|| read(output, "url"))?;
    if !crate::js::truthy(Some(url)) {
        return None;
    }
    let mut detail = JsObject::new();
    detail.insert("type", text("fetch"));
    detail.insert("url", url.clone());
    let put_truthy = |detail: &mut JsObject, key: &str, value: Option<&JsValue>| {
        if crate::js::truthy(value) {
            detail.insert(key, value.cloned().unwrap_or(JsValue::Undefined));
        }
    };
    put_truthy(&mut detail, "prompt", read(input, "prompt"));
    put_truthy(&mut detail, "result", read(output, "result"));
    if let Some(code) = read(output, "code") {
        detail.insert("code", code.clone());
    }
    put_truthy(&mut detail, "codeText", read(output, "codeText"));
    if let Some(bytes) = read(output, "bytes") {
        detail.insert("bytes", bytes.clone());
    }
    if let Some(duration) = read(output, "durationMs") {
        detail.insert("durationMs", duration.clone());
    }
    Some(JsValue::Object(detail))
}
#[must_use]
pub fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// A `z.nullable(schema)` member: `null` is `Some(None)`.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
fn nullable<T>(value: &JsValue, parse: impl FnOnce(&JsValue) -> Parse<T>) -> Parse<Option<T>> {
    if value.is_null() {
        return Ok(Some(None));
    }
    Ok(parse(value)?.map(Some))
}

pub fn infallible<T>(
    parse: impl FnOnce(&JsValue) -> Option<T>,
) -> impl FnOnce(&JsValue) -> Parse<T> {
    move |value| Ok(parse(value))
}

/// Parses a branch's `input` then `output` (zod checks every shape key, so
/// both run even when the name does not match); `None` when either fails.
///
/// # Errors
///
/// The baseline's throws described in the module documentation.
pub fn parse_pair<I, O>(
    input: &JsValue,
    output: &JsValue,
    parse_input: impl FnOnce(&JsValue) -> Parse<I>,
    parse_output: impl FnOnce(&JsValue) -> Parse<O>,
) -> Parse<(Option<I>, Option<O>)> {
    let left = nullable(input, parse_input)?;
    let right = nullable(output, parse_output)?;
    Ok(left.zip(right))
}

const SHELL_ENVELOPE_HEADER_PREFIXES: [&str; 4] = [
    "chunk id:",
    "wall time:",
    "process exited with code",
    "original token count:",
];

fn is_shell_envelope_header(line: &str) -> bool {
    let normalized = js_trim(line).to_lowercase();
    SHELL_ENVELOPE_HEADER_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

fn looks_like_shell_envelope(lines: &[&str]) -> bool {
    let Some(first) = lines.first() else {
        return false;
    };
    if !js_trim(first).to_lowercase().starts_with("chunk id:") {
        return false;
    }
    let window: Vec<String> = lines
        .iter()
        .take(8)
        .map(|line| js_trim(line).to_lowercase())
        .collect();
    window.iter().any(|line| line.starts_with("wall time:"))
        && window
            .iter()
            .any(|line| line.starts_with("process exited with code"))
}

/// `extractCodexShellOutput(value)`: strips Codex's unified-exec envelope.
#[must_use]
pub fn extract_codex_shell_output(value: Option<&str>) -> Option<String> {
    let text = value.filter(|text| !text.is_empty())?;
    let normalized = text.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    if !looks_like_shell_envelope(&lines) {
        return Some(text.to_owned());
    }
    if let Some(index) = lines.iter().position(|line| js_trim(line) == "Output:") {
        let body = lines[index + 1..].join("\n");
        return (!body.is_empty()).then_some(body);
    }
    let first_body = (1..lines.len()).find(|index| !is_shell_envelope_header(lines[*index]))?;
    let body = lines[first_body..].join("\n");
    (!body.is_empty()).then_some(body)
}
