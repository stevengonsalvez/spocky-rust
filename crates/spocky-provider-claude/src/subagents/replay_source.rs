//! `subagents/replay-source.ts`: subagent observations rebuilt from a
//! persisted Claude session (`<session>/subagents/agent-<id>.jsonl` and its
//! `agent-<id>.meta.json` sidecar), in the live source's vocabulary.

use std::collections::HashMap;

use spocky_contracts::js_value::{JsObject, JsValue, parse};
use spocky_contracts::text::js_trim;
use spocky_session::agent_sdk::AgentError;

use super::observation::{SubagentObservation, SubagentStatus};
use super::presentation::{PresentationFacts, build_claude_subagent_subtitle};
use crate::models::resolve_observed_claude_model_id;
use crate::timestamps::normalize_replay_timestamp;

/// `ClaudeSubagentMeta`: every field falls back to absent when malformed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeSubagentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    pub spawn_depth: Option<f64>,
}

/// `parseClaudeSubagentMeta(contents)`.
#[must_use]
pub fn parse_claude_subagent_meta(contents: &str) -> Option<ClaudeSubagentMeta> {
    let parsed = parse(contents).ok()?;
    let record = parsed.as_object()?;
    let string = |key: &str| record.get(key).and_then(JsValue::as_str).map(str::to_owned);
    let meta = ClaudeSubagentMeta {
        agent_type: string("agentType"),
        description: string("description"),
        tool_use_id: string("toolUseId"),
        spawn_depth: record
            .get("spawnDepth")
            .and_then(JsValue::as_f64)
            .filter(|depth| depth.is_finite()),
    };
    (meta != ClaudeSubagentMeta::default()).then_some(meta)
}

/// One Task call the parent declared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayToolCall {
    pub title: Option<String>,
    pub description: Option<String>,
}

/// `ClaudeReplayParentFacts`.
#[derive(Debug, Clone, Default)]
pub struct ClaudeReplayParentFacts {
    /// Task `tool_use` id to its declared identity.
    pub tool_calls: HashMap<String, ReplayToolCall>,
    /// agentId to `(toolCallId, failed)`, scraped from tool results.
    pub links_by_agent_id: HashMap<String, (String, bool)>,
    /// Task `tool_use` id to whether it failed.
    pub outcomes_by_tool_call_id: HashMap<String, bool>,
}

/// `ClaudeReplaySubagentInput`.
#[derive(Debug, Clone)]
pub struct ClaudeReplaySubagentInput {
    pub agent_id: String,
    pub meta: Option<ClaudeSubagentMeta>,
    /// History entries (objects).
    pub entries: Vec<JsObject>,
    pub parent_facts: Option<ClaudeReplayParentFacts>,
}

/// Converts one history entry to timeline items; it may throw.
pub type ConvertEntry<'a> = &'a mut dyn FnMut(&JsObject) -> Result<Vec<JsValue>, AgentError>;

fn entry_type(entry: &JsObject) -> Option<&str> {
    entry.get("type").and_then(JsValue::as_str)
}

fn message_of(entry: &JsObject) -> Option<&JsValue> {
    entry
        .get("message")
        .filter(|message| !matches!(message, JsValue::Undefined | JsValue::Null))
}

fn read_runtime(entries: &[JsObject]) -> (Option<String>, Option<String>) {
    let mut model = None;
    let mut effort = None;
    for entry in entries {
        if entry_type(entry) != Some("assistant") {
            continue;
        }
        if let Some(observed) = entry
            .get("effort")
            .and_then(JsValue::as_str)
            .map(js_trim)
            .filter(|effort| !effort.is_empty())
        {
            effort = Some(observed.to_owned());
        }
        if let Some(observed) = resolve_observed_claude_model_id(
            message_of(entry)
                .and_then(|message| message.get("model"))
                .and_then(JsValue::as_str),
        ) {
            model = Some(observed);
        }
    }
    (model, effort)
}

/// `readTotalTokens(raw)`.
fn read_total_tokens(raw: Option<&JsValue>) -> Option<f64> {
    let usage = raw?.as_object()?;
    let counters: Vec<Option<f64>> = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .iter()
    .map(|key| {
        usage
            .get(key)
            .and_then(JsValue::as_f64)
            .filter(|count| count.is_finite())
    })
    .collect();
    if counters.iter().all(Option::is_none) {
        return None;
    }
    Some(counters.iter().map(|count| count.unwrap_or(0.0)).sum())
}

fn read_usage(entries: &[JsObject]) -> Option<f64> {
    let mut total = None;
    for entry in entries {
        if entry_type(entry) != Some("assistant") {
            continue;
        }
        if let Some(observed) =
            read_total_tokens(message_of(entry).and_then(|message| message.get("usage")))
        {
            total = Some(observed);
        }
    }
    total
}

struct ParentLink {
    id: String,
    tool_call_id: String,
    status: Option<SubagentStatus>,
}

fn outcome_status(failed: bool) -> SubagentStatus {
    if failed {
        SubagentStatus::Failed
    } else {
        SubagentStatus::Completed
    }
}

fn resolve_parent_link(
    subagent: &ClaudeReplaySubagentInput,
    parent: &ClaudeReplayParentFacts,
) -> Option<ParentLink> {
    if let Some(meta_id) = subagent
        .meta
        .as_ref()
        .and_then(|meta| meta.tool_use_id.as_deref())
        .map(js_trim)
        .filter(|id| !id.is_empty())
        && parent.tool_calls.contains_key(meta_id)
    {
        return Some(ParentLink {
            id: meta_id.to_owned(),
            tool_call_id: meta_id.to_owned(),
            status: parent
                .outcomes_by_tool_call_id
                .get(meta_id)
                .map(|failed| outcome_status(*failed)),
        });
    }
    let (tool_call_id, failed) = parent.links_by_agent_id.get(&subagent.agent_id)?;
    parent
        .tool_calls
        .contains_key(tool_call_id)
        .then(|| ParentLink {
            id: tool_call_id.clone(),
            tool_call_id: tool_call_id.clone(),
            status: Some(outcome_status(*failed)),
        })
}

fn read_child_terminal_status(entries: &[JsObject]) -> Option<SubagentStatus> {
    let last = entries
        .iter()
        .rev()
        .find(|entry| entry_type(entry) == Some("assistant"))?;
    (message_of(last)
        .and_then(|message| message.get("stop_reason"))
        .and_then(JsValue::as_str)
        == Some("end_turn"))
    .then_some(SubagentStatus::Completed)
}

fn timestamp_of(entry: Option<&JsObject>) -> Option<String> {
    normalize_replay_timestamp(entry.and_then(|entry| entry.get("timestamp")))
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.filter(|value| !value.is_empty()).cloned()
}

fn observe_subagent(
    subagent: &ClaudeReplaySubagentInput,
    parent: &ClaudeReplayParentFacts,
    link: &ParentLink,
    convert_entry: ConvertEntry<'_>,
    parent_subagent_id: Option<&str>,
) -> Result<Vec<SubagentObservation>, AgentError> {
    let tool_call = parent.tool_calls.get(&link.tool_call_id);
    let meta = subagent.meta.as_ref();
    let title = tool_call
        .and_then(|call| call.title.clone())
        .or_else(|| meta.and_then(|meta| meta.agent_type.clone()));
    let description = tool_call
        .and_then(|call| call.description.clone())
        .or_else(|| meta.and_then(|meta| meta.description.clone()));
    let mut observations = vec![SubagentObservation::Declared {
        id: link.id.clone(),
        title: non_empty(title.as_ref()),
        description: non_empty(description.as_ref()),
        tool_call_id: non_empty(Some(&link.tool_call_id)),
        parent_subagent_id: parent_subagent_id
            .filter(|id| !id.is_empty())
            .map(str::to_owned),
        timestamp: timestamp_of(subagent.entries.first()),
    }];
    let (model, effort) = read_runtime(&subagent.entries);
    let total_tokens = read_usage(&subagent.entries);
    let has_details =
        model.is_some() || effort.is_some() || total_tokens.is_some_and(|tokens| tokens > 0.0);
    if has_details
        && let Some(subtitle) = build_claude_subagent_subtitle(&PresentationFacts {
            title,
            model,
            effort,
            total_tokens,
        })
    {
        observations.push(SubagentObservation::Subtitle {
            id: link.id.clone(),
            subtitle,
            timestamp: timestamp_of(subagent.entries.last()),
        });
    }
    for entry in &subagent.entries {
        let timestamp = timestamp_of(Some(entry));
        for item in convert_entry(entry)? {
            observations.push(SubagentObservation::Timeline {
                id: link.id.clone(),
                item,
                timestamp: timestamp.clone(),
            });
        }
    }
    if let Some(status) = link
        .status
        .or_else(|| read_child_terminal_status(&subagent.entries))
    {
        observations.push(SubagentObservation::Status {
            id: link.id.clone(),
            status,
            timestamp: timestamp_of(subagent.entries.last()),
        });
    }
    Ok(observations)
}

fn record_replay_tool_owners(
    owners: &mut HashMap<String, String>,
    entries: &[JsObject],
    subagent_id: &str,
) {
    for entry in entries {
        if entry_type(entry) != Some("assistant") {
            continue;
        }
        let Some(content) = message_of(entry)
            .and_then(|message| message.get("content"))
            .and_then(JsValue::as_array)
        else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(JsValue::as_str) == Some("tool_use")
                && let Some(id) = block.get("id").and_then(JsValue::as_str)
            {
                owners.insert(id.to_owned(), subagent_id.to_owned());
            }
        }
    }
}

/// `observeReplaySubagents({ subagents, parent, convertEntry })`: the
/// observations and the tool-call owners.
///
/// # Errors
///
/// A throw from `convert_entry`.
pub fn observe_replay_subagents(
    subagents: Vec<ClaudeReplaySubagentInput>,
    parent: &ClaudeReplayParentFacts,
    convert_entry: ConvertEntry<'_>,
) -> Result<(Vec<SubagentObservation>, HashMap<String, String>), AgentError> {
    let mut observations = Vec::new();
    let mut tool_owners = HashMap::new();
    let mut unresolved = subagents;
    let depth = |subagent: &ClaudeReplaySubagentInput| {
        subagent
            .meta
            .as_ref()
            .and_then(|meta| meta.spawn_depth)
            .unwrap_or(1.0)
    };
    unresolved.sort_by(|left, right| {
        (depth(left) - depth(right))
            .partial_cmp(&0.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // `resolvedParents`, in insertion order.
    let mut resolved_parents: Vec<(String, ClaudeReplayParentFacts)> = Vec::new();
    let mut made_progress = true;
    while !unresolved.is_empty() && made_progress {
        made_progress = false;
        let mut index = unresolved.len();
        while index > 0 {
            index -= 1;
            let resolved = resolve_parent_link(&unresolved[index], parent)
                .map(|link| (None, parent.clone(), link))
                .or_else(|| {
                    resolved_parents.iter().find_map(|(owner, facts)| {
                        resolve_parent_link(&unresolved[index], facts)
                            .map(|link| (Some(owner.clone()), facts.clone(), link))
                    })
                });
            let Some((owner, facts, link)) = resolved else {
                continue;
            };
            let subagent = unresolved.remove(index);
            record_replay_tool_owners(&mut tool_owners, &subagent.entries, &link.id);
            observations.extend(observe_subagent(
                &subagent,
                &facts,
                &link,
                &mut *convert_entry,
                owner.as_deref(),
            )?);
            if let Some(child_facts) = subagent.parent_facts {
                match resolved_parents.iter_mut().find(|(id, _)| *id == link.id) {
                    Some(slot) => slot.1 = child_facts,
                    None => resolved_parents.push((link.id.clone(), child_facts)),
                }
            }
            made_progress = true;
        }
    }
    Ok((observations, tool_owners))
}
