//! `subagents/workflow-output.ts`: the result text of a finished Claude
//! workflow, read from its task output file.

use std::io::Read;

use spocky_contracts::js_value::{
    JsTextUnit, JsValue, js_text_from_utf16, js_text_units, js_text_utf16, parse, stringify_pretty,
};

const MAX_WORKFLOW_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_WORKFLOW_RESULT_CHARS: usize = 100_000;

/// `readClaudeWorkflowResultFile(outputFile)`.
#[must_use]
pub fn read_claude_workflow_result_file(output_file: &str) -> Option<String> {
    let file = std::fs::File::open(output_file).ok()?;
    let mut buffer = Vec::new();
    file.take(u64::try_from(MAX_WORKFLOW_OUTPUT_BYTES + 1).ok()?)
        .read_to_end(&mut buffer)
        .ok()?;
    if buffer.len() > MAX_WORKFLOW_OUTPUT_BYTES {
        return None;
    }
    parse_claude_workflow_result(&String::from_utf8_lossy(&buffer))
}

/// `parseClaudeWorkflowResult(contents)`.
#[must_use]
pub fn parse_claude_workflow_result(contents: &str) -> Option<String> {
    // `Buffer.byteLength(contents, "utf8")`: a lone surrogate is written as
    // U+FFFD, three bytes.
    let byte_length: usize = js_text_units(contents)
        .map(|unit| match unit {
            JsTextUnit::Char(character) => character.len_utf8(),
            JsTextUnit::LoneSurrogate(_) => 3,
        })
        .sum();
    if byte_length > MAX_WORKFLOW_OUTPUT_BYTES {
        return None;
    }
    let parsed = parse(contents).ok()?;
    let record = parsed.as_object()?;
    record.get("result").and_then(format_claude_workflow_result)
}

fn unwrap_single_value(value: &JsValue) -> &JsValue {
    let mut current = value;
    for _ in 0..8 {
        let Some(record) = current.as_object() else {
            return current;
        };
        if record.len() != 1 {
            return current;
        }
        match record.iter().next() {
            Some((_, inner)) => current = inner,
            None => return current,
        }
    }
    current
}

/// `formatClaudeWorkflowResult(result)`.
#[must_use]
pub fn format_claude_workflow_result(result: &JsValue) -> Option<String> {
    let text = match unwrap_single_value(result) {
        JsValue::String(text) => spocky_contracts::text::js_trim(text).to_owned(),
        value @ (JsValue::Number(_) | JsValue::Bool(_)) => {
            spocky_contracts::js::js_string(Some(value))
        }
        JsValue::Null | JsValue::Undefined => return None,
        value => format!("```json\n{}\n```", stringify_pretty(value)),
    };
    if text.is_empty() {
        return None;
    }
    let units: Vec<u16> = js_text_utf16(&text).collect();
    if units.len() <= MAX_WORKFLOW_RESULT_CHARS {
        return Some(text);
    }
    Some(format!(
        "{}\n\n…",
        js_text_from_utf16(&units[..MAX_WORKFLOW_RESULT_CHARS])
    ))
}
