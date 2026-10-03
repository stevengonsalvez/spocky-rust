//! `forkSession(sessionId, { upToMessageId })` of `@anthropic-ai/claude-agent-sdk`
//! 0.3.246, the default `ClaudeRewindSdk` (`rewind.ts`): copies a session's
//! transcript up to a message into a new session file under the same Claude
//! project directory.
//!
//! The transcript is read and written as the SDK does: the project
//! directories are scanned in `readdir` order for `<sessionId>.jsonl`, the
//! file is split into lines of UTF-8 (lossy), and the fork gets fresh uuids,
//! re-linked parents, and a `custom-title` entry.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_contracts::text::{is_js_whitespace, js_length, js_trim};
use spocky_session::agent_sdk::AgentError;
use unicode_normalization::UnicodeNormalization;

use crate::local::LocalBoxFuture;
use crate::project_dir::{home_dir, join_path};
use crate::session::RewindSdk;

/// `Cr`: the head and tail the title is read from.
const HEAD_TAIL_BYTES: usize = 65536;
/// `E4`: the longest custom title, in code points.
const TITLE_CODE_POINTS: usize = 200;

/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` (`i`).
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `PW(value)`: unescapes a JSON string body when it holds a backslash.
fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    if let Ok(JsValue::String(ref text)) = parse(&format!("\"{value}\"")) {
        return text.clone();
    }
    value.to_owned()
}

/// `un(text, key)`: the string value of the last `"key":"…"` in `text`.
fn last_string_field(text: &str, key: &str) -> Option<String> {
    let patterns = [format!("\"{key}\":\""), format!("\"{key}\": \"")];
    let bytes = text.as_bytes();
    let mut found: Option<String> = None;
    let mut found_at: Option<usize> = None;
    for pattern in &patterns {
        let mut from = 0;
        while let Some(offset) = text
            .get(from..)
            .and_then(|rest| rest.find(pattern.as_str()))
        {
            let start = from + offset;
            let value_start = start + pattern.len();
            let mut cursor = value_start;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == b'"' {
                    if found_at.is_none_or(|at| start > at) {
                        found = Some(unescape(&text[value_start..cursor]));
                        found_at = Some(start);
                    }
                    break;
                }
                cursor += 1;
            }
            from = cursor + 1;
        }
    }
    found
}

/// `cee`: `<command-name>(.*?)</command-name>`.
fn command_name(text: &str) -> Option<&str> {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let mut from = 0;
    while let Some(offset) = text[from..].find(OPEN) {
        let start = from + offset + OPEN.len();
        if let Some(end) = text[start..].find(CLOSE) {
            let captured = &text[start..start + end];
            if !captured.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
                return Some(captured);
            }
        }
        from += offset + 1;
    }
    None
}

/// `<bash-input>([\s\S]*?)</bash-input>`.
fn bash_input(text: &str) -> Option<&str> {
    const OPEN: &str = "<bash-input>";
    const CLOSE: &str = "</bash-input>";
    let start = text.find(OPEN)? + OPEN.len();
    let end = text[start..].find(CLOSE)?;
    Some(&text[start..start + end])
}

/// `aee`: `^(?:\s*<[a-z][\w-]*[\s>]|\[Request interrupted by user[^\]]*\])`.
fn looks_like_markup(text: &str) -> bool {
    let rest = text.trim_start_matches(is_js_whitespace);
    if let Some(after) = rest.strip_prefix('<') {
        let mut chars = after.chars();
        if chars.next().is_some_and(|first| first.is_ascii_lowercase()) {
            let tail: String = chars.collect();
            let name_length = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .count();
            if tail
                .chars()
                .nth(name_length)
                .is_some_and(|next| next == '>' || is_js_whitespace(next))
            {
                return true;
            }
        }
    }
    text.strip_prefix("[Request interrupted by user")
        .is_some_and(|after| after.contains(']'))
}

/// `_d(text, 200)`: the first 200 UTF-16 units, without a split pair.
fn truncate_units(text: &str, limit: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for character in text.chars() {
        units += character.len_utf16();
        if units > limit {
            break;
        }
        out.push(character);
    }
    out
}

/// `yd(entry, state)`: the first prompt of a user entry.
fn first_prompt(entry: &JsValue, command_fallback: &mut String) -> Option<String> {
    if str_of(entry, "type") != Some("user") {
        return None;
    }
    if entry.get("isMeta") == Some(&JsValue::Bool(true))
        || entry.get("isCompactSummary") == Some(&JsValue::Bool(true))
    {
        return None;
    }
    let message = entry
        .get("message")
        .filter(|message| spocky_contracts::js::truthy(Some(message)))?;
    let content = message.get("content");
    let mut texts: Vec<String> = Vec::new();
    match content {
        Some(JsValue::String(text)) => texts.push(text.clone()),
        Some(JsValue::Array(blocks)) => {
            for block in blocks {
                if !matches!(block, JsValue::Object(_) | JsValue::Array(_)) {
                    continue;
                }
                if str_of(block, "type") == Some("tool_result") {
                    return None;
                }
                if str_of(block, "type") == Some("text")
                    && let Some(text) = block.get("text").and_then(JsValue::as_str)
                {
                    texts.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
    for text in texts {
        let flattened = text.replace('\n', " ");
        let mut candidate = js_trim(&flattened).to_owned();
        if candidate.is_empty() {
            continue;
        }
        if let Some(name) = command_name(&candidate) {
            if command_fallback.is_empty() {
                name.clone_into(command_fallback);
            }
            continue;
        }
        if let Some(input) = bash_input(&candidate) {
            return Some(format!("! {}", js_trim(input)));
        }
        if looks_like_markup(&candidate) {
            continue;
        }
        if js_length(&candidate) > 200 {
            candidate = format!("{}…", js_trim(&truncate_units(&candidate, 200)));
        }
        return Some(candidate);
    }
    None
}

/// `Ey(head)`: the first user prompt in a transcript head, else the first
/// slash command's name.
fn first_prompt_of_text(head: &str) -> String {
    let mut command_fallback = String::new();
    for line in head.split('\n') {
        if !line.contains("\"type\":\"user\"") && !line.contains("\"type\": \"user\"") {
            continue;
        }
        if line.contains("\"tool_result\"") {
            continue;
        }
        if line.contains("\"isMeta\":true") || line.contains("\"isMeta\": true") {
            continue;
        }
        if line.contains("\"isCompactSummary\":true") || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }
        let Ok(entry) = parse(line) else {
            continue;
        };
        if let Some(prompt) = first_prompt(&entry, &mut command_fallback) {
            return prompt;
        }
    }
    command_fallback
}

/// `x4(title)`: `Eze(gy(title.trim())).trim()`.
fn clean_title(title: &str) -> String {
    let flattened = control_runs_to_space(js_trim(title));
    let stripped: String = flattened
        .chars()
        .filter(|c| !matches!(*c as u32, 0..=0x1f | 0x7f..=0x9f))
        .take(TITLE_CODE_POINTS)
        .collect();
    js_trim(&stripped).to_owned()
}

/// `gy(text)`: runs of `\p{Cc}`, `\p{Cf}`, U+2028 and U+2029 become one space.
fn control_runs_to_space(text: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for character in text.chars() {
        if is_control_or_format(character) {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(character);
            in_run = false;
        }
    }
    out
}

/// `\p{Cc}`, `\p{Cf}`, ` `, ` `.
#[must_use]
pub fn is_control_or_format(character: char) -> bool {
    let code = character as u32;
    matches!(code, 0..=0x1f | 0x7f..=0x9f | 0x2028 | 0x2029)
        || matches!(
            code,
            0xad | 0x600..=0x605
                | 0x61c
                | 0x6dd
                | 0x70f
                | 0x890..=0x891
                | 0x8e2
                | 0x180e
                | 0x200b..=0x200f
                | 0x202a..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x110bd
                | 0x110cd
                | 0x13430..=0x1343f
                | 0x1bca0..=0x1bca3
                | 0x1d173..=0x1d17a
                | 0xe0001
                | 0xe0020..=0xe007f
        )
}

/// The records a transcript holds (`qze`).
#[derive(Default)]
struct ParsedTranscript {
    transcript: Vec<JsValue>,
    content_replacements: Vec<JsValue>,
    relocated_cwd: Option<String>,
    history_suppressed: bool,
    atis_latch: Option<String>,
}

/// `/^[\x21-\x7e]*$/`.
fn is_printable_ascii(value: &str) -> bool {
    value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

/// `qze(buffer, sessionId)`.
fn parse_transcript(buffer: &[u8], session_id: &str) -> ParsedTranscript {
    let mut parsed = ParsedTranscript::default();
    for raw in buffer.split(|byte| *byte == 10) {
        let start = raw.iter().position(|byte| *byte > 32);
        let Some(start) = start else {
            continue;
        };
        let line = String::from_utf8_lossy(&raw[start..]);
        let Ok(entry) = parse(&line) else {
            continue;
        };
        if !matches!(entry, JsValue::Object(_)) {
            continue;
        }
        let kind = str_of(&entry, "type").map(str::to_owned);
        let kind = kind.as_deref();
        if matches!(
            kind,
            Some("user" | "assistant" | "attachment" | "system" | "progress")
        ) && entry
            .get("uuid")
            .is_some_and(|uuid| uuid.as_str().is_some())
        {
            parsed.transcript.push(entry);
        } else if kind == Some("history-suppression") {
            parsed.history_suppressed = true;
        } else if kind == Some("atis-latch")
            && str_of(&entry, "sessionId") == Some(session_id)
            && let Some(atis) = str_of(&entry, "atis").filter(|atis| is_printable_ascii(atis))
        {
            parsed.atis_latch = Some(atis.to_owned());
        } else if kind == Some("content-replacement")
            && str_of(&entry, "sessionId") == Some(session_id)
            && let Some(replacements) = entry.get("replacements").and_then(JsValue::as_array)
        {
            parsed
                .content_replacements
                .extend(replacements.iter().cloned());
        } else if kind == Some("relocated")
            && str_of(&entry, "sessionId") == Some(session_id)
            && let Some(cwd) = str_of(&entry, "relocatedCwd").filter(|cwd| !cwd.is_empty())
        {
            parsed.relocated_cwd = Some(cwd.to_owned());
        }
    }
    parsed
}

fn iso_now() -> String {
    // `new Date().toISOString()`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_iso(now.as_millis())
}

/// `Date#toISOString` for an epoch in milliseconds (years 1970 to 9999).
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // Epoch milliseconds.
fn format_iso(millis: u128) -> String {
    let seconds = (millis / 1000) as i64;
    let fraction = (millis % 1000) as u32;
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:03}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

/// The title `z4` falls back to when no title is given, from the file's head
/// and tail (`Yze`).
fn derive_title(buffer: &[u8], sidecar_title: Option<&str>) -> String {
    let length = buffer.len();
    let head = String::from_utf8_lossy(&buffer[..length.min(HEAD_TAIL_BYTES)]).into_owned();
    let tail =
        String::from_utf8_lossy(&buffer[length.saturating_sub(HEAD_TAIL_BYTES)..]).into_owned();
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let tail_title = last_string_field(&tail, "customTitle");
    let preferred = match tail_title {
        Some(title) => Some(title),
        None => sidecar_title
            .map(str::to_owned)
            .or_else(|| last_string_field(&head, "customTitle")),
    };
    non_empty(preferred)
        .or_else(|| non_empty(last_string_field(&tail, "aiTitle")))
        .or_else(|| non_empty(last_string_field(&head, "aiTitle")))
        .or_else(|| non_empty(Some(first_prompt_of_text(&head))))
        .unwrap_or_default()
}

/// `z4(parsed, sessionId, options, title)`: the entries of the fork.
#[allow(clippy::too_many_lines)] // `z4`.
fn build_fork(
    parsed: &ParsedTranscript,
    session_id: &str,
    up_to_message_id: Option<&str>,
    title: &dyn Fn() -> String,
) -> Result<(Vec<JsValue>, String), AgentError> {
    let mut kept: Vec<&JsValue> = parsed
        .transcript
        .iter()
        .filter(|entry| !spocky_contracts::js::truthy(entry.get("isSidechain")))
        .collect();
    if kept.is_empty() {
        return Err(AgentError::new(format!(
            "Session {session_id} has no messages to fork"
        )));
    }
    if let Some(target) = up_to_message_id.filter(|target| !target.is_empty()) {
        let position = kept
            .iter()
            .position(|entry| str_of(entry, "uuid") == Some(target))
            .ok_or_else(|| {
                AgentError::new(format!(
                    "Message {target} not found in session {session_id}"
                ))
            })?;
        kept.truncate(position + 1);
    }
    let mut new_ids: HashMap<&str, String> = HashMap::new();
    for entry in &kept {
        if let Some(uuid) = str_of(entry, "uuid") {
            new_ids.insert(uuid, random_uuid());
        }
    }
    let visible: Vec<&JsValue> = kept
        .iter()
        .copied()
        .filter(|entry| str_of(entry, "type") != Some("progress"))
        .collect();
    if visible.is_empty() {
        return Err(AgentError::new(format!(
            "Session {session_id} has no messages to fork"
        )));
    }
    let mut by_uuid: HashMap<&str, &JsValue> = HashMap::new();
    for entry in kept.iter().copied() {
        if let Some(uuid) = str_of(entry, "uuid") {
            by_uuid.insert(uuid, entry);
        }
    }
    let forked_id = random_uuid();
    let now = iso_now();
    let mut entries: Vec<JsValue> = Vec::new();
    if parsed.history_suppressed {
        entries.push(object(vec![
            ("type", text("history-suppression")),
            ("sessionId", text(&forked_id)),
            ("cause", text("fork_inherit")),
            ("ts", text(&iso_now())),
        ]));
    }
    for (position, entry) in visible.iter().copied().enumerate() {
        let uuid = str_of(entry, "uuid").unwrap_or_default();
        let new_uuid = new_ids.get(uuid).cloned().unwrap_or_default();
        let mut parent: JsValue = JsValue::Null;
        let mut cursor = entry
            .get("parentUuid")
            .and_then(JsValue::as_str)
            .filter(|id| !id.is_empty());
        while let Some(parent_id) = cursor {
            let Some(ancestor) = by_uuid.get(parent_id) else {
                break;
            };
            if str_of(ancestor, "type") != Some("progress") {
                parent = new_ids
                    .get(parent_id)
                    .map_or(JsValue::Null, |id| JsValue::String(id.clone()));
                break;
            }
            cursor = ancestor
                .get("parentUuid")
                .and_then(JsValue::as_str)
                .filter(|id| !id.is_empty());
        }
        let timestamp = if position == visible.len() - 1 {
            JsValue::String(now.clone())
        } else {
            entry
                .get("timestamp")
                .cloned()
                .unwrap_or(JsValue::Undefined)
        };
        let logical_parent = match entry.get("logicalParentUuid") {
            None | Some(JsValue::Undefined) => JsValue::Undefined,
            Some(JsValue::String(id)) => new_ids
                .get(id.as_str())
                .map_or(JsValue::Null, |id| JsValue::String(id.clone())),
            Some(_) => JsValue::Null,
        };
        let mut forked = spocky_contracts::js::spread(Some(entry));
        if str_of(entry, "type") == Some("system")
            && str_of(entry, "subtype") == Some("model_refusal_fallback")
        {
            forked.insert("neutralizedByFork", JsValue::Bool(true));
        }
        forked.insert("uuid", JsValue::String(new_uuid));
        forked.insert("parentUuid", parent);
        forked.insert("logicalParentUuid", logical_parent);
        forked.insert("sessionId", text(&forked_id));
        forked.insert("timestamp", timestamp);
        forked.insert("isSidechain", JsValue::Bool(false));
        for key in [
            "teamName",
            "agentName",
            "sessionKind",
            "slug",
            "sourceToolAssistantUUID",
        ] {
            forked.insert(key, JsValue::Undefined);
        }
        forked.insert(
            "forkedFrom",
            object(vec![
                ("sessionId", text(session_id)),
                (
                    "messageUuid",
                    entry.get("uuid").cloned().unwrap_or(JsValue::Undefined),
                ),
            ]),
        );
        entries.push(JsValue::Object(forked));
    }
    if !parsed.content_replacements.is_empty() {
        entries.push(object(vec![
            ("type", text("content-replacement")),
            ("sessionId", text(&forked_id)),
            (
                "replacements",
                JsValue::Array(parsed.content_replacements.clone()),
            ),
            ("uuid", text(&random_uuid())),
            ("timestamp", text(&now)),
        ]));
    }
    if let Some(atis) = &parsed.atis_latch {
        entries.push(object(vec![
            ("type", text("atis-latch")),
            ("sessionId", text(&forked_id)),
            ("atis", text(atis)),
        ]));
    }
    if let Some(cwd) = &parsed.relocated_cwd {
        entries.push(object(vec![
            ("type", text("relocated")),
            ("sessionId", text(&forked_id)),
            ("relocatedCwd", text(cwd)),
        ]));
    }
    let derived = title();
    let heading = if derived.is_empty() {
        "Forked session".to_owned()
    } else {
        derived
    };
    entries.push(object(vec![
        ("type", text("custom-title")),
        ("sessionId", text(&forked_id)),
        ("customTitle", text(&format!("{heading} (fork)"))),
        ("uuid", text(&random_uuid())),
        ("timestamp", text(&now)),
    ]));
    Ok((entries, forked_id))
}

/// `readdir` names in libuv's order.
fn list_names(directory: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

/// `Gze(sessionId)` without a `dir`: the first project directory that holds a
/// non-empty `<sessionId>.jsonl`.
fn find_session(projects: &str, session_id: &str) -> Option<(Vec<u8>, String)> {
    let names = list_names(Path::new(projects)).ok()?;
    let file = format!("{session_id}.jsonl");
    for name in names {
        let project_dir = join_path(projects, &name);
        if let Ok(bytes) = std::fs::read(join_path(&project_dir, &file))
            && !bytes.is_empty()
        {
            return Some((bytes, project_dir));
        }
    }
    None
}

/// `ku(path, sessionId)`: the title in `<projectDir>/<sessionId>/custom-title.json`.
fn sidecar_title(project_dir: &str, session_id: &str) -> Option<String> {
    let path = join_path(&join_path(project_dir, session_id), "custom-title.json");
    let contents = std::fs::read(path).ok()?;
    let parsed = parse(&String::from_utf8_lossy(&contents)).ok()?;
    let title = parsed.get("customTitle").and_then(JsValue::as_str)?;
    let cleaned = clean_title(title);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// `claudeForkSession(sessionId, { upToMessageId })` against `config_dir`
/// (`CLAUDE_CONFIG_DIR` or `~/.claude`, NFC).
///
/// # Errors
///
/// The SDK's errors: a malformed id, a missing or empty session, a message
/// that is not in it, or a failed write.
pub fn fork_session_in(
    config_dir: &str,
    session_id: &str,
    up_to_message_id: Option<&str>,
) -> Result<String, AgentError> {
    if !is_uuid(session_id) {
        return Err(AgentError::new(format!("Invalid sessionId: {session_id}")));
    }
    if let Some(target) = up_to_message_id.filter(|target| !target.is_empty())
        && !is_uuid(target)
    {
        return Err(AgentError::new(format!("Invalid upToMessageId: {target}")));
    }
    let projects = join_path(config_dir, "projects");
    let Some((buffer, project_dir)) = find_session(&projects, session_id) else {
        return Err(AgentError::new(format!("Session {session_id} not found")));
    };
    let sidecar = sidecar_title(&project_dir, session_id);
    let parsed = parse_transcript(&buffer, session_id);
    let (entries, forked_id) = build_fork(&parsed, session_id, up_to_message_id, &|| {
        derive_title(&buffer, sidecar.as_deref())
    })?;
    let path = join_path(&project_dir, &format!("{forked_id}.jsonl"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| AgentError::new(error.to_string()))?;
    for entry in &entries {
        file.write_all((stringify(entry) + "\n").as_bytes())
            .map_err(|error| AgentError::new(error.to_string()))?;
    }
    Ok(forked_id)
}

/// The daemon's Claude config directory: `CLAUDE_CONFIG_DIR`, else
/// `~/.claude`, NFC-normalized.
fn process_config_dir() -> String {
    let dir =
        std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| join_path(&home_dir(), ".claude"));
    dir.nfc().collect()
}

/// `realClaudeRewindSdk`.
pub struct RealRewindSdk;

impl RewindSdk for RealRewindSdk {
    fn fork_session(
        &self,
        session_id: &str,
        up_to_message_id: &str,
    ) -> LocalBoxFuture<'static, Result<String, AgentError>> {
        let session_id = session_id.to_owned();
        let up_to = up_to_message_id.to_owned();
        Box::pin(async move {
            let config_dir = process_config_dir();
            tokio::task::spawn_blocking(move || {
                fork_session_in(&config_dir, &session_id, Some(&up_to))
            })
            .await
            .map_err(|error| AgentError::new(error.to_string()))?
        })
    }
}
