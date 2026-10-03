//! `tool-call-detail-parser.ts` (`deriveClaudeToolDetail`).
//!
//! The schemas, mappers, and helpers it shares with other providers
//! (`tool-call-detail-primitives.ts`, `tool-call-mapper-utils.ts`) live in
//! [`spocky_contracts::tool_detail`], where the baseline throws that escape
//! `safeParse` are described; they surface here as [`AgentError`].

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_contracts::text::js_trim;
use spocky_contracts::tool_detail::{
    Parse, Thrown, edit_detail, edit_input, edit_output, fetch_detail, fetch_input, fetch_output,
    field, glob_output, infallible, is_grep_output, object, parse_pair, read_detail, read_input,
    read_output, search_detail, search_input, shell_detail, shell_input, shell_output, string,
    text, web_search_output, write_detail, write_input, write_output,
};
use spocky_session::agent_sdk::AgentError;

/// `ClaudeGrepOutputSchema` without its `.nullable()`: the parsed output
/// object (the input itself for the grep shape, a built one for `{output}`).
fn claude_grep_output(value: &JsValue) -> Option<JsObject> {
    let record = object(value)?;
    if is_grep_output(record) {
        return Some(record.clone());
    }
    let output = field(record, "output").and_then(string)?;
    let mut built = JsObject::new();
    built.insert("numFiles", JsValue::Number(0.0));
    built.insert("filenames", JsValue::Array(Vec::new()));
    built.insert("content", text(output));
    Some(built)
}

/// The `Skill` output schema: `{ output: string }` or a string.
fn skill_output(value: &JsValue) -> Option<String> {
    match value {
        JsValue::String(text) => Some(text.clone()),
        other => object(other)
            .and_then(|record| field(record, "output"))
            .and_then(string)
            .map(str::to_owned),
    }
}

fn skill_detail(input: Option<&JsObject>, output: Option<String>) -> Option<JsValue> {
    let skill = input
        .and_then(|input| field(input, "skill"))
        .and_then(string)
        .filter(|skill| !skill.is_empty())?;
    let mut detail = JsObject::new();
    detail.insert("type", text("plain_text"));
    detail.insert("label", text(skill));
    detail.insert("icon", text("sparkles"));
    if let Some(output) = output.filter(|output| !output.is_empty()) {
        detail.insert("text", text(&output));
    }
    Some(JsValue::Object(detail))
}

/// The speak input schema: a string or `{ text: string }`, as its text.
fn speak_input(value: &JsValue) -> Option<String> {
    match value {
        JsValue::String(text) => Some(text.clone()),
        other => object(other)
            .and_then(|record| field(record, "text"))
            .and_then(string)
            .map(str::to_owned),
    }
}

#[derive(Clone, Copy)]
enum Branch {
    Shell,
    Read,
    Write,
    Edit,
    WebSearch,
    Search,
    Grep,
    Glob,
    Fetch,
    Skill,
}

/// `ClaudeToolDetailPass2Schema` branch names in union order (the speak
/// branch follows them).
const BRANCHES: [(&str, Branch); 32] = [
    ("Bash", Branch::Shell),
    ("bash", Branch::Shell),
    ("shell", Branch::Shell),
    ("exec_command", Branch::Shell),
    ("Read", Branch::Read),
    ("read", Branch::Read),
    ("read_file", Branch::Read),
    ("view_file", Branch::Read),
    ("Write", Branch::Write),
    ("write", Branch::Write),
    ("write_file", Branch::Write),
    ("create_file", Branch::Write),
    ("Edit", Branch::Edit),
    ("MultiEdit", Branch::Edit),
    ("multi_edit", Branch::Edit),
    ("edit", Branch::Edit),
    ("apply_patch", Branch::Edit),
    ("apply_diff", Branch::Edit),
    ("str_replace_editor", Branch::Edit),
    ("WebSearch", Branch::WebSearch),
    ("web_search", Branch::WebSearch),
    ("search", Branch::Search),
    ("Grep", Branch::Grep),
    ("grep", Branch::Grep),
    ("Glob", Branch::Glob),
    ("glob", Branch::Glob),
    ("WebFetch", Branch::Fetch),
    ("web_fetch", Branch::Fetch),
    ("WebFetchTool", Branch::Fetch),
    ("web_fetch_tool", Branch::Fetch),
    ("webfetch", Branch::Fetch),
    ("Skill", Branch::Skill),
];

/// One union branch: `Ok(None)` when zod rejects it, `Ok(Some(detail))`
/// with the mapper's result (`None` for `undefined`) when it parses.
fn run_branch(
    branch: Branch,
    name_matches: bool,
    input: &JsValue,
    output: &JsValue,
) -> Parse<Option<JsValue>> {
    let detail = match branch {
        Branch::Shell => parse_pair(
            input,
            output,
            infallible(shell_input),
            infallible(shell_output),
        )?
        .map(|(input, output)| shell_detail(input, output)),
        Branch::Read => match parse_pair(input, output, infallible(read_input), |_: &JsValue| {
            Ok(Some(()))
        })? {
            Some((parsed_input, _)) if name_matches => {
                let parsed_output = if output.is_null() {
                    None
                } else {
                    read_output(output)?
                };
                Some(read_detail(parsed_input, parsed_output))
            }
            Some(_) | None => None,
        },
        Branch::Write => parse_pair(input, output, write_input, write_output)?
            .map(|(input, output)| write_detail(input, output)),
        Branch::Edit => parse_pair(input, output, edit_input, edit_output)?
            .map(|(input, output)| edit_detail(input, output.as_ref())),
        Branch::WebSearch => parse_pair(
            input,
            output,
            infallible(search_input),
            infallible(web_search_output),
        )?
        .map(|(query, output)| search_detail(query, output.as_ref(), "web_search")),
        Branch::Search => parse_pair(input, output, infallible(search_input), |_: &JsValue| {
            Ok(Some(()))
        })?
        .map(|(query, _)| search_detail(query, None, "search")),
        Branch::Grep => parse_pair(
            input,
            output,
            infallible(search_input),
            infallible(claude_grep_output),
        )?
        .map(|(query, output)| search_detail(query, output.as_ref(), "grep")),
        Branch::Glob => parse_pair(
            input,
            output,
            infallible(search_input),
            infallible(glob_output),
        )?
        .map(|(query, output)| search_detail(query, output.as_ref(), "glob")),
        Branch::Fetch => parse_pair(
            input,
            output,
            infallible(fetch_input),
            infallible(fetch_output),
        )?
        .map(|(input, output)| fetch_detail(input.as_ref(), output.as_ref())),
        Branch::Skill => parse_pair(
            input,
            output,
            infallible(|value: &JsValue| {
                let record = object(value)?;
                field(record, "skill").and_then(string)?;
                Some(record.clone())
            }),
            infallible(skill_output),
        )?
        .map(|(input, output)| skill_detail(input.as_ref(), output)),
    };
    Ok(detail.filter(|_| name_matches))
}

fn unknown_detail(input: JsValue, output: JsValue) -> JsValue {
    let mut detail = JsObject::new();
    detail.insert("type", text("unknown"));
    detail.insert("input", input);
    detail.insert("output", output);
    JsValue::Object(detail)
}

/// `deriveClaudeToolDetail(name, input, output)`; `None` input or output
/// is `undefined`.
///
/// # Errors
///
/// The baseline's throws described in [`spocky_contracts::tool_detail`].
pub fn derive_claude_tool_detail(
    name: &str,
    input: Option<&JsValue>,
    output: Option<&JsValue>,
) -> Result<JsValue, AgentError> {
    derive(name, input, output).map_err(|Thrown { name, message }| AgentError { name, message })
}

fn derive(
    name: &str,
    input: Option<&JsValue>,
    output: Option<&JsValue>,
) -> Result<JsValue, Thrown> {
    let nullish = |value: Option<&JsValue>| match value {
        None | Some(JsValue::Undefined) => JsValue::Null,
        Some(value) => value.clone(),
    };
    let input = nullish(input);
    let output = nullish(output);
    if name.is_empty() {
        return Ok(unknown_detail(input, output));
    }
    for (branch_name, branch) in BRANCHES {
        if let Some(result) = run_branch(branch, name == branch_name, &input, &output)? {
            return Ok(result.unwrap_or_else(|| unknown_detail(input.clone(), output.clone())));
        }
    }
    if name == "speak" {
        let parsed_input = if input.is_null() {
            Some(None)
        } else {
            speak_input(&input).map(Some)
        };
        if let Some(parsed_input) = parsed_input {
            let spoken = parsed_input
                .as_deref()
                .map(js_trim)
                .unwrap_or_default()
                .to_owned();
            if !spoken.is_empty() {
                return Ok(unknown_detail(text(&spoken), JsValue::Null));
            }
        }
    }
    Ok(unknown_detail(input, output))
}
