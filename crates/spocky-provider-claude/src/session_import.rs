//! `listImportableSessions` and `importSession` of `ClaudeAgentClient`
//! (`agent.ts`: `collectRecentClaudeSessions`, `parseClaudeSessionDescriptor`;
//! `provider-session-import.ts`: `importSessionFromPersistence`).

use std::path::Path;

use spocky_contracts::js_value::{JsObject, JsValue, js_text_from_utf16, js_text_utf16, parse};
use spocky_contracts::text::{is_js_whitespace, js_trim};
use spocky_session::agent_sdk::{
    AgentError, ImportProviderSessionContext, ImportProviderSessionInput,
    ImportableProviderSession, ImportedProviderSession, ImportedTimelineEntry,
    ListImportableSessionsOptions,
};

use crate::project_dir::{claude_config_dir, claude_project_dir, join_path};
use crate::transcript::{extract_claude_user_text, is_synthetic_user_entry};

/// `readdir` names in libuv's order.
fn list_names(directory: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

/// A `.jsonl` file and its modification time in epoch milliseconds.
struct Candidate {
    path: String,
    mtime_millis: i64,
}

/// `fileStats.mtime.getTime()`.
fn mtime_millis(path: &str) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let millis = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).ok()?,
        Err(before) => -i64::try_from(before.duration().as_millis()).ok()?,
    };
    Some(millis)
}

/// `array.slice(0, end)` for a JavaScript number `end`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // List lengths are far below 2^52; `end` is truncated and in range.
fn slice_end(length: usize, end: f64) -> usize {
    if end.is_nan() {
        return 0;
    }
    let end = end.trunc();
    if end < 0.0 {
        let from_end = length as f64 + end;
        return if from_end < 0.0 { 0 } else { from_end as usize };
    }
    if end >= length as f64 {
        length
    } else {
        end as usize
    }
}

/// `collectRecentClaudeSessions(root, limit, { rootIsProjectDir })`; the
/// baseline's `endsWith` is case-sensitive.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn collect_recent_sessions(root: &str, limit: f64, root_is_project_dir: bool) -> Vec<Candidate> {
    let Ok(root_entries) = list_names(Path::new(root)) else {
        return Vec::new();
    };
    let files: Vec<String> = if root_is_project_dir {
        root_entries
            .iter()
            .filter(|file| file.ends_with(".jsonl"))
            .map(|file| join_path(root, file))
            .collect()
    } else {
        let mut all = Vec::new();
        for directory in &root_entries {
            let project_path = join_path(root, directory);
            let Ok(metadata) = std::fs::metadata(&project_path) else {
                continue;
            };
            if !metadata.is_dir() {
                continue;
            }
            let Ok(names) = list_names(Path::new(&project_path)) else {
                continue;
            };
            all.extend(
                names
                    .iter()
                    .filter(|file| file.ends_with(".jsonl"))
                    .map(|file| join_path(&project_path, file)),
            );
        }
        all
    };
    let mut candidates: Vec<Candidate> = files
        .into_iter()
        .filter_map(|path| mtime_millis(&path).map(|mtime_millis| Candidate { path, mtime_millis }))
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.mtime_millis));
    candidates.truncate(slice_end(candidates.len(), limit));
    candidates
}

/// `/"type"\s*:\s*"(?:user|custom-title|ai-title)"/`.
fn has_descriptor_record(line: &str) -> bool {
    const KEY: &str = "\"type\"";
    let mut from = 0;
    while let Some(offset) = line[from..].find(KEY) {
        let after = from + offset + KEY.len();
        let rest = line[after..].trim_start_matches(is_js_whitespace);
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start_matches(is_js_whitespace);
            if let Some(rest) = rest.strip_prefix('"')
                && ["user\"", "custom-title\"", "ai-title\""]
                    .iter()
                    .any(|word| rest.starts_with(word))
            {
                return true;
            }
        }
        from += offset + 1;
    }
    false
}

/// `/"<key>"\s*:/`.
fn has_key(line: &str, key: &str) -> bool {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(offset) = line[from..].find(&needle) {
        let after = from + offset + needle.len();
        if line[after..]
            .trim_start_matches(is_js_whitespace)
            .starts_with(':')
        {
            return true;
        }
        from += offset + 1;
    }
    false
}

#[derive(Default)]
struct Accumulator {
    session_id: Option<String>,
    cwd: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    first_prompt_title: Option<String>,
    first_prompt_preview: Option<String>,
    last_prompt_preview: Option<String>,
}

/// `normalizeClaudeSessionTitle(title)`.
fn normalize_title(title: &str) -> Option<String> {
    let trimmed = js_trim(title);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `normalizeImportablePromptPreview(text)`.
fn normalize_preview(text: &str) -> Option<String> {
    let trimmed = js_trim(text);
    let mut collapsed = String::new();
    let mut in_space = false;
    for character in trimmed.chars() {
        if is_js_whitespace(character) {
            if !in_space {
                collapsed.push(' ');
                in_space = true;
            }
        } else {
            collapsed.push(character);
            in_space = false;
        }
    }
    if collapsed.is_empty() {
        return None;
    }
    let units: Vec<u16> = js_text_utf16(&collapsed).collect();
    if units.len() > 160 {
        return Some(js_text_from_utf16(&units[..160]));
    }
    Some(collapsed)
}

fn is_falsy_string(value: Option<&String>) -> bool {
    value.is_none_or(String::is_empty)
}

/// `applyClaudeSessionEntryToAccumulator(entry, acc)`.
fn apply_entry(entry: &JsValue, acc: &mut Accumulator) {
    if !matches!(entry, JsValue::Object(_)) {
        return;
    }
    if spocky_contracts::js::truthy(entry.get("isSidechain")) {
        return;
    }
    let kind = entry.get("type").and_then(JsValue::as_str);
    if kind == Some("user") && is_synthetic_user_entry(entry) {
        return;
    }
    if is_falsy_string(acc.session_id.as_ref())
        && let Some(id) = entry.get("sessionId").and_then(JsValue::as_str)
    {
        acc.session_id = Some(id.to_owned());
    }
    if is_falsy_string(acc.cwd.as_ref())
        && let Some(cwd) = entry.get("cwd").and_then(JsValue::as_str)
    {
        acc.cwd = Some(cwd.to_owned());
    }
    if kind == Some("custom-title")
        && let Some(title) = entry.get("customTitle").and_then(JsValue::as_str)
    {
        acc.custom_title = normalize_title(title);
        return;
    }
    if kind == Some("ai-title")
        && let Some(title) = entry.get("aiTitle").and_then(JsValue::as_str)
    {
        acc.ai_title = normalize_title(title);
        return;
    }
    if kind == Some("user")
        && spocky_contracts::js::truthy(entry.get("message"))
        && let Some(text) = extract_claude_user_text(entry.get("message"))
    {
        if acc.first_prompt_title.is_none() {
            acc.first_prompt_title = Some(text.clone());
        }
        let preview = normalize_preview(&text);
        if acc.first_prompt_preview.is_none() {
            acc.first_prompt_preview.clone_from(&preview);
        }
        acc.last_prompt_preview = preview;
    }
}

/// `parseClaudeSessionDescriptor(filePath, mtime)`.
fn parse_descriptor(candidate: &Candidate) -> Option<ImportableProviderSession> {
    let bytes = std::fs::read(&candidate.path).ok()?;
    let content = String::from_utf8_lossy(&bytes);
    let mut acc = Accumulator::default();
    let segments: Vec<&str> = content.split('\n').collect();
    for (index, segment) in segments.iter().enumerate() {
        // `/\r?\n/`: a carriage return only counts before a newline.
        let line = if index + 1 < segments.len() {
            segment.strip_suffix('\r').unwrap_or(segment)
        } else {
            segment
        };
        if line.is_empty() {
            continue;
        }
        let relevant = has_descriptor_record(line)
            || (is_falsy_string(acc.session_id.as_ref()) && has_key(line, "sessionId"))
            || (is_falsy_string(acc.cwd.as_ref()) && has_key(line, "cwd"));
        if !relevant {
            continue;
        }
        let Ok(entry) = parse(line) else {
            continue;
        };
        apply_entry(&entry, &mut acc);
    }
    let session_id = acc.session_id.filter(|id| !id.is_empty())?;
    let cwd = acc.cwd.filter(|cwd| !cwd.is_empty())?;
    let units: Vec<u16> = js_text_utf16(&session_id).collect();
    let short = js_text_from_utf16(&units[..units.len().min(8)]);
    let title = acc
        .custom_title
        .or(acc.ai_title)
        .or_else(|| acc.first_prompt_title.as_deref().and_then(normalize_title))
        .unwrap_or_else(|| format!("Claude session {short}"));
    #[allow(clippy::cast_precision_loss)] // Epoch milliseconds.
    let last_activity = candidate.mtime_millis as f64;
    Some(ImportableProviderSession {
        provider_handle_id: session_id,
        cwd,
        title: Some(title),
        first_prompt_preview: acc.first_prompt_preview,
        last_prompt_preview: acc.last_prompt_preview,
        last_activity_at_millis: last_activity,
    })
}

/// `ClaudeAgentClient.listImportableSessions(options)` against `env`, the
/// provider env.
#[must_use]
pub fn list_importable_sessions(
    env: &JsObject,
    options: Option<&ListImportableSessionsOptions>,
) -> Vec<ImportableProviderSession> {
    let config_dir = claude_config_dir(env);
    let cwd = options
        .and_then(|options| options.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty());
    let sessions_root = match cwd {
        Some(cwd) => claude_project_dir(cwd, &config_dir),
        None => join_path(&config_dir, "projects"),
    };
    if !Path::new(&sessions_root).exists() {
        return Vec::new();
    }
    let limit = options.and_then(|options| options.limit).unwrap_or(20.0);
    let scan_limit = options
        .and_then(|options| options.scan_limit)
        .unwrap_or(limit * 3.0);
    let scan_limit = if scan_limit.is_nan() {
        scan_limit
    } else {
        scan_limit.min(500.0)
    };
    let candidates = collect_recent_sessions(&sessions_root, scan_limit, cwd.is_some());
    let mut parsed: Vec<ImportableProviderSession> =
        candidates.iter().filter_map(parse_descriptor).collect();
    let keep = slice_end(parsed.len(), limit);
    parsed.truncate(keep);
    parsed
}

/// `importSessionFromPersistence`'s inputs for Claude: the persistence handle
/// the import resumes, and the configs it reports.
pub struct ImportPlan {
    /// The resolved session config.
    pub config: JsValue,
    /// The config stored with the import.
    pub stored_config: JsValue,
    /// `AgentPersistenceHandle`.
    pub persistence: JsValue,
}

fn with_provider_and_cwd(base: &JsValue, cwd: &str) -> JsObject {
    let mut merged = spocky_contracts::js::spread(Some(base));
    merged.insert("provider", JsValue::String("claude".to_owned()));
    merged.insert("cwd", JsValue::String(cwd.to_owned()));
    merged
}

/// The configs and persistence handle of `importSessionFromPersistence`.
#[must_use]
pub fn plan_import(
    input: &ImportProviderSessionInput,
    context: &ImportProviderSessionContext,
) -> ImportPlan {
    let config = with_provider_and_cwd(&context.config, &input.cwd);
    let stored = with_provider_and_cwd(&context.stored_config, &input.cwd);
    let mut metadata = spocky_contracts::js::spread(Some(&JsValue::Object(stored.clone())));
    metadata.insert("provider", JsValue::String("claude".to_owned()));
    metadata.insert("cwd", JsValue::String(input.cwd.clone()));
    let mut persistence = JsObject::new();
    persistence.insert("provider", JsValue::String("claude".to_owned()));
    persistence.insert(
        "sessionId",
        JsValue::String(input.provider_handle_id.clone()),
    );
    persistence.insert(
        "nativeHandle",
        JsValue::String(input.provider_handle_id.clone()),
    );
    persistence.insert("metadata", JsValue::Object(metadata));
    ImportPlan {
        config: JsValue::Object(config),
        stored_config: JsValue::Object(stored),
        persistence: JsValue::Object(persistence),
    }
}

/// `collectImportedHistory(events)`: the timeline rows and the
/// `provider_subagent` events of a replayed history.
#[must_use]
pub fn collect_imported_history(
    events: Vec<JsValue>,
) -> (Vec<ImportedTimelineEntry>, Vec<JsValue>) {
    let mut timeline = Vec::new();
    let mut subagents = Vec::new();
    for event in events {
        match event.get("type").and_then(JsValue::as_str) {
            Some("provider_subagent") => subagents.push(event),
            Some("timeline") => {
                let timestamp = event
                    .get("timestamp")
                    .filter(|stamp| spocky_contracts::js::truthy(Some(stamp)))
                    .and_then(JsValue::as_str)
                    .map(str::to_owned);
                timeline.push(ImportedTimelineEntry {
                    item: event.get("item").cloned().unwrap_or(JsValue::Undefined),
                    timestamp,
                });
            }
            _ => {}
        }
    }
    (timeline, subagents)
}

/// The assembled result once the session is resumed.
#[must_use]
pub fn imported_session(
    session: std::sync::Arc<dyn spocky_session::agent_sdk::AgentSession>,
    plan: ImportPlan,
    history: Vec<JsValue>,
) -> ImportedProviderSession {
    let (timeline, subagents) = collect_imported_history(history);
    ImportedProviderSession {
        session,
        config: plan.stored_config,
        persistence: plan.persistence,
        timeline,
        provider_subagent_events: Some(subagents),
    }
}

/// Drains a history stream into events.
///
/// # Errors
///
/// The stream's error.
pub async fn drain_history(
    mut stream: Box<dyn spocky_session::agent_sdk::AgentEventStream>,
) -> Result<Vec<JsValue>, AgentError> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}
