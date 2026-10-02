//! Codex tool calls to Paseo `tool_call` timeline items.
//!
//! Port of the shell path of pinned Paseo `codex/tool-call-mapper.ts`,
//! `codex/tool-call-detail-parser.ts`, `tool-call-detail-primitives.ts`, and
//! `tool-call-mapper-utils.ts`, plus `mapCodexExecNotificationToToolCall` and
//! `normalizeCodexCommandValue` from `codex-app-server-agent.ts`.
//!
//! Slice scope: `commandExecution` items and shell envelopes map here.
//! `fileChange`, `mcpToolCall`, `webSearch`, `collabAgentToolCall`, and
//! `subAgentActivity` items, and detail branches other than shell (read,
//! write, edit, search, speak), return [`ToolMapping::Unported`].
//!
//! Paseo's regular expressions are matched by hand with the JavaScript `\s`
//! set from `spocky-contracts`, since Rust `char::is_whitespace` differs.

use serde_json::{Map, Value, json};
use spocky_contracts::js_value::array_index;
use spocky_contracts::text::{is_js_whitespace, js_length, js_trim};

use crate::transport::js_truthy;

/// Result of mapping a Codex tool item or envelope.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolMapping {
    /// Paseo emits this `tool_call` item.
    Item(Value),
    /// Paseo emits nothing.
    Skip,
    /// Paseo maps this through a branch not yet ported.
    Unported(String),
}

fn nullish(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null))
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text),
        _ => None,
    }
}

const FAILED_STATUS_VOCAB: [&str; 6] = [
    "failed", "failure", "error", "errored", "rejected", "denied",
];
const CANCELED_STATUS_VOCAB: [&str; 4] = ["canceled", "cancelled", "interrupted", "aborted"];
const COMPLETED_STATUS_VOCAB: [&str; 5] = ["completed", "complete", "done", "success", "succeeded"];

/// `normalizeToolCallStatus(rawStatus, error, output)`.
#[must_use]
pub fn normalize_tool_call_status(
    raw_status: Option<&str>,
    error: Option<&Value>,
    output: Option<&Value>,
) -> &'static str {
    if !nullish(error) {
        return "failed";
    }
    if let Some(raw) = raw_status {
        let normalized = js_trim(raw).to_lowercase();
        if !normalized.is_empty() {
            if FAILED_STATUS_VOCAB.contains(&normalized.as_str()) {
                return "failed";
            }
            if CANCELED_STATUS_VOCAB.contains(&normalized.as_str()) {
                return "canceled";
            }
            if COMPLETED_STATUS_VOCAB.contains(&normalized.as_str()) {
                return "completed";
            }
            return "running";
        }
    }
    if nullish(output) {
        "running"
    } else {
        "completed"
    }
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

fn strip_matching_edge_quotes(value: &str) -> &str {
    let quoted =
        |quote: char| value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote);
    if quoted('"') || quoted('\'') {
        &value[1..value.len() - 1]
    } else if value == "\"" || value == "'" {
        // `"x".slice(1, -1)` on a one-character string is empty.
        ""
    } else {
        value
    }
}

/// Length in bytes of the leading JS whitespace run.
fn whitespace_run(text: &str) -> usize {
    text.char_indices()
        .find(|(_, character)| !is_js_whitespace(*character))
        .map_or(text.len(), |(index, _)| index)
}

/// `\s+-(?:lc|c)\s+([\s\S]+)$` applied after the shell name; returns group 1.
fn shell_flag_tail<'a>(rest: &'a str, flags: &[&str]) -> Option<&'a str> {
    let gap = whitespace_run(rest);
    if gap == 0 {
        return None;
    }
    let after_gap = &rest[gap..];
    let after_dash = after_gap.strip_prefix('-')?;
    for flag in flags {
        let Some(after_flag) = after_dash.strip_prefix(flag) else {
            continue;
        };
        let run = whitespace_run(after_flag);
        if run == 0 {
            continue;
        }
        let tail = &after_flag[run..];
        if !tail.is_empty() {
            return Some(tail);
        }
        // `\s+` gives back its last character so `[\s\S]+` can match.
        let last = after_flag[..run].chars().last()?;
        if run > last.len_utf8() {
            return Some(&after_flag[run - last.len_utf8()..run]);
        }
    }
    None
}

/// `^(?:(?:\/[^/\s]+)*\/)?(?:zsh|bash|sh)\s+-(?:lc|c)\s+([\s\S]+)$`.
fn unix_shell_wrapper(trimmed: &str) -> Option<&str> {
    let head_end = trimmed
        .char_indices()
        .find(|(_, character)| is_js_whitespace(*character))
        .map_or(trimmed.len(), |(index, _)| index);
    let head = &trimmed[..head_end];
    let name = match head.rfind('/') {
        Some(slash) => {
            let prefix = &head[..=slash];
            let segments: Vec<&str> = prefix.split('/').collect();
            let valid = segments.first() == Some(&"")
                && segments.last() == Some(&"")
                && segments[1..segments.len() - 1]
                    .iter()
                    .all(|segment| !segment.is_empty());
            if !valid {
                return None;
            }
            &head[slash + 1..]
        }
        None => head,
    };
    if !matches!(name, "zsh" | "bash" | "sh") {
        return None;
    }
    shell_flag_tail(&trimmed[head_end..], &["lc", "c"])
}

fn ascii_starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// The remainder after `(?:\s+-[A-Za-z]+(?:\s+[^-\s][^\s]*)?)*\s+(?:-Command|-c|\/c)\s+`,
/// tried in backtracking order; returns group 1.
fn windows_args_tail(rest: &str) -> Option<&str> {
    // The greedy repetition is tried first; the terminal flag only after
    // every repetition choice fails, as a backtracking engine does.
    let gap = whitespace_run(rest);
    if gap > 0 {
        let after_gap = &rest[gap..];
        if let Some(after_dash) = after_gap.strip_prefix('-') {
            let letters = after_dash
                .bytes()
                .take_while(u8::is_ascii_alphabetic)
                .count();
            // Greedy `-[A-Za-z]+` may end before the full letter run.
            for take in (1..=letters).rev() {
                let after_letters = &after_dash[take..];
                // With an argument.
                let arg_gap = whitespace_run(after_letters);
                if arg_gap > 0 {
                    let arg = &after_letters[arg_gap..];
                    if let Some(first) = arg.chars().next()
                        && first != '-'
                        && !is_js_whitespace(first)
                    {
                        let arg_len = arg
                            .char_indices()
                            .find(|(_, character)| is_js_whitespace(*character))
                            .map_or(arg.len(), |(index, _)| index);
                        for end in (first.len_utf8()..=arg_len).rev() {
                            if !arg.is_char_boundary(end) {
                                continue;
                            }
                            if let Some(tail) = windows_args_tail(&arg[end..]) {
                                return Some(tail);
                            }
                        }
                    }
                }
                // Without an argument.
                if let Some(tail) = windows_args_tail(after_letters) {
                    return Some(tail);
                }
            }
        }
    }
    windows_terminal_flag(rest)
}

fn windows_terminal_flag(rest: &str) -> Option<&str> {
    let gap = whitespace_run(rest);
    if gap == 0 {
        return None;
    }
    let after_gap = &rest[gap..];
    for flag in ["-Command", "-c", "/c"] {
        if !ascii_starts_with_ignore_case(after_gap, flag) {
            continue;
        }
        let after_flag = &after_gap[flag.len()..];
        let run = whitespace_run(after_flag);
        if run == 0 {
            continue;
        }
        let tail = &after_flag[run..];
        if !tail.is_empty() {
            return Some(tail);
        }
        let last = after_flag[..run].chars().last()?;
        if run > last.len_utf8() {
            return Some(&after_flag[run - last.len_utf8()..run]);
        }
    }
    None
}

/// `^(?:"[^"]*\\)?(?:pwsh|powershell|cmd)(?:\.exe)?"?` then the argument
/// tail, case-insensitive.
fn windows_shell_wrapper(trimmed: &str) -> Option<&str> {
    let mut starts = Vec::new();
    if let Some(after_quote) = trimmed.strip_prefix('"') {
        let run = after_quote.find('"').unwrap_or(after_quote.len());
        let mut backslashes: Vec<usize> = after_quote[..run]
            .match_indices('\\')
            .map(|(index, _)| 1 + index + 1)
            .collect();
        backslashes.reverse();
        starts.extend(backslashes);
    }
    starts.push(0);
    for start in starts {
        let rest = &trimmed[start..];
        for name in ["pwsh", "powershell", "cmd"] {
            if !ascii_starts_with_ignore_case(rest, name) {
                continue;
            }
            let after_name = &rest[name.len()..];
            let mut candidates = Vec::new();
            for exe in [true, false] {
                let after_exe = if exe {
                    if !ascii_starts_with_ignore_case(after_name, ".exe") {
                        continue;
                    }
                    &after_name[4..]
                } else {
                    after_name
                };
                if let Some(after_quote) = after_exe.strip_prefix('"') {
                    candidates.push(after_quote);
                }
                candidates.push(after_exe);
            }
            for candidate in candidates {
                if let Some(tail) = windows_args_tail(candidate) {
                    return Some(tail);
                }
            }
        }
    }
    None
}

/// `unwrapShellCommand(command)` from the tool-call mapper.
#[must_use]
pub fn unwrap_shell_command(command: &str) -> String {
    let trimmed = js_trim(command);
    if let Some(group) = unix_shell_wrapper(trimmed).filter(|group| !group.is_empty()) {
        return strip_matching_edge_quotes(js_trim(group)).to_owned();
    }
    match windows_shell_wrapper(trimmed).filter(|group| !group.is_empty()) {
        Some(group) => strip_matching_edge_quotes(js_trim(group)).to_owned(),
        None => trimmed.to_owned(),
    }
}

/// `/(?:^|\\)(?:pwsh|powershell|cmd)(?:\.exe)?$/i` after stripping one
/// leading and one trailing quote.
fn is_windows_shell_command(command: &str) -> bool {
    let mut normalized = command;
    if let Some(rest) = normalized.strip_prefix(['"', '\'']) {
        normalized = rest;
    }
    if let Some(rest) = normalized.strip_suffix(['"', '\'']) {
        normalized = rest;
    }
    let lower = normalized.to_ascii_lowercase();
    let base = lower.strip_suffix(".exe").unwrap_or(&lower);
    ["pwsh", "powershell", "cmd"].iter().any(|name| {
        base.ends_with(name) && {
            let before = &base[..base.len() - name.len()];
            before.is_empty() || before.ends_with('\\')
        }
    })
}

/// `normalizeCommandExecutionCommand(value)`.
#[must_use]
pub fn normalize_command_execution_command(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(command)) => {
            let normalized = unwrap_shell_command(command);
            (!normalized.is_empty()).then_some(normalized)
        }
        Some(Value::Array(entries)) => {
            let parts: Vec<&str> = entries
                .iter()
                .filter_map(Value::as_str)
                .map(js_trim)
                .filter(|part| !part.is_empty())
                .collect();
            if parts.is_empty() {
                return None;
            }
            if parts.len() >= 3 && (parts[1] == "-lc" || parts[1] == "-c") {
                let unwrapped = js_trim(parts[2]);
                return (!unwrapped.is_empty()).then(|| unwrapped.to_owned());
            }
            if parts.len() >= 3
                && is_windows_shell_command(parts[0])
                && ["-command", "-c", "/c"].contains(&parts[1].to_ascii_lowercase().as_str())
            {
                let unwrapped = parts[2..].join(" ");
                let unwrapped = js_trim(&unwrapped);
                return (!unwrapped.is_empty())
                    .then(|| strip_matching_edge_quotes(unwrapped).to_owned());
            }
            Some(parts.join(" "))
        }
        _ => None,
    }
}

/// `normalizeCodexCommandValue(value)` from `codex-app-server-agent.ts`:
/// `^(?:\/bin\/)?(?:zsh|bash|sh)\s+-(?:lc|c)\s+([\s\S]+)$` unwrapping.
#[must_use]
pub fn normalize_codex_command_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(command) => {
            let trimmed = js_trim(command);
            if trimmed.is_empty() {
                return None;
            }
            let without_bin = trimmed.strip_prefix("/bin/").unwrap_or(trimmed);
            let attempts = if without_bin.len() == trimmed.len() {
                vec![trimmed]
            } else {
                vec![without_bin, trimmed]
            };
            let mut candidate = None;
            for attempt in attempts {
                for name in ["zsh", "bash", "sh"] {
                    if let Some(rest) = attempt.strip_prefix(name)
                        && let Some(group) = shell_flag_tail(rest, &["lc", "c"])
                    {
                        candidate = Some(group);
                        break;
                    }
                }
                if candidate.is_some() {
                    break;
                }
            }
            let Some(group) = candidate else {
                return Some(json!(trimmed));
            };
            let group = js_trim(group);
            if group.is_empty() {
                return Some(json!(trimmed));
            }
            let quoted = |quote: char| group.starts_with(quote) && group.ends_with(quote);
            if quoted('"') || quoted('\'') {
                return Some(json!(strip_matching_edge_quotes(group)));
            }
            Some(json!(group))
        }
        Value::Array(entries) => {
            let parts: Vec<&str> = entries
                .iter()
                .filter_map(Value::as_str)
                .map(js_trim)
                .filter(|part| !part.is_empty())
                .collect();
            if parts.is_empty() {
                return None;
            }
            if parts.len() >= 3 && (parts[1] == "-lc" || parts[1] == "-c") {
                return Some(json!(parts[2]));
            }
            Some(json!(parts))
        }
        _ => None,
    }
}

const SHELL_NAMES: [&str; 6] = ["Bash", "shell", "bash", "exec", "exec_command", "command"];
const OTHER_DETAIL_BRANCHES: [&str; 11] = [
    "read",
    "read_file",
    "write",
    "write_file",
    "create_file",
    "edit",
    "apply_patch",
    "apply_diff",
    "search",
    "web_search",
    "speak",
];

/// Parsed `ToolShellInputSchema` output.
struct ShellInput {
    command: Option<String>,
    cwd: Option<String>,
}

/// `ToolShellInputSchema`; `Err` when the union rejects the value.
fn parse_shell_input(input: &Value) -> Result<Option<ShellInput>, ()> {
    let record = match input {
        Value::Null => return Ok(None),
        Value::Object(record) => record,
        _ => return Err(()),
    };
    let command_ok = |value: Option<&Value>| match value {
        Some(Value::String(_)) => true,
        Some(Value::Array(items)) => items.iter().all(Value::is_string),
        _ => false,
    };
    let optional_string_ok = |key: &str| matches!(record.get(key), None | Some(Value::String(_)));
    let fields_ok = optional_string_ok("cwd") && optional_string_ok("directory");
    let accepted =
        fields_ok && (command_ok(record.get("command")) || command_ok(record.get("cmd")));
    if !accepted {
        return Err(());
    }
    let raw = if record.contains_key("command") {
        record.get("command")
    } else {
        record.get("cmd")
    };
    let command = match raw {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            let joined = items
                .iter()
                .filter_map(Value::as_str)
                .map(js_trim)
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    };
    let cwd = non_empty_str(record.get("cwd"))
        .or_else(|| non_empty_str(record.get("directory")))
        .map(str::to_owned);
    Ok(Some(ShellInput { command, cwd }))
}

/// Parsed `ToolShellOutputSchema` output.
struct ShellOutput {
    command: Option<String>,
    output: Option<String>,
    exit_code: Option<Value>,
}

fn strings_ok(record: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter()
        .all(|key| matches!(record.get(*key), None | Some(Value::String(_))))
}

fn exit_codes_ok(record: &Map<String, Value>) -> bool {
    ["exitCode", "exit_code"].iter().all(|key| {
        matches!(
            record.get(*key),
            None | Some(Value::Null | Value::Number(_))
        )
    })
}

fn nested_ok(
    record: &Map<String, Value>,
    key: &str,
    check: &dyn Fn(&Map<String, Value>) -> bool,
) -> bool {
    match record.get(key) {
        None => true,
        Some(Value::Object(nested)) => check(nested),
        Some(_) => false,
    }
}

/// `ToolShellOutputSchema`; `Err` when the union rejects the value.
fn parse_shell_output(output: &Value) -> Result<Option<ShellOutput>, ()> {
    let record = match output {
        Value::Null => return Ok(None),
        Value::String(text) => {
            return Ok(Some(ShellOutput {
                command: None,
                output: extract_codex_shell_output(Some(text)),
                exit_code: None,
            }));
        }
        Value::Object(record) => record,
        _ => return Err(()),
    };
    let text_keys = ["output", "text", "content"];
    let valid = strings_ok(
        record,
        &[
            "command",
            "output",
            "text",
            "content",
            "aggregated_output",
            "aggregatedOutput",
        ],
    ) && exit_codes_ok(record)
        && nested_ok(record, "metadata", &exit_codes_ok)
        && nested_ok(record, "structuredContent", &|nested| {
            strings_ok(nested, &text_keys)
        })
        && nested_ok(record, "structured_content", &|nested| {
            strings_ok(nested, &text_keys)
        })
        && nested_ok(record, "result", &|nested| {
            strings_ok(nested, &["command", "output", "text", "content"])
        });
    if !valid {
        return Err(());
    }
    let nested = |key: &str, field: &str| {
        record
            .get(key)
            .and_then(Value::as_object)
            .and_then(|nested| nested.get(field))
    };
    let raw_text = [
        record.get("output"),
        record.get("text"),
        record.get("content"),
        record.get("aggregated_output"),
        record.get("aggregatedOutput"),
        nested("structuredContent", "output"),
        nested("structuredContent", "text"),
        nested("structuredContent", "content"),
        nested("structured_content", "output"),
        nested("structured_content", "text"),
        nested("structured_content", "content"),
        nested("result", "output"),
        nested("result", "text"),
        nested("result", "content"),
    ]
    .into_iter()
    .find_map(non_empty_str);
    let exit_code = [
        record.get("exitCode"),
        record.get("exit_code"),
        nested("metadata", "exitCode"),
        nested("metadata", "exit_code"),
    ]
    .into_iter()
    .find(|value| !nullish(*value))
    .flatten()
    .cloned();
    Ok(Some(ShellOutput {
        command: non_empty_str(record.get("command"))
            .or_else(|| non_empty_str(nested("result", "command")))
            .map(str::to_owned),
        output: extract_codex_shell_output(raw_text),
        exit_code,
    }))
}

/// `deriveCodexToolDetail({ name, input, output, cwd })`.
///
/// # Errors
/// Returns the branch name when Paseo would use a detail branch that is not
/// ported (read, write, edit, search, speak).
pub fn derive_tool_detail(name: &str, input: &Value, output: &Value) -> Result<Value, String> {
    let unknown = || json!({"type": "unknown", "input": input, "output": output});
    if OTHER_DETAIL_BRANCHES.contains(&name) {
        return Err(format!("tool detail branch {name}"));
    }
    if !SHELL_NAMES.contains(&name) {
        return Ok(unknown());
    }
    let (Ok(input), Ok(output)) = (parse_shell_input(input), parse_shell_output(output)) else {
        return Ok(unknown());
    };
    let command = input
        .as_ref()
        .and_then(|input| input.command.clone())
        .or_else(|| output.as_ref().and_then(|output| output.command.clone()));
    let Some(command) = command else {
        return Ok(unknown());
    };
    let mut detail = Map::new();
    detail.insert("type".to_owned(), json!("shell"));
    detail.insert("command".to_owned(), json!(command));
    if let Some(cwd) = input.as_ref().and_then(|input| input.cwd.as_ref()) {
        detail.insert("cwd".to_owned(), json!(cwd));
    }
    if let Some(text) = output.as_ref().and_then(|output| output.output.as_ref()) {
        detail.insert("output".to_owned(), json!(text));
    }
    if let Some(exit_code) = output.as_ref().and_then(|output| output.exit_code.clone()) {
        detail.insert("exitCode".to_owned(), exit_code);
    }
    Ok(Value::Object(detail))
}

/// `resolveCodexToolKind(name)` for the kinds this port renders.
fn is_edit_tool(name: &str) -> bool {
    matches!(name, "edit" | "apply_patch" | "apply_diff")
}

/// `toToolCallFromNormalizedEnvelope` and `toToolCallTimelineItem`.
fn tool_call_item(
    call_id: &str,
    name: &str,
    input: &Value,
    output: &Value,
    status: &str,
    error: &Value,
) -> ToolMapping {
    let name = js_trim(name);
    if call_id.is_empty() || name.is_empty() {
        return ToolMapping::Skip;
    }
    if is_edit_tool(name) || name == "speak" {
        return ToolMapping::Unported(format!("tool kind {name}"));
    }
    let detail = match derive_tool_detail(name, input, output) {
        Ok(detail) => detail,
        Err(unported) => return ToolMapping::Unported(unported),
    };
    let error = if status == "failed" {
        if error.is_null() {
            json!({"message": "Tool call failed"})
        } else {
            error.clone()
        }
    } else {
        Value::Null
    };
    ToolMapping::Item(json!({
        "type": "tool_call",
        "callId": call_id,
        "name": name,
        "status": status,
        "error": error,
        "detail": detail,
    }))
}

/// `CodexCommandExecutionItemSchema` validation.
fn command_execution_item_ok(item: &Map<String, Value>) -> bool {
    non_empty_str(item.get("id")).is_some()
        && matches!(item.get("status"), None | Some(Value::String(_)))
        && match item.get("command") {
            None | Some(Value::String(_)) => true,
            Some(Value::Array(parts)) => parts.iter().all(Value::is_string),
            Some(_) => false,
        }
        && matches!(item.get("cwd"), None | Some(Value::String(_)))
        && matches!(
            item.get("aggregatedOutput"),
            None | Some(Value::Null | Value::String(_))
        )
        && matches!(
            item.get("exitCode"),
            None | Some(Value::Null | Value::Number(_))
        )
}

/// `mapCommandExecutionItem` through `toToolCallFromNormalizedEnvelope`.
fn command_execution_to_tool_call(item: &Map<String, Value>) -> ToolMapping {
    if !command_execution_item_ok(item) {
        return ToolMapping::Skip;
    }
    let command = normalize_command_execution_command(item.get("command"));
    let parsed_output =
        extract_codex_shell_output(item.get("aggregatedOutput").and_then(Value::as_str));
    let mut input = Map::new();
    if let Some(command) = &command {
        input.insert("command".to_owned(), json!(command));
    }
    if let Some(cwd) = item.get("cwd") {
        input.insert("cwd".to_owned(), cwd.clone());
    }
    let input = if input.is_empty() {
        Value::Null
    } else {
        Value::Object(input)
    };
    let exit_code = item.get("exitCode");
    let output = if parsed_output.is_some() || exit_code.is_some() {
        let mut output = Map::new();
        if let Some(command) = &command {
            output.insert("command".to_owned(), json!(command));
        }
        if let Some(text) = &parsed_output {
            output.insert("output".to_owned(), json!(text));
        }
        if let Some(exit_code) = exit_code {
            output.insert("exitCode".to_owned(), exit_code.clone());
        }
        Value::Object(output)
    } else {
        Value::Null
    };
    let error = item.get("error").cloned().unwrap_or(Value::Null);
    let status = normalize_tool_call_status(
        item.get("status").and_then(Value::as_str),
        Some(&error),
        Some(&output),
    );
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    tool_call_item(id, "shell", &input, &output, status, &error)
}

/// `mapCodexToolCallFromThreadItem(item, { cwd })` for a normalized item.
///
/// A `fileChange` item that passes Paseo's item schema maps to its
/// `apply_patch` envelope (see [`map_file_change_item`]), but its timeline
/// detail is Paseo's shared edit-detail branch, which lives in
/// `spocky_contracts::tool_detail` once it lands, so the item is unported
/// here. An item the schema rejects is skipped, as Paseo returns `null`.
#[must_use]
pub fn tool_call_from_thread_item(item: &Map<String, Value>, item_type: &str) -> ToolMapping {
    match item_type {
        "commandExecution" => command_execution_to_tool_call(item),
        "fileChange" => match map_file_change_item(item, None) {
            Ok(None) => ToolMapping::Skip,
            Ok(Some(_)) => ToolMapping::Unported("thread item fileChange".to_owned()),
            Err(DiffTruncationUnported) => ToolMapping::Unported(
                "thread item fileChange (diff over the truncate limit)".to_owned(),
            ),
        },
        other => ToolMapping::Unported(format!("thread item {other}")),
    }
}

/// `mapCodexToolCallEnvelope` for non-edit tools.
fn tool_call_envelope(
    call_id: Option<&str>,
    name: &str,
    input: &Value,
    output: &Value,
    error: &Value,
) -> ToolMapping {
    let name = js_trim(name);
    if name.is_empty() {
        return ToolMapping::Skip;
    }
    let call_id = call_id.map(js_trim).unwrap_or_default();
    if call_id.is_empty() {
        return ToolMapping::Skip;
    }
    if name == "apply_patch" || name == "apply_diff" {
        return ToolMapping::Unported(format!("tool envelope {name}"));
    }
    let status = normalize_tool_call_status(Some("completed"), Some(error), Some(output));
    tool_call_item(call_id, name, input, output, status, error)
}

/// Inputs of `mapCodexExecNotificationToToolCall`.
pub struct ExecNotification<'a> {
    pub call_id: Option<&'a str>,
    pub command: &'a Value,
    pub cwd: Option<&'a str>,
    pub output: Option<&'a str>,
    pub exit_code: Option<&'a Value>,
    pub success: Option<bool>,
    pub stderr: Option<&'a str>,
    pub running: bool,
}

/// `mapCodexExecNotificationToToolCall(params)`.
#[must_use]
pub fn exec_notification_to_tool_call(params: &ExecNotification<'_>) -> ToolMapping {
    let Some(command) = normalize_codex_command_value(params.command) else {
        return ToolMapping::Skip;
    };
    let exit_code = params.exit_code.filter(|code| !code.is_null());
    let is_failure = !params.running
        && (params.success == Some(false)
            || exit_code
                .and_then(Value::as_f64)
                .is_some_and(|code| code != 0.0));
    let output = if params.running {
        Value::Null
    } else {
        let mut output = Map::new();
        output.insert("command".to_owned(), command.clone());
        if let Some(text) = params.output {
            output.insert("output".to_owned(), json!(text));
        }
        if let Some(exit_code) = exit_code {
            output.insert("exitCode".to_owned(), exit_code.clone());
        }
        Value::Object(output)
    };
    let mut input = Map::new();
    input.insert("command".to_owned(), command);
    if let Some(cwd) = params.cwd.filter(|cwd| !cwd.is_empty()) {
        input.insert("cwd".to_owned(), json!(cwd));
    }
    let error = if is_failure {
        let stderr = params.stderr.map(js_trim).unwrap_or_default();
        let message = if stderr.is_empty() {
            "Command failed"
        } else {
            stderr
        };
        json!({"message": message})
    } else {
        Value::Null
    };
    let mapped = tool_call_envelope(
        params.call_id,
        "shell",
        &Value::Object(input),
        &output,
        &error,
    );
    match mapped {
        ToolMapping::Item(mut item) if params.running => {
            item["status"] = json!("running");
            item["error"] = Value::Null;
            ToolMapping::Item(item)
        }
        other => other,
    }
}

/// `decodeCodexOutputDeltaChunk(chunk)`: base64 chunks that round-trip are
/// decoded, anything else passes through.
#[must_use]
pub fn decode_output_delta_chunk(chunk: &str) -> String {
    let trimmed = js_trim(chunk);
    if trimmed.is_empty()
        || !trimmed.len().is_multiple_of(4)
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return chunk.to_owned();
    }
    let Some(bytes) = node_base64_decode(trimmed) else {
        return chunk.to_owned();
    };
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    if decoded.is_empty() {
        return chunk.to_owned();
    }
    let input = trimmed.trim_end_matches('=');
    let round_trip = base64_encode(decoded.as_bytes());
    if round_trip.trim_end_matches('=') == input {
        decoded
    } else {
        chunk.to_owned()
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Node `Buffer.from(text, "base64")` for text already limited to the
/// standard alphabet: decoding stops at the first `=`.
fn node_base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0_u32;
    let mut count = 0;
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        let value = u32::try_from(BASE64_ALPHABET.iter().position(|c| *c == byte)?).ok()?;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push(u8::try_from((bits >> count) & 0xff).ok()?);
        }
    }
    Some(bytes)
}

fn base64_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (triple >> (18 - 6 * index)) & 0x3f;
                output.push(char::from(BASE64_ALPHABET[sextet as usize]));
            } else {
                output.push('=');
            }
        }
    }
    output
}

// ---------------------------------------------------------------------------
// fileChange items: `mapFileChangeItem` and the apply_patch text helpers of
// `codex/tool-call-mapper.ts`, `normalizeCodexFilePath`
// (`codex/tool-call-detail-parser.ts`), and `stripCwdPrefix`
// (`@getpaseo/protocol/path-utils`).
// ---------------------------------------------------------------------------

/// `truncateDiffText`'s default limit, in UTF-16 code units.
const DIFF_TRUNCATE_CHARS: usize = 12_000;

/// A diff text longer than `truncateDiffText`'s 12,000 UTF-16 units. Paseo
/// would cut it and append a marker; that function is one of the shared
/// primitives that move to `spocky_contracts::tool_detail`, so a longer text
/// is reported as unported here instead of being cut by a second copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffTruncationUnported;

/// `truncateDiffText(text)` for a text within its limit.
fn within_diff_limit(text: &str) -> Result<String, DiffTruncationUnported> {
    if js_length(text) > DIFF_TRUNCATE_CHARS {
        Err(DiffTruncationUnported)
    } else {
        Ok(text.to_owned())
    }
}

/// `String.prototype.trimStart`.
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

/// `text.split(/\r?\n/)`.
fn split_js_lines(text: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = text.split('\n').collect();
    let last = parts.len().saturating_sub(1);
    for part in &mut parts[..last] {
        if let Some(stripped) = part.strip_suffix('\r') {
            *part = stripped;
        }
    }
    parts
}

/// `looksLikeUnifiedDiff`.
fn looks_like_unified_diff(text: &str) -> bool {
    let normalized = js_trim_start(text);
    !normalized.is_empty()
        && (normalized.starts_with("diff --git")
            || normalized.starts_with("@@")
            || normalized.starts_with("--- ")
            || normalized.starts_with("+++ "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchDirectiveKind {
    Add,
    Update,
    Delete,
}

/// `parseCodexApplyPatchDirective`.
fn parse_apply_patch_directive(line: &str) -> Option<(PatchDirectiveKind, String)> {
    let trimmed = js_trim(line);
    for (prefix, kind) in [
        ("*** Add File:", PatchDirectiveKind::Add),
        ("*** Update File:", PatchDirectiveKind::Update),
        ("*** Delete File:", PatchDirectiveKind::Delete),
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some((kind, js_trim(rest).to_owned()));
        }
    }
    None
}

/// `looksLikeCodexApplyPatch`.
fn looks_like_codex_apply_patch(text: &str) -> bool {
    let normalized = js_trim_start(text);
    if normalized.is_empty() {
        return false;
    }
    if normalized.starts_with("*** Begin Patch") {
        return true;
    }
    split_js_lines(text)
        .into_iter()
        .any(|line| parse_apply_patch_directive(line).is_some())
}

/// `normalizeDiffHeaderPath`: trim, then drop a run of quotes at each end.
fn normalize_diff_header_path(raw: &str) -> String {
    let is_quote = |character: char| character == '"' || character == '\'';
    js_trim(raw)
        .trim_start_matches(is_quote)
        .trim_end_matches(is_quote)
        .to_owned()
}

/// `codexApplyPatchToUnifiedDiff`.
fn codex_apply_patch_to_unified_diff(text: &str) -> String {
    let normalized_text = text.replace("\r\n", "\n");
    let mut output: Vec<String> = Vec::new();
    let mut saw_diff_content = false;
    for line in normalized_text.split('\n') {
        if let Some((kind, raw_path)) = parse_apply_patch_directive(line) {
            let path = normalize_diff_header_path(&raw_path);
            if !path.is_empty() {
                if output.last().is_some_and(|last| !last.is_empty()) {
                    output.push(String::new());
                }
                let left = if kind == PatchDirectiveKind::Add {
                    "/dev/null".to_owned()
                } else {
                    format!("a/{path}")
                };
                let right = if kind == PatchDirectiveKind::Delete {
                    "/dev/null".to_owned()
                } else {
                    format!("b/{path}")
                };
                output.push(format!("diff --git a/{path} b/{path}"));
                output.push(format!("--- {left}"));
                output.push(format!("+++ {right}"));
                saw_diff_content = true;
            }
            continue;
        }
        let trimmed = js_trim(line);
        if trimmed == "*** Begin Patch"
            || trimmed == "*** End Patch"
            || trimmed == "*** End of File"
            || trimmed.starts_with("*** Move to:")
        {
            continue;
        }
        if line.starts_with("@@")
            || line.starts_with('+')
            || line.starts_with('-')
            || line.starts_with(' ')
            || line.starts_with("\\ No newline at end of file")
        {
            output.push(line.to_owned());
            saw_diff_content = true;
        }
    }
    if !saw_diff_content {
        return text.to_owned();
    }
    let normalized = js_trim(&output.join("\n")).to_owned();
    if normalized.is_empty() {
        text.to_owned()
    } else {
        normalized
    }
}

/// `contentToDeletionDiff`, including its `lines.indexOf(l)` filter: an empty
/// line is kept only when the first empty line is not the last element.
fn content_to_deletion_diff(file_path: &str, content: &str) -> String {
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let first_empty = lines.iter().position(|line| line.is_empty());
    let kept: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !line.is_empty() || first_empty.is_some_and(|first| first + 1 < lines.len()))
        .collect();
    let mut output = vec![
        format!("diff --git a/{file_path} b/{file_path}"),
        format!("--- a/{file_path}"),
        "+++ /dev/null".to_owned(),
    ];
    if !kept.is_empty() {
        output.push(format!("@@ -1,{} +0,0 @@", kept.len()));
        output.extend(kept.iter().map(|line| format!("-{line}")));
    }
    output.join("\n")
}

/// `classifyDiffLikeText`: `(isDiff, text)`.
fn classify_diff_like_text(text: &str) -> (bool, String) {
    if looks_like_unified_diff(text) {
        return (true, text.to_owned());
    }
    if looks_like_codex_apply_patch(text) {
        return (true, codex_apply_patch_to_unified_diff(text));
    }
    (false, text.to_owned())
}

/// `asEditTextFields`: at most one of the two fields is set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct EditTextFields {
    unified_diff: Option<String>,
    new_string: Option<String>,
}

impl EditTextFields {
    fn is_empty(&self) -> bool {
        self.unified_diff.is_none() && self.new_string.is_none()
    }
}

fn as_edit_text_fields(text: Option<&str>) -> Result<EditTextFields, DiffTruncationUnported> {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Ok(EditTextFields::default());
    };
    let (is_diff, classified) = classify_diff_like_text(text);
    if is_diff {
        return Ok(EditTextFields {
            unified_diff: Some(within_diff_limit(&classified)?),
            new_string: None,
        });
    }
    Ok(EditTextFields {
        unified_diff: None,
        new_string: Some(text.to_owned()),
    })
}

/// `asEditFileOutputFields`: `(patch, content)`, at most one set.
fn as_edit_file_output_fields(
    text: Option<&str>,
) -> Result<(Option<String>, Option<String>), DiffTruncationUnported> {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Ok((None, None));
    };
    let (is_diff, classified) = classify_diff_like_text(text);
    if is_diff {
        return Ok((Some(within_diff_limit(&classified)?), None));
    }
    Ok((None, Some(text.to_owned())))
}

/// `CodexFileChangeEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileChangeEntry {
    path: String,
    kind: Option<String>,
    diff: Option<String>,
}

/// `stripCwdPrefix(filePath, cwd)`.
fn strip_cwd_prefix(file_path: &str, cwd: &str) -> String {
    if cwd.is_empty() || file_path.is_empty() {
        return file_path.to_owned();
    }
    let normalized_cwd = cwd.replace('\\', "/");
    let normalized_cwd = normalized_cwd.trim_end_matches('/');
    let normalized_path = file_path.replace('\\', "/");
    let prefix = format!("{normalized_cwd}/");
    if let Some(rest) = normalized_path.strip_prefix(&prefix) {
        return rest.to_owned();
    }
    if normalized_path == normalized_cwd {
        return ".".to_owned();
    }
    file_path.to_owned()
}

/// `normalizeCodexFilePath(filePath, cwd)`.
fn normalize_codex_file_path(file_path: &str, cwd: Option<&str>) -> Option<String> {
    if file_path.is_empty() {
        return None;
    }
    match cwd {
        Some(cwd) if !cwd.is_empty() => Some(strip_cwd_prefix(file_path, cwd)),
        _ => Some(file_path.to_owned()),
    }
}

/// A string field that is non-empty after trimming, trimmed.
fn trimmed_non_empty<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    match entry.get(key) {
        Some(Value::String(text)) if !js_trim(text).is_empty() => Some(js_trim(text)),
        _ => None,
    }
}

/// `parseFileChangePath`.
fn parse_file_change_path(
    entry: &Map<String, Value>,
    cwd: Option<&str>,
    fallback_path: Option<&str>,
) -> Option<String> {
    let raw = trimmed_non_empty(entry, "path")
        .or_else(|| trimmed_non_empty(entry, "file_path"))
        .or_else(|| trimmed_non_empty(entry, "filePath"))
        .or_else(|| fallback_path.map(js_trim).filter(|path| !path.is_empty()))?;
    normalize_codex_file_path(raw, cwd)
}

/// A non-empty string value of `key`.
fn non_empty_string_field<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    non_empty_str(entry.get(key))
}

/// `parseFileChangeKind`: a non-empty string `kind`, else a non-empty string
/// `type`. Codex 0.159.0 sends `kind` as an object, which gives `None`.
fn parse_file_change_kind(entry: &Map<String, Value>) -> Option<String> {
    non_empty_string_field(entry, "kind")
        .or_else(|| non_empty_string_field(entry, "type"))
        .map(str::to_owned)
}

/// `parseFileChangeDiff`: `pickFirstPatchLikeString`.
fn parse_file_change_diff(entry: &Map<String, Value>) -> Option<String> {
    [
        "diff",
        "patch",
        "unified_diff",
        "unifiedDiff",
        "content",
        "newString",
    ]
    .into_iter()
    .find_map(|key| non_empty_string_field(entry, key))
    .map(str::to_owned)
}

/// `toFileChangeEntry`.
fn to_file_change_entry(
    entry: &Map<String, Value>,
    cwd: Option<&str>,
    fallback_path: Option<&str>,
) -> Option<FileChangeEntry> {
    let path = parse_file_change_path(entry, cwd, fallback_path)?;
    Some(FileChangeEntry {
        path,
        kind: parse_file_change_kind(entry),
        diff: parse_file_change_diff(entry),
    })
}

/// `Object.entries(map)` order: array-index keys ascending, then the rest in
/// insertion order.
fn js_entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut indexed: Vec<(u32, &String, &Value)> = Vec::new();
    let mut named: Vec<(&String, &Value)> = Vec::new();
    for (key, value) in map {
        match array_index(key) {
            Some(index) => indexed.push((index, key, value)),
            None => named.push((key, value)),
        }
    }
    indexed.sort_by_key(|(index, _, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(named)
        .collect()
}

/// `parseFileChangeEntries(changes, options)`.
fn parse_file_change_entries(changes: &Value, cwd: Option<&str>) -> Vec<FileChangeEntry> {
    if !js_truthy(changes) {
        return Vec::new();
    }
    if let Value::Array(entries) = changes {
        return entries
            .iter()
            .filter_map(|entry| match entry {
                Value::Object(record) => to_file_change_entry(record, cwd, None),
                _ => None,
            })
            .collect();
    }
    let Value::Object(record) = changes else {
        return Vec::new();
    };
    if let Some(files @ Value::Array(_)) = record.get("files") {
        return parse_file_change_entries(files, cwd);
    }
    if let Some(single) = to_file_change_entry(record, cwd, None) {
        return vec![single];
    }
    js_entries(record)
        .into_iter()
        .filter_map(|(path, value)| match value {
            Value::Object(entry) => to_file_change_entry(entry, cwd, Some(path)),
            Value::String(diff) => {
                let path = normalize_codex_file_path(js_trim(path), cwd)?;
                Some(FileChangeEntry {
                    path,
                    kind: None,
                    diff: Some(diff.clone()),
                })
            }
            _ => None,
        })
        .collect()
}

/// `resolveFileChangeTextFields`.
fn resolve_file_change_text_fields(
    file: Option<&FileChangeEntry>,
) -> Result<EditTextFields, DiffTruncationUnported> {
    let Some(file) = file else {
        return Ok(EditTextFields::default());
    };
    if file.kind.as_deref() == Some("delete") {
        let unified = match file.diff.as_deref().filter(|diff| !diff.is_empty()) {
            Some(diff) => {
                let (is_diff, classified) = classify_diff_like_text(diff);
                if is_diff {
                    within_diff_limit(&classified)?
                } else {
                    within_diff_limit(&content_to_deletion_diff(&file.path, diff))?
                }
            }
            None => content_to_deletion_diff(&file.path, ""),
        };
        return Ok(EditTextFields {
            unified_diff: Some(unified),
            new_string: None,
        });
    }
    as_edit_text_fields(file.diff.as_deref())
}

/// `CodexNormalizedToolCallEnvelope` as `mapFileChangeItem` builds it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileChangeEnvelope {
    pub call_id: String,
    pub name: &'static str,
    pub input: Value,
    pub output: Value,
    pub status: &'static str,
    pub error: Value,
    pub cwd: Value,
}

impl FileChangeEnvelope {
    /// The envelope as Paseo's object literal orders it.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "callId": self.call_id,
            "name": self.name,
            "input": self.input,
            "output": self.output,
            "status": self.status,
            "error": self.error,
            "cwd": self.cwd,
        })
    }
}

/// `CodexFileChangeItemSchema`: a non-empty string `id` and an optional
/// string `status`; `error` and `changes` are anything.
fn file_change_item_ok(item: &Map<String, Value>) -> bool {
    non_empty_str(item.get("id")).is_some()
        && matches!(item.get("status"), None | Some(Value::String(_)))
}

/// `toNullableObject`.
fn nullable_object(map: Map<String, Value>) -> Value {
    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map)
    }
}

/// `mapFileChangeItem(item, { cwd })` after `CodexThreadItemSchema` accepted
/// `item`. `Ok(None)` is the schema rejecting the item (Paseo then returns
/// `null`); `Err` is a diff text over the `truncateDiffText` limit.
///
/// # Errors
/// Returns [`DiffTruncationUnported`] when a diff text is longer than 12,000
/// UTF-16 units.
pub fn map_file_change_item(
    item: &Map<String, Value>,
    cwd: Option<&str>,
) -> Result<Option<FileChangeEnvelope>, DiffTruncationUnported> {
    if !file_change_item_ok(item) {
        return Ok(None);
    }
    let files = item
        .get("changes")
        .map(|changes| parse_file_change_entries(changes, cwd))
        .unwrap_or_default();
    let file_ref = |file: &FileChangeEntry, extra: Option<(&str, String)>| {
        let mut entry = Map::new();
        entry.insert("path".to_owned(), json!(file.path));
        if let Some(kind) = &file.kind {
            entry.insert("kind".to_owned(), json!(kind));
        }
        if let Some((key, value)) = extra {
            entry.insert(key.to_owned(), json!(value));
        }
        Value::Object(entry)
    };

    let mut input = Map::new();
    if !files.is_empty() {
        let refs: Vec<Value> = files.iter().map(|file| file_ref(file, None)).collect();
        input.insert("files".to_owned(), Value::Array(refs));
    }

    let mut output = Map::new();
    if !files.is_empty() {
        let mut entries = Vec::new();
        for file in &files {
            let extra = if file.kind.as_deref() == Some("delete") {
                resolve_file_change_text_fields(Some(file))?
                    .unified_diff
                    .map(|patch| ("patch", patch))
            } else {
                let (patch, content) = as_edit_file_output_fields(file.diff.as_deref())?;
                patch
                    .map(|patch| ("patch", patch))
                    .or_else(|| content.map(|content| ("content", content)))
            };
            entries.push(file_ref(file, extra));
        }
        output.insert("files".to_owned(), Value::Array(entries));
    }
    let output = nullable_object(output);

    let error = match item.get("error") {
        None | Some(Value::Null) => Value::Null,
        Some(error) => error.clone(),
    };
    let status = normalize_tool_call_status(
        item.get("status").and_then(Value::as_str),
        Some(&error),
        Some(&output),
    );
    let first_file = files.first();
    let first_text = resolve_file_change_text_fields(first_file)?;
    if !first_text.is_empty() {
        if let Some(first) = first_file {
            input.insert("path".to_owned(), json!(first.path));
        }
        if let Some(patch) = &first_text.unified_diff {
            input.insert("patch".to_owned(), json!(patch));
        }
        if let Some(content) = &first_text.new_string {
            input.insert("content".to_owned(), json!(content));
        }
    }
    Ok(Some(FileChangeEnvelope {
        call_id: item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: "apply_patch",
        input: nullable_object(input),
        output,
        status,
        error,
        cwd: cwd.map_or(Value::Null, |cwd| json!(cwd)),
    }))
}

// ---------------------------------------------------------------------------
// Legacy `codex/event/patch_apply_*` notifications:
// `mapCodexPatchNotificationToToolCall`, `parseCodexPatchChanges`, and
// `codexPatchTextFields` (`codex-app-server-agent.ts`), and the edit-input
// normalization `mapCodexToolCallEnvelope` applies to `apply_patch`
// (`normalizeToolCallEditInput`, `codex/tool-call-mapper.ts`).
// ---------------------------------------------------------------------------

/// `extractPatchPrimaryFilePath`: the first directive path that is not empty.
fn extract_patch_primary_file_path(patch: &str) -> Option<String> {
    split_js_lines(patch)
        .into_iter()
        .find_map(|line| parse_apply_patch_directive(line).filter(|(_, path)| !path.is_empty()))
        .map(|(_, path)| path)
}

/// `findToolCallEditPatchText`: the first non-empty text field.
fn find_tool_call_edit_patch_text(input: &Map<String, Value>) -> Option<&str> {
    ["patch", "diff", "unified_diff", "unifiedDiff", "content"]
        .into_iter()
        .find_map(|key| non_empty_string_field(input, key))
}

/// `findToolCallEditInputPath`: a `path`, `file_path` or `filePath` that is
/// not blank (returned untrimmed), else the patch's first directive path.
fn find_tool_call_edit_input_path(input: &Map<String, Value>, patch_text: &str) -> Option<String> {
    for key in ["path", "file_path", "filePath"] {
        if let Some(Value::String(text)) = input.get(key)
            && !js_trim(text).is_empty()
        {
            return Some(text.clone());
        }
    }
    extract_patch_primary_file_path(patch_text)
}

/// `normalizeToolCallEditRecordInput`.
///
/// # Errors
/// Returns [`DiffTruncationUnported`] when the patch text is a diff over the
/// `truncateDiffText` limit.
fn normalize_tool_call_edit_record_input(
    input: &Map<String, Value>,
) -> Result<Value, DiffTruncationUnported> {
    let Some(candidate) = find_tool_call_edit_patch_text(input) else {
        return Ok(Value::Object(input.clone()));
    };
    let text_fields = as_edit_text_fields(Some(candidate))?;
    let raw_path = find_tool_call_edit_input_path(input, candidate);
    let mut normalized = Map::new();
    for (key, value) in input {
        if !matches!(
            key.as_str(),
            "patch" | "diff" | "unified_diff" | "unifiedDiff"
        ) {
            normalized.insert(key.clone(), value.clone());
        }
    }
    if let Some(path) = raw_path {
        normalized.insert("path".to_owned(), json!(path));
    }
    if let Some(unified) = &text_fields.unified_diff {
        normalized.insert("patch".to_owned(), json!(unified));
    }
    if let Some(new_string) = &text_fields.new_string {
        normalized.insert("content".to_owned(), json!(new_string));
    }
    if text_fields.unified_diff.is_some() && normalized.contains_key("content") {
        normalized = normalized
            .into_iter()
            .filter(|(key, _)| key != "content")
            .collect();
    }
    Ok(Value::Object(normalized))
}

/// `CodexPatchFileChange`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchFile {
    path: String,
    kind: Option<String>,
    content: Option<String>,
}

/// A record entry that names its file: `path`, `file_path` or `filePath`,
/// trimmed and not blank.
fn patch_file_from_record(record: &Map<String, Value>) -> Option<PatchFile> {
    let path = trimmed_non_empty(record, "path")
        .or_else(|| trimmed_non_empty(record, "file_path"))
        .or_else(|| trimmed_non_empty(record, "filePath"))?;
    Some(PatchFile {
        path: path.to_owned(),
        kind: parse_file_change_kind(record),
        content: parse_file_change_diff(record),
    })
}

/// `parseCodexPatchChanges(changes)`.
fn parse_codex_patch_changes(changes: &Value) -> Vec<PatchFile> {
    match changes {
        Value::Array(entries) => entries
            .iter()
            .filter_map(|entry| match entry {
                Value::Object(record) => patch_file_from_record(record),
                _ => None,
            })
            .collect(),
        Value::Object(record) => {
            if let Some(file) = patch_file_from_record(record) {
                return vec![file];
            }
            js_entries(record)
                .into_iter()
                .filter_map(|(key, value)| {
                    let path = js_trim(key);
                    if path.is_empty() {
                        return None;
                    }
                    // Here `kind` is the value's `type` as given, even empty.
                    let kind = match value {
                        Value::Object(entry) => {
                            entry.get("type").and_then(Value::as_str).map(str::to_owned)
                        }
                        _ => None,
                    };
                    let content = match value {
                        Value::Object(entry) => parse_file_change_diff(entry),
                        _ => None,
                    };
                    Some(PatchFile {
                        path: path.to_owned(),
                        kind,
                        content,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// `codexPatchTextFields(text)`: `patch` when it looks like a unified diff,
/// else `content`; nothing for a missing text.
fn insert_patch_text_fields(map: &mut Map<String, Value>, text: Option<&str>) {
    let Some(text) = text else {
        return;
    };
    let key = if looks_like_unified_diff(text) {
        "patch"
    } else {
        "content"
    };
    map.insert(key.to_owned(), json!(text));
}

/// Arguments of `mapCodexPatchNotificationToToolCall`.
pub struct PatchNotification<'a> {
    pub call_id: Option<&'a str>,
    pub changes: &'a Value,
    pub cwd: Option<&'a str>,
    pub stdout: Option<&'a str>,
    pub stderr: Option<&'a str>,
    pub success: Option<bool>,
    pub running: bool,
}

/// The `CodexNormalizedToolCallEnvelope` that `mapCodexToolCallEnvelope`
/// hands `toToolCallFromNormalizedEnvelope`, before the shared edit-detail
/// branch derives the timeline detail.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchEnvelope {
    pub call_id: String,
    pub name: &'static str,
    pub input: Value,
    pub output: Value,
    pub error: Value,
    pub status: &'static str,
    pub cwd: Value,
    /// A started notification: the timeline item is then forced to
    /// `running` with no error (`toRunningToolCall`).
    pub running: bool,
}

impl PatchEnvelope {
    /// The envelope as `mapCodexToolCallEnvelope`'s object literal orders it.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "callId": self.call_id,
            "name": self.name,
            "input": self.input,
            "output": self.output,
            "error": self.error,
            "status": self.status,
            "cwd": self.cwd,
        })
    }

    /// The status the timeline item carries after `toRunningToolCall`.
    #[must_use]
    pub fn timeline_status(&self) -> &'static str {
        if self.running { "running" } else { self.status }
    }

    /// The error the timeline item carries after `toRunningToolCall`.
    #[must_use]
    pub fn timeline_error(&self) -> Value {
        if self.running {
            Value::Null
        } else {
            self.error.clone()
        }
    }
}

/// `mapCodexPatchNotificationToToolCall` up to the edit-detail branch.
/// `Ok(None)` is Paseo returning `null` (no usable call id).
///
/// # Errors
/// Returns [`DiffTruncationUnported`] when a patch text is a diff over the
/// `truncateDiffText` limit.
pub fn patch_notification_envelope(
    params: &PatchNotification<'_>,
) -> Result<Option<PatchEnvelope>, DiffTruncationUnported> {
    let files = parse_codex_patch_changes(params.changes);
    let patch_text: Option<&str> = files
        .iter()
        .filter_map(|file| file.content.as_deref().map(js_trim))
        .find(|text| !text.is_empty());

    let mut input = Map::new();
    if let Some(first) = files.first() {
        input.insert("path".to_owned(), json!(first.path));
        insert_patch_text_fields(&mut input, patch_text);
        let refs: Vec<Value> = files
            .iter()
            .map(|file| {
                let mut entry = Map::new();
                entry.insert("path".to_owned(), json!(file.path));
                if let Some(kind) = &file.kind {
                    entry.insert("kind".to_owned(), json!(kind));
                }
                Value::Object(entry)
            })
            .collect();
        input.insert("files".to_owned(), Value::Array(refs));
    } else {
        input.insert("changes".to_owned(), params.changes.clone());
        insert_patch_text_fields(&mut input, patch_text);
    }

    let output = if params.running {
        Value::Null
    } else {
        let mut output = Map::new();
        if !files.is_empty() {
            let entries: Vec<Value> = files
                .iter()
                .map(|file| {
                    let mut entry = Map::new();
                    entry.insert("path".to_owned(), json!(file.path));
                    if let Some(kind) = file.kind.as_ref().filter(|kind| !kind.is_empty()) {
                        entry.insert("kind".to_owned(), json!(kind));
                    }
                    insert_patch_text_fields(&mut entry, file.content.as_deref().or(patch_text));
                    Value::Object(entry)
                })
                .collect();
            output.insert("files".to_owned(), Value::Array(entries));
        }
        if let Some(stdout) = params.stdout.filter(|text| !text.is_empty()) {
            output.insert("stdout".to_owned(), json!(stdout));
        }
        if let Some(stderr) = params.stderr.filter(|text| !text.is_empty()) {
            output.insert("stderr".to_owned(), json!(stderr));
        }
        if let Some(success) = params.success {
            output.insert("success".to_owned(), json!(success));
        }
        Value::Object(output)
    };

    let error = if params.running || params.success != Some(false) {
        Value::Null
    } else {
        let message = params
            .stderr
            .map(js_trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("Patch apply failed");
        json!({"message": message})
    };

    // `mapCodexToolCallEnvelope`.
    let call_id = params.call_id.map(js_trim).unwrap_or_default();
    if call_id.is_empty() {
        return Ok(None);
    }
    let input = normalize_tool_call_edit_record_input(&input)?;
    let status = normalize_tool_call_status(Some("completed"), Some(&error), Some(&output));
    Ok(Some(PatchEnvelope {
        call_id: call_id.to_owned(),
        name: "apply_patch",
        input,
        output,
        error,
        status,
        cwd: params.cwd.map_or(Value::Null, |cwd| json!(cwd)),
        running: params.running,
    }))
}

/// A legacy patch notification as a timeline mapping: nothing for a missing
/// call id, otherwise unported (the shared edit-detail branch is not here).
#[must_use]
pub fn map_patch_notification(params: &PatchNotification<'_>) -> ToolMapping {
    match patch_notification_envelope(params) {
        Ok(None) => ToolMapping::Skip,
        Ok(Some(_)) => ToolMapping::Unported("tool detail branch apply_patch".to_owned()),
        Err(DiffTruncationUnported) => ToolMapping::Unported(
            "tool detail branch apply_patch (diff over the truncate limit)".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(value: &Value) -> Value {
        let Value::Object(record) = value else {
            panic!("object");
        };
        match tool_call_from_thread_item(record, "commandExecution") {
            ToolMapping::Item(item) => item,
            other => panic!("expected item, got {other:?}"),
        }
    }

    #[test]
    fn unwraps_unix_shell_wrappers() {
        assert_eq!(unwrap_shell_command("/bin/zsh -lc 'echo hi'"), "echo hi");
        assert_eq!(unwrap_shell_command("bash -c \"ls -la\""), "ls -la");
        assert_eq!(unwrap_shell_command("/usr/local/bin/sh -c pwd"), "pwd");
        assert_eq!(unwrap_shell_command("  git status  "), "git status");
        assert_eq!(unwrap_shell_command("/bin//zsh -lc x"), "/bin//zsh -lc x");
        assert_eq!(unwrap_shell_command("fish -c x"), "fish -c x");
    }

    #[test]
    fn unwraps_windows_shell_wrappers() {
        assert_eq!(
            unwrap_shell_command("powershell -NoProfile -Command Get-Date"),
            "Get-Date"
        );
        assert_eq!(unwrap_shell_command("cmd.exe /c \"dir\""), "dir");
        assert_eq!(
            unwrap_shell_command("\"C:\\Program Files\\PowerShell\\pwsh.exe\" -c ls"),
            "ls"
        );
    }

    #[test]
    fn command_arrays_unwrap_shell_flags() {
        assert_eq!(
            normalize_command_execution_command(Some(&json!(["/bin/zsh", "-lc", "echo hi"]))),
            Some("echo hi".to_owned())
        );
        assert_eq!(
            normalize_command_execution_command(Some(&json!([
                "pwsh.exe", "-Command", "Get-Date", "-x"
            ]))),
            Some("Get-Date -x".to_owned())
        );
        assert_eq!(
            normalize_command_execution_command(Some(&json!(["git", " ", "status"]))),
            Some("git status".to_owned())
        );
    }

    // Paseo: "shows a successful shell command that produces no output".
    #[test]
    fn silent_completed_command_maps_to_shell_detail_with_exit_code() {
        assert_eq!(
            serde_json::to_string(&item(&json!({
                "type": "commandExecution", "id": "silent-merge", "status": "completed",
                "command": "gh pr merge 2030 --squash", "cwd": "/workspace/project",
                "aggregatedOutput": null, "exitCode": 0
            })))
            .unwrap(),
            r#"{"type":"tool_call","callId":"silent-merge","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"gh pr merge 2030 --squash","cwd":"/workspace/project","exitCode":0}}"#
        );
    }

    #[test]
    fn real_codex_0_159_command_items() {
        let started = item(&json!({
            "type": "commandExecution", "id": "call_1", "pluginId": null, "scriptPath": null,
            "command": "/bin/zsh -lc 'echo hi'", "cwd": "/p", "processId": null, "source": "agent",
            "status": "inProgress", "commandActions": [{"type": "unknown", "command": "echo hi"}],
            "aggregatedOutput": null, "exitCode": null, "durationMs": null
        }));
        assert_eq!(
            serde_json::to_string(&started).unwrap(),
            r#"{"type":"tool_call","callId":"call_1","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"echo hi","cwd":"/p"}}"#
        );
        let completed = item(&json!({
            "type": "commandExecution", "id": "call_1", "command": "/bin/zsh -lc 'echo hi'",
            "cwd": "/p", "status": "completed", "aggregatedOutput": "hi\n", "exitCode": 0
        }));
        assert_eq!(
            serde_json::to_string(&completed).unwrap(),
            r#"{"type":"tool_call","callId":"call_1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"echo hi","cwd":"/p","output":"hi\n","exitCode":0}}"#
        );
    }

    #[test]
    fn failed_and_declined_command_statuses() {
        let failed = item(&json!({
            "type": "commandExecution", "id": "c", "command": "false", "status": "failed",
            "aggregatedOutput": null, "exitCode": 1
        }));
        assert_eq!(failed["status"], json!("failed"));
        assert_eq!(failed["error"], json!({"message": "Tool call failed"}));
        let declined = item(&json!({
            "type": "commandExecution", "id": "c", "command": "rm x", "status": "declined"
        }));
        assert_eq!(declined["status"], json!("running"));
        assert_eq!(
            declined["detail"],
            json!({"type": "shell", "command": "rm x"})
        );
    }

    #[test]
    fn invalid_command_items_are_skipped() {
        let Value::Object(record) = json!({"type": "commandExecution", "id": "", "command": "x"})
        else {
            unreachable!()
        };
        assert_eq!(
            tool_call_from_thread_item(&record, "commandExecution"),
            ToolMapping::Skip
        );
    }

    #[test]
    fn shell_envelope_output_is_stripped() {
        assert_eq!(
            extract_codex_shell_output(Some(
                "Chunk ID: 1\nWall time: 0.1s\nProcess exited with code 0\nOriginal token count: 2\nOutput:\nhi\n"
            )),
            Some("hi\n".to_owned())
        );
        assert_eq!(
            extract_codex_shell_output(Some(
                "Chunk ID: 1\nWall time: 0.1s\nProcess exited with code 0\nhello"
            )),
            Some("hello".to_owned())
        );
        assert_eq!(extract_codex_shell_output(Some("")), None);
    }

    #[test]
    fn exec_approval_preview_is_a_running_shell_call() {
        let mapped = exec_notification_to_tool_call(&ExecNotification {
            call_id: Some("exec-approval-1"),
            command: &json!("git restore README.md"),
            cwd: Some("/workspace/project"),
            output: None,
            exit_code: None,
            success: None,
            stderr: None,
            running: true,
        });
        assert_eq!(
            mapped,
            ToolMapping::Item(json!({
                "type": "tool_call", "callId": "exec-approval-1", "name": "shell",
                "status": "running", "error": null,
                "detail": {"type": "shell", "command": "git restore README.md", "cwd": "/workspace/project"}
            }))
        );
    }

    #[test]
    fn exec_completion_with_nonzero_exit_fails_with_stderr() {
        let ToolMapping::Item(mapped) = exec_notification_to_tool_call(&ExecNotification {
            call_id: Some("c"),
            command: &json!(["/bin/bash", "-lc", "false"]),
            cwd: None,
            output: Some("out"),
            exit_code: Some(&json!(2)),
            success: None,
            stderr: Some("  bad  "),
            running: false,
        }) else {
            panic!("item");
        };
        assert_eq!(mapped["status"], json!("failed"));
        assert_eq!(mapped["error"], json!({"message": "bad"}));
        assert_eq!(
            mapped["detail"],
            json!({"type": "shell", "command": "false", "output": "out", "exitCode": 2})
        );
    }

    #[test]
    fn status_vocabulary() {
        assert_eq!(
            normalize_tool_call_status(Some(" Done "), None, None),
            "completed"
        );
        assert_eq!(
            normalize_tool_call_status(Some("cancelled"), None, None),
            "canceled"
        );
        assert_eq!(
            normalize_tool_call_status(Some("weird"), None, None),
            "running"
        );
        assert_eq!(
            normalize_tool_call_status(None, None, Some(&json!({}))),
            "completed"
        );
        assert_eq!(
            normalize_tool_call_status(Some("completed"), Some(&json!("e")), None),
            "failed"
        );
    }

    #[test]
    fn output_delta_base64_round_trip() {
        assert_eq!(decode_output_delta_chunk("aGkK"), "hi\n");
        assert_eq!(decode_output_delta_chunk("plain text"), "plain text");
        assert_eq!(decode_output_delta_chunk("abc"), "abc");
    }

    #[test]
    fn unported_tool_types_are_reported() {
        let Value::Object(record) = json!({"type": "fileChange", "id": "f"}) else {
            unreachable!()
        };
        assert_eq!(
            tool_call_from_thread_item(&record, "fileChange"),
            ToolMapping::Unported("thread item fileChange".to_owned())
        );
    }

    fn record(value: &Value) -> &Map<String, Value> {
        value.as_object().expect("object")
    }

    #[test]
    fn strip_cwd_prefix_follows_path_utils() {
        assert_eq!(strip_cwd_prefix("/w/p/a.txt", "/w/p"), "a.txt");
        assert_eq!(strip_cwd_prefix("/w/p/a.txt", "/w/p///"), "a.txt");
        assert_eq!(strip_cwd_prefix("C:\\w\\p\\a.txt", "C:/w/p"), "a.txt");
        assert_eq!(strip_cwd_prefix("/w/p", "/w/p/"), ".");
        assert_eq!(strip_cwd_prefix("/other/a.txt", "/w/p"), "/other/a.txt");
        assert_eq!(strip_cwd_prefix("/w/p/a.txt", ""), "/w/p/a.txt");
    }

    #[test]
    fn apply_patch_text_converts_to_a_unified_diff() {
        let patch = "*** Begin Patch\n*** Add File: 'a.txt'\n+hi\n*** Update File: b.txt\n@@\n-x\n+y\n*** End Patch\n";
        assert_eq!(
            codex_apply_patch_to_unified_diff(patch),
            "diff --git a/a.txt b/a.txt\n--- /dev/null\n+++ b/a.txt\n+hi\n\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@\n-x\n+y"
        );
        assert_eq!(codex_apply_patch_to_unified_diff("plain"), "plain");
    }

    #[test]
    fn deletion_diff_keeps_pasteds_empty_line_quirk() {
        // `lines.indexOf("")` is the first empty line, so empty lines stay
        // unless that first one is the last element.
        assert_eq!(
            content_to_deletion_diff("f", "a\n\nb\n"),
            "diff --git a/f b/f\n--- a/f\n+++ /dev/null\n@@ -1,4 +0,0 @@\n-a\n-\n-b\n-"
        );
        assert_eq!(
            content_to_deletion_diff("f", ""),
            "diff --git a/f b/f\n--- a/f\n+++ /dev/null"
        );
        assert_eq!(
            content_to_deletion_diff("f", "a"),
            "diff --git a/f b/f\n--- a/f\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-a"
        );
    }

    #[test]
    fn recorded_codex_patch_item_maps_to_its_envelope() {
        // The shape real codex 0.159.0 sends: `kind` is an object, so no kind.
        let item = json!({
            "type": "fileChange", "id": "call_patch", "status": "inProgress",
            "changes": [{"path": "/w/outside.txt", "kind": {"type": "add"}, "diff": "hi\n"}]
        });
        let envelope = map_file_change_item(record(&item), Some("/w/project"))
            .expect("within limit")
            .expect("valid item");
        assert_eq!(
            serde_json::to_string(&envelope.to_json()).unwrap(),
            r#"{"callId":"call_patch","name":"apply_patch","input":{"files":[{"path":"/w/outside.txt"}],"path":"/w/outside.txt","content":"hi\n"},"output":{"files":[{"path":"/w/outside.txt","content":"hi\n"}]},"status":"running","error":null,"cwd":"/w/project"}"#
        );
    }

    #[test]
    fn file_change_entries_cover_every_changes_shape() {
        let entries = |changes: Value| parse_file_change_entries(&changes, Some("/w"));
        assert_eq!(entries(json!(null)), []);
        assert_eq!(entries(json!("text")), []);
        let paths = |entries: Vec<FileChangeEntry>| -> Vec<String> {
            entries.into_iter().map(|entry| entry.path).collect()
        };
        assert_eq!(
            paths(entries(
                json!([{"file_path": "/w/a"}, {"filePath": " /w/b "}, 3])
            )),
            ["a", "b"]
        );
        assert_eq!(paths(entries(json!({"files": [{"path": "/w/a"}]}))), ["a"]);
        assert_eq!(paths(entries(json!({"path": "/w/a", "diff": "x"}))), ["a"]);
        // Keyed by path; array-index keys come first, as `Object.entries`.
        assert_eq!(
            paths(entries(
                json!({"/w/z": "zz", "10": "t", "2": {"diff": "d"}, "/w/y": {"diff": "y"}, "k": 1})
            )),
            ["2", "10", "z", "y"]
        );
    }

    #[test]
    fn invalid_file_change_items_are_skipped_and_long_diffs_are_unported() {
        for item in [
            json!({"type": "fileChange", "id": "", "changes": []}),
            json!({"type": "fileChange", "changes": []}),
            json!({"type": "fileChange", "id": "x", "status": null}),
            json!({"type": "fileChange", "id": "x", "status": 3}),
        ] {
            assert_eq!(
                map_file_change_item(record(&item), None),
                Ok(None),
                "{item}"
            );
            assert_eq!(
                tool_call_from_thread_item(record(&item), "fileChange"),
                ToolMapping::Skip
            );
        }
        let long = json!({
            "type": "fileChange", "id": "x",
            "changes": [{"path": "a", "diff": "+".repeat(12_001), "kind": "update"}]
        });
        // `+++ ` makes this look like a diff, so truncation applies.
        let long_diff = json!({
            "type": "fileChange", "id": "x",
            "changes": [{"path": "a", "diff": format!("@@\n{}", "+".repeat(12_001))}]
        });
        assert!(map_file_change_item(record(&long), None).is_ok());
        assert_eq!(
            map_file_change_item(record(&long_diff), None),
            Err(DiffTruncationUnported)
        );
    }

    #[test]
    fn legacy_patch_begin_maps_to_a_running_envelope() {
        let changes =
            json!([{"path": "src/a.ts", "kind": "update", "diff": "@@ -1 +1 @@\n-a\n+b\n"}]);
        let envelope = patch_notification_envelope(&PatchNotification {
            call_id: Some(" call_9 "),
            changes: &changes,
            cwd: Some("/w"),
            stdout: None,
            stderr: None,
            success: None,
            running: true,
        })
        .expect("within limit")
        .expect("call id");
        assert_eq!(
            serde_json::to_string(&envelope.to_json()).unwrap(),
            r#"{"callId":"call_9","name":"apply_patch","input":{"path":"src/a.ts","files":[{"path":"src/a.ts","kind":"update"}],"patch":"@@ -1 +1 @@\n-a\n+b"},"output":null,"error":null,"status":"completed","cwd":"/w"}"#
        );
        assert_eq!(envelope.timeline_status(), "running");
        assert_eq!(envelope.timeline_error(), Value::Null);
    }

    #[test]
    fn legacy_patch_end_failure_carries_the_trimmed_stderr() {
        let changes = json!({"src/a.ts": {"type": "update", "content": "new text"}});
        let envelope = patch_notification_envelope(&PatchNotification {
            call_id: Some("c"),
            changes: &changes,
            cwd: None,
            stdout: Some("out"),
            stderr: Some("  bad hunk\n"),
            success: Some(false),
            running: false,
        })
        .unwrap()
        .unwrap();
        assert_eq!(envelope.status, "failed");
        assert_eq!(envelope.error, json!({"message": "bad hunk"}));
        assert_eq!(
            envelope.output,
            json!({
                "files": [{"path": "src/a.ts", "kind": "update", "content": "new text"}],
                "stdout": "out", "stderr": "  bad hunk\n", "success": false
            })
        );
    }

    #[test]
    fn legacy_patch_without_a_call_id_is_dropped() {
        let changes = json!([]);
        let params = |call_id| PatchNotification {
            call_id,
            changes: &changes,
            cwd: None,
            stdout: None,
            stderr: None,
            success: None,
            running: true,
        };
        assert_eq!(patch_notification_envelope(&params(None)), Ok(None));
        assert_eq!(patch_notification_envelope(&params(Some("  "))), Ok(None));
        assert_eq!(map_patch_notification(&params(None)), ToolMapping::Skip);
    }
}
