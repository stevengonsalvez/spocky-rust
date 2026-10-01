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
use spocky_contracts::text::{is_js_whitespace, js_trim};

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
#[must_use]
pub fn tool_call_from_thread_item(item: &Map<String, Value>, item_type: &str) -> ToolMapping {
    match item_type {
        "commandExecution" => command_execution_to_tool_call(item),
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
}
