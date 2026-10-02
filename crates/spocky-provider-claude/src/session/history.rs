//! Persisted history: Claude Code's JSONL transcript, its sidechain and
//! workflow sidecars, and their conversion to timeline items.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::AgentError;

use super::timeline::{BlockOptions, is_content_chunk};
use super::{ClaudeSession, text};
use crate::project_dir::{claude_config_dir, claude_project_dir, join_path};
use crate::provider_image::is_provider_image_markdown;
use crate::subagents::observation::{SubagentObservation, fold_subagent_observations};
use crate::subagents::replay_source::{
    ClaudeReplayParentFacts, ClaudeReplaySubagentInput, ClaudeSubagentMeta, ReplayToolCall,
    observe_replay_subagents, parse_claude_subagent_meta,
};
use crate::subagents::workflow_replay_source::{
    observe_replay_workflows, parse_claude_workflow_run,
};
use crate::task_notification::{
    map_system_record_to_tool_call, map_user_content_to_tool_call,
    read_tool_use_id_from_history_record,
};
use crate::timestamps::normalize_replay_timestamp;
use crate::transcript::{
    completed_compaction_item, extract_user_message_text, is_synthetic_history_user_entry,
    is_tool_result_user_entry, is_transcript_noise_content, read_non_empty_string,
};

fn str_of<'a>(value: &'a JsValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(JsValue::as_str)
}

/// `text.split(/\r?\n/)`.
fn split_lines(content: &str) -> Vec<&str> {
    content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// `parseClaudeHistoryRecords(content)`: the object rows, corrupt rows skipped.
fn parse_claude_history_records(content: &str) -> Vec<JsObject> {
    split_lines(content)
        .into_iter()
        .filter_map(|line| {
            let trimmed = js_trim(line);
            if trimmed.is_empty() {
                return None;
            }
            parse(trimmed).ok()?.as_object().cloned()
        })
        .collect()
}

/// `normalizeHistoryBlocks(content)`.
fn normalize_history_blocks(content: Option<&JsValue>) -> Option<Vec<JsValue>> {
    match content? {
        JsValue::Array(items) => {
            let blocks: Vec<JsValue> = items
                .iter()
                .filter(|entry| is_content_chunk(entry))
                .cloned()
                .collect();
            (!blocks.is_empty()).then_some(blocks)
        }
        value if is_content_chunk(value) => Some(vec![value.clone()]),
        _ => None,
    }
}

/// `hasToolLikeBlock(block)`.
fn has_tool_like_block(block: &JsValue) -> bool {
    str_of(block, "type").is_some_and(|kind| kind.to_lowercase().contains("tool"))
}

/// `isProviderImageMessage(item)`.
fn is_provider_image_message(item: &JsValue) -> bool {
    str_of(item, "type") == Some("assistant_message")
        && str_of(item, "text").is_some_and(is_provider_image_markdown)
}

/// `convertClaudeHistoryEntry(entry, mapBlocks)`.
///
/// # Errors
///
/// A throw from `map_blocks`.
pub(crate) fn convert_claude_history_entry(
    entry: &JsValue,
    map_blocks: &dyn Fn(&JsValue) -> Result<Vec<JsValue>, AgentError>,
) -> Result<Vec<JsValue>, AgentError> {
    let entry_type = str_of(entry, "type");
    if entry_type == Some("system") && str_of(entry, "subtype") == Some("compact_boundary") {
        return Ok(vec![completed_compaction_item(entry)]);
    }
    if let Some(notification) = map_system_record_to_tool_call(entry) {
        return Ok(vec![notification]);
    }
    if spocky_contracts::js::truthy(entry.get("isCompactSummary")) {
        return Ok(Vec::new());
    }
    if entry_type == Some("user") && is_synthetic_history_user_entry(entry) {
        return Ok(Vec::new());
    }
    let Some(message) = entry.get("message").filter(|message| {
        message
            .as_object()
            .is_some_and(|object| object.get("content").is_some())
    }) else {
        return Ok(Vec::new());
    };
    let content = message.get("content");
    if matches!(entry_type, Some("user" | "assistant")) && is_transcript_noise_content(content) {
        return Ok(Vec::new());
    }
    let normalized = normalize_history_blocks(content);
    let content_value: Option<JsValue> = match content {
        Some(JsValue::String(value)) => Some(JsValue::String(value.clone())),
        _ => normalized.clone().map(JsValue::Array),
    };
    let has_tool_block = normalized
        .as_ref()
        .is_some_and(|blocks| blocks.iter().any(has_tool_like_block));
    let user_message_id = (entry_type == Some("user"))
        .then(|| str_of(entry, "uuid").filter(|uuid| !uuid.is_empty()))
        .flatten();
    if entry_type == Some("user")
        && let Some(notification) = map_user_content_to_tool_call(content, user_message_id)
    {
        return Ok(vec![notification]);
    }
    let mut timeline = Vec::new();
    if entry_type == Some("user")
        && let Some(user_text) = extract_user_message_text(content).filter(|text| !text.is_empty())
    {
        let mut item = JsObject::new();
        item.insert("type", text("user_message"));
        item.insert("text", JsValue::String(user_text));
        if let Some(id) = user_message_id {
            item.insert("messageId", text(id));
        }
        timeline.push(JsValue::Object(item));
    }
    if has_tool_block && let Some(blocks) = normalized {
        let mapped = map_blocks(&JsValue::Array(blocks))?;
        if entry_type == Some("user") {
            // Tool results emit image markdown beside the tool call; user text
            // blocks also map to assistant messages here and stay suppressed.
            let tool_items = mapped.into_iter().filter(|item| {
                str_of(item, "type") == Some("tool_call") || is_provider_image_message(item)
            });
            timeline.extend(tool_items);
            return Ok(timeline);
        }
        return Ok(mapped);
    }
    if entry_type == Some("assistant")
        && let Some(content_value) = content_value.filter(|value| {
            // A truthy content: a non-empty string or an array.
            spocky_contracts::js::truthy(Some(value))
        })
    {
        let mut items = map_blocks(&content_value)?;
        if let Some(uuid) = str_of(entry, "uuid").filter(|uuid| !uuid.is_empty()) {
            for item in &mut items {
                if str_of(item, "type") == Some("assistant_message")
                    && !spocky_contracts::js::truthy(item.get("messageId"))
                    && let JsValue::Object(object) = item
                {
                    object.insert("messageId", text(uuid));
                }
            }
        }
        return Ok(items);
    }
    Ok(timeline)
}

/// `readClaudeHistoricalSubagentToolCalls(entries)`: `(id, name, type,
/// description)` per `Task` or `Agent` call, later calls replacing earlier.
fn read_historical_subagent_tool_calls(entries: &[JsObject]) -> Vec<(String, ReplayToolCall)> {
    let mut calls: Vec<(String, ReplayToolCall)> = Vec::new();
    for entry in entries {
        let content = entry
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(JsValue::as_array);
        for block in content.unwrap_or_default() {
            if !block.is_object()
                || str_of(block, "type") != Some("tool_use")
                || !matches!(str_of(block, "name"), Some("Task" | "Agent"))
            {
                continue;
            }
            let Some(id) = str_of(block, "id") else {
                continue;
            };
            let input = block.get("input").filter(|input| input.is_object());
            let field = |key: &str| input.and_then(|input| read_non_empty_string(input.get(key)));
            let name = field("name");
            let subagent_type = field("subagent_type");
            let call = ReplayToolCall {
                title: name.or(subagent_type),
                description: field("description"),
            };
            match calls.iter_mut().find(|(existing, _)| existing == id) {
                Some(slot) => slot.1 = call,
                None => calls.push((id.to_owned(), call)),
            }
        }
    }
    calls
}

/// `/agentId:\s*([\w-]+)/` on `JSON.stringify(content)`.
fn scrape_agent_id(content: Option<&JsValue>) -> Option<String> {
    let json = content.map_or_else(|| "undefined".to_owned(), stringify);
    let mut rest = json.as_str();
    while let Some(position) = rest.find("agentId:") {
        let after = rest[position + "agentId:".len()..]
            .trim_start_matches(spocky_contracts::text::is_js_whitespace);
        let word: String = after
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
            .collect();
        if !word.is_empty() {
            return Some(word);
        }
        rest = &rest[position + 1..];
    }
    None
}

/// `readClaudeHistoricalSubagentToolResults(entries)`.
fn read_historical_subagent_tool_results(entries: &[JsObject]) -> HashMap<String, (String, bool)> {
    let mut results = HashMap::new();
    for entry in entries {
        let content = entry
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(JsValue::as_array);
        for block in content.unwrap_or_default() {
            if !block.is_object() || str_of(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = str_of(block, "tool_use_id") else {
                continue;
            };
            if let Some(agent_id) = scrape_agent_id(block.get("content")) {
                results.insert(
                    agent_id,
                    (
                        tool_use_id.to_owned(),
                        block.get("is_error") == Some(&JsValue::Bool(true)),
                    ),
                );
            }
        }
    }
    results
}

/// `readClaudeReplayParentFacts(parentEntries)`.
fn read_replay_parent_facts(parent_entries: &[JsObject]) -> ClaudeReplayParentFacts {
    let mut facts = ClaudeReplayParentFacts::default();
    for (id, call) in read_historical_subagent_tool_calls(parent_entries) {
        facts.tool_calls.insert(id, call);
    }
    for entry in parent_entries {
        let content = entry
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(JsValue::as_array);
        for block in content.unwrap_or_default() {
            if !block.is_object() || str_of(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = str_of(block, "tool_use_id") else {
                continue;
            };
            if !facts.tool_calls.contains_key(tool_use_id) {
                continue;
            }
            facts.outcomes_by_tool_call_id.insert(
                tool_use_id.to_owned(),
                block.get("is_error") == Some(&JsValue::Bool(true)),
            );
        }
    }
    facts.links_by_agent_id = read_historical_subagent_tool_results(parent_entries);
    facts
}

/// `ClaudeSidechainHistory`.
#[derive(Default)]
struct SidechainHistory {
    contents: Vec<String>,
    workflow_contents: Vec<String>,
    workflow_sidechain_contents_by_run_id: HashMap<String, Vec<String>>,
    meta_by_agent_id: HashMap<String, ClaudeSubagentMeta>,
}

/// `fs.readdirSync(dir, { withFileTypes: true })`: entries sorted by name
/// bytes, as libuv's scandir returns them.
fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<(String, PathBuf, std::fs::FileType)>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
            entry.file_type()?,
        ));
    }
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    Ok(entries)
}

/// `readClaudeSidechainHistory(historyPath)`; the baseline's `endsWith` checks
/// are case-sensitive.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn read_claude_sidechain_history(history_path: &Path) -> std::io::Result<SidechainHistory> {
    let stem = history_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let session_directory = history_path.parent().unwrap_or(Path::new("")).join(stem);
    let sidechain_directory = session_directory.join("subagents");
    let mut history = SidechainHistory::default();
    let workflow_directory = session_directory.join("workflows");
    if workflow_directory.exists() {
        for (name, path, kind) in read_dir_sorted(&workflow_directory)? {
            if !kind.is_file() || !name.ends_with(".json") {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(path) {
                history.workflow_contents.push(contents);
            }
        }
    }
    if !sidechain_directory.exists() {
        return Ok(history);
    }
    let mut directories = vec![sidechain_directory.clone()];
    while let Some(directory) = directories.pop() {
        for (name, path, kind) in read_dir_sorted(&directory)? {
            if kind.is_dir() {
                directories.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if name.ends_with(".jsonl") {
                record_sidechain_contents(&mut history, &sidechain_directory, &path)?;
                continue;
            }
            let Some(agent_id) = name
                .strip_prefix("agent-")
                .and_then(|rest| rest.strip_suffix(".meta.json"))
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Some(meta) = parse_claude_subagent_meta(&contents)
            {
                history.meta_by_agent_id.insert(agent_id.to_owned(), meta);
            }
        }
    }
    Ok(history)
}

/// `recordClaudeSidechainContents(history, sidechainDirectory, entryPath)`.
fn record_sidechain_contents(
    history: &mut SidechainHistory,
    sidechain_directory: &Path,
    entry_path: &Path,
) -> std::io::Result<()> {
    let contents = std::fs::read_to_string(entry_path)?;
    let relative: Vec<String> = entry_path
        .strip_prefix(sidechain_directory)
        .unwrap_or(entry_path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let workflow_run_id = (relative.first().map(String::as_str) == Some("workflows")
        && relative.len() >= 3)
        .then(|| relative[1].clone());
    match workflow_run_id {
        Some(run_id) => history
            .workflow_sidechain_contents_by_run_id
            .entry(run_id)
            .or_default()
            .push(contents),
        None => history.contents.push(contents),
    }
    Ok(())
}

/// `ClaudeReplayOwnership`.
#[derive(Default)]
struct ReplayOwnership {
    restored_ids: HashSet<String>,
    tool_owners: HashMap<String, String>,
}

fn is_true(value: Option<&JsValue>) -> bool {
    value == Some(&JsValue::Bool(true))
}

impl ClaudeSession {
    /// `convertHistoryEntry(entry)`.
    pub(crate) fn convert_history_entry(
        &self,
        entry: &JsValue,
    ) -> Result<Vec<JsValue>, AgentError> {
        convert_claude_history_entry(entry, &|content| {
            self.map_blocks_to_timeline(content, BlockOptions::default())
        })
    }

    /// `loadPersistedHistory(sessionId)`: failures are ignored.
    pub(crate) fn load_persisted_history(&self, session_id: &str) {
        let _ = self.try_load_persisted_history(session_id);
    }

    fn try_load_persisted_history(&self, session_id: &str) -> Result<(), AgentError> {
        self.state.borrow_mut().task_state.reset();
        let Some(history_path) = self.resolve_history_path(session_id) else {
            return Ok(());
        };
        if !Path::new(&history_path).exists() {
            return Ok(());
        }
        let content = std::fs::read(&history_path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| AgentError::new(error.to_string()))?;
        let sidechains = read_claude_sidechain_history(Path::new(&history_path))
            .map_err(|error| AgentError::new(error.to_string()))?;
        let replay = self.ingest_persisted_sidechains(&content, &sidechains)?;
        self.ingest_persisted_history(&content, &replay)
    }

    fn ingest_persisted_history(
        &self,
        content: &str,
        replay: &ReplayOwnership,
    ) -> Result<(), AgentError> {
        if content.is_empty() {
            return Ok(());
        }
        let mut timeline: Vec<(JsValue, Option<String>)> = Vec::new();
        let mut result = Ok(());
        for line in split_lines(content) {
            if let Err(error) = self.ingest_persisted_history_line(line, &mut timeline, replay) {
                result = Err(error);
                break;
            }
        }
        // Rows ingested before a throw are dropped with it, as the baseline's
        // local `timeline` array is.
        result?;
        if !timeline.is_empty() {
            let mut state = self.state.borrow_mut();
            state.persisted_history.extend(timeline);
            state.history_pending = true;
        }
        Ok(())
    }

    fn ingest_persisted_sidechains(
        &self,
        parent_content: &str,
        sidechains: &SidechainHistory,
    ) -> Result<ReplayOwnership, AgentError> {
        let parent_entries: Vec<JsObject> = parse_claude_history_records(parent_content)
            .into_iter()
            .filter(|entry| !is_true(entry.get("isSidechain")))
            .collect();
        let sidechain_entries: Vec<JsObject> = std::iter::once(parent_content)
            .chain(sidechains.contents.iter().map(String::as_str))
            .flat_map(parse_claude_history_records)
            .filter(|entry| {
                is_true(entry.get("isSidechain"))
                    && entry.get("agentId").is_some_and(JsValue::is_string)
            })
            .collect();
        // Grouped by agent id in first-seen order.
        let mut grouped: Vec<(String, Vec<JsObject>)> = Vec::new();
        for entry in sidechain_entries {
            let agent_id = entry
                .get("agentId")
                .and_then(JsValue::as_str)
                .unwrap_or_default()
                .to_owned();
            match grouped.iter_mut().find(|(id, _)| *id == agent_id) {
                Some(group) => group.1.push(entry),
                None => grouped.push((agent_id, vec![entry])),
            }
        }
        let subagents: Vec<ClaudeReplaySubagentInput> = grouped
            .into_iter()
            .map(|(agent_id, entries)| ClaudeReplaySubagentInput {
                meta: sidechains.meta_by_agent_id.get(&agent_id).cloned(),
                parent_facts: Some(read_replay_parent_facts(&entries)),
                agent_id,
                entries,
            })
            .collect();
        let parent = read_replay_parent_facts(&parent_entries);
        let mut convert =
            |entry: &JsObject| self.convert_history_entry(&JsValue::Object(entry.clone()));
        let (mut observations, tool_owners) =
            observe_replay_subagents(subagents, &parent, &mut convert)?;
        let workflows: Vec<_> = sidechains
            .workflow_contents
            .iter()
            .filter_map(|contents| parse_claude_workflow_run(contents))
            .collect();
        let entries_by_run_id: HashMap<String, Vec<JsObject>> = sidechains
            .workflow_sidechain_contents_by_run_id
            .iter()
            .map(|(run_id, contents)| {
                (
                    run_id.clone(),
                    contents
                        .iter()
                        .flat_map(|content| parse_claude_history_records(content))
                        .filter(|entry| {
                            entry.get("type").and_then(JsValue::as_str) != Some("user")
                                || is_tool_result_user_entry(&JsValue::Object(entry.clone()))
                        })
                        .collect(),
                )
            })
            .collect();
        let mut convert =
            |entry: &JsObject| self.convert_history_entry(&JsValue::Object(entry.clone()));
        observations.extend(observe_replay_workflows(
            &workflows,
            &parent_entries,
            &entries_by_run_id,
            &mut convert,
        )?);
        let restored_ids: HashSet<String> = observations
            .iter()
            .filter_map(|observation| match observation {
                SubagentObservation::Declared { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let replay = ReplayOwnership {
            restored_ids,
            tool_owners,
        };
        if observations.is_empty() {
            return Ok(replay);
        }
        let mut state = self.state.borrow_mut();
        state.persisted_provider_subagent_events.extend(
            fold_subagent_observations(&observations)
                .into_iter()
                .map(super::options::provider_subagent),
        );
        state.history_pending = true;
        drop(state);
        Ok(replay)
    }

    fn ingest_persisted_history_line(
        &self,
        line: &str,
        timeline: &mut Vec<(JsValue, Option<String>)>,
        replay: &ReplayOwnership,
    ) -> Result<(), AgentError> {
        let trimmed = js_trim(line);
        if trimmed.is_empty() {
            return Ok(());
        }
        let Some(entry) = parse(trimmed).ok().filter(JsValue::is_object) else {
            return Ok(());
        };
        if spocky_contracts::js::truthy(entry.get("isSidechain")) {
            return Ok(());
        }
        let notification_tool_use_id = read_tool_use_id_from_history_record(&entry);
        if notification_tool_use_id
            .as_ref()
            .is_some_and(|id| replay.restored_ids.contains(id))
        {
            return Ok(());
        }
        let history_timestamp = normalize_replay_timestamp(entry.get("timestamp"));
        let notification_owner = notification_tool_use_id
            .as_ref()
            .and_then(|id| replay.tool_owners.get(id));
        if let Some(owner) = notification_owner {
            for item in self.convert_history_entry(&entry)? {
                let mut event = JsObject::new();
                event.insert("type", text("timeline"));
                event.insert("id", text(owner));
                event.insert("item", item);
                event.insert(
                    "timestamp",
                    history_timestamp
                        .clone()
                        .map_or(JsValue::Undefined, JsValue::String),
                );
                self.state
                    .borrow_mut()
                    .persisted_provider_subagent_events
                    .push(super::options::provider_subagent(JsValue::Object(event)));
            }
            return Ok(());
        }
        let snapshot = self.state.borrow_mut().task_state.observe(&entry);
        let mut items: Vec<JsValue> = snapshot.into_iter().collect();
        items.extend(self.convert_history_entry(&entry)?);
        let uuid = str_of(&entry, "uuid");
        let is_visible_user_entry = str_of(&entry, "type") == Some("user")
            && uuid.is_some()
            && !is_synthetic_history_user_entry(&entry)
            && !is_tool_result_user_entry(&entry);
        if is_visible_user_entry && let Some(uuid) = uuid {
            self.remember_user_message_id(Some(uuid));
            self.remember_rewind_user_anchor(Some(uuid));
        }
        if str_of(&entry, "type") == Some("assistant")
            && let Some(uuid) = uuid
        {
            self.remember_rewind_assistant_anchor(Some(uuid));
        }
        timeline.extend(
            items
                .into_iter()
                .map(|item| (item, history_timestamp.clone())),
        );
        Ok(())
    }

    /// `resolveHistoryPath(sessionId)`.
    pub(crate) fn resolve_history_path(&self, session_id: &str) -> Option<String> {
        let cwd = self.config_str("cwd").filter(|cwd| !cwd.is_empty())?;
        let config_dir = claude_config_dir(&self.build_sdk_env());
        let mut candidates = vec![cwd.clone()];
        if let Ok(real) = std::fs::canonicalize(&cwd) {
            let real = real.to_string_lossy().into_owned();
            if real != cwd {
                candidates.push(real);
            }
        }
        for candidate in &candidates {
            let history_path = join_path(
                &claude_project_dir(candidate, &config_dir),
                &format!("{session_id}.jsonl"),
            );
            if Path::new(&history_path).exists() {
                return Some(history_path);
            }
        }
        Some(join_path(
            &claude_project_dir(&cwd, &config_dir),
            &format!("{session_id}.jsonl"),
        ))
    }
}
