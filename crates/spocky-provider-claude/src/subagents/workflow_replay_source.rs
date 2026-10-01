//! `subagents/workflow-replay-source.ts`: workflow rows rebuilt from Claude's
//! persisted run summaries (`<session>/workflows/*.json`).

use std::collections::{HashMap, HashSet};

use spocky_contracts::js_value::{JsObject, JsValue, parse};
use spocky_contracts::text::{is_js_whitespace, js_trim};
use spocky_session::agent_sdk::AgentError;

use super::observation::{SubagentObservation, SubagentStatus};
use super::presentation::{PresentationFacts, build_claude_subagent_subtitle};
use super::replay_source::ConvertEntry;
use super::workflow_output::format_claude_workflow_result;
use crate::timestamps::{
    date_parse, iso_from_date_string, iso_from_time_value, normalize_replay_timestamp,
};

/// `ClaudeWorkflowRun`: fields other than `runId` fall back to absent.
#[derive(Debug, Clone)]
pub struct ClaudeWorkflowRun {
    pub run_id: String,
    pub timestamp: Option<String>,
    pub summary: Option<String>,
    pub workflow_name: Option<String>,
    pub status: Option<String>,
    pub start_time: Option<f64>,
    pub default_model: Option<String>,
    pub total_tokens: Option<f64>,
    pub result: Option<JsValue>,
}

/// `parseClaudeWorkflowRun(contents)`.
#[must_use]
pub fn parse_claude_workflow_run(contents: &str) -> Option<ClaudeWorkflowRun> {
    let parsed = parse(contents).ok()?;
    let record = parsed.as_object()?;
    let string = |key: &str| record.get(key).and_then(JsValue::as_str).map(str::to_owned);
    let number = |key: &str| {
        record
            .get(key)
            .and_then(JsValue::as_f64)
            .filter(|number| number.is_finite())
    };
    Some(ClaudeWorkflowRun {
        run_id: string("runId")?,
        timestamp: string("timestamp"),
        summary: string("summary"),
        workflow_name: string("workflowName"),
        status: string("status"),
        start_time: number("startTime"),
        default_model: string("defaultModel"),
        total_tokens: number("totalTokens"),
        result: record
            .get("result")
            .filter(|result| !matches!(result, JsValue::Undefined))
            .cloned(),
    })
}

fn read_string(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn content_of(entry: &JsObject) -> Option<&[JsValue]> {
    entry.get("message")?.get("content")?.as_array()
}

/// `readWorkflowLinks(entries)`: run id to the Workflow tool call id.
fn read_workflow_links(entries: &[JsObject]) -> HashMap<String, String> {
    let mut workflow_tool_call_ids = HashSet::new();
    for content in entries.iter().filter_map(content_of) {
        for block in content.iter().filter_map(JsValue::as_object) {
            if block.get("type").and_then(JsValue::as_str) != Some("tool_use")
                || block.get("name").and_then(JsValue::as_str) != Some("Workflow")
            {
                continue;
            }
            if let Some(id) = read_string(block.get("id").and_then(JsValue::as_str)) {
                workflow_tool_call_ids.insert(id);
            }
        }
    }
    let mut links = HashMap::new();
    for content in entries.iter().filter_map(content_of) {
        for block in content.iter().filter_map(JsValue::as_object) {
            if block.get("type").and_then(JsValue::as_str) != Some("tool_result") {
                continue;
            }
            let Some(tool_call_id) =
                read_string(block.get("tool_use_id").and_then(JsValue::as_str))
                    .filter(|id| workflow_tool_call_ids.contains(id))
            else {
                continue;
            };
            if let Some(run_id) =
                read_result_text(block.get("content")).and_then(|text| read_run_id(&text))
            {
                links.insert(run_id, tool_call_id);
            }
        }
    }
    links
}

/// `/(?:^|\n)Run ID:\s*(wf_[A-Za-z0-9-]+)/u`: the first match.
fn read_run_id(text: &str) -> Option<String> {
    let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(index, _)| index + 1));
    for start in starts {
        let Some(rest) = text[start..].strip_prefix("Run ID:") else {
            continue;
        };
        let rest = rest.trim_start_matches(is_js_whitespace);
        let Some(id) = rest.strip_prefix("wf_") else {
            continue;
        };
        let length = id
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
            .count();
        if length > 0 {
            return Some(format!("wf_{}", &id[..length]));
        }
    }
    None
}

fn read_result_text(content: Option<&JsValue>) -> Option<String> {
    match content? {
        JsValue::String(text) => Some(text.clone()),
        JsValue::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(JsValue::as_object)
                .filter(|block| block.get("type").and_then(JsValue::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(JsValue::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn replay_workflow_status(status: Option<&str>) -> SubagentStatus {
    match status
        .map(|status| js_trim(status).to_lowercase())
        .as_deref()
    {
        Some("completed") => SubagentStatus::Completed,
        Some("canceled" | "cancelled" | "killed" | "stopped") => SubagentStatus::Canceled,
        _ => SubagentStatus::Failed,
    }
}

/// `compareReplayTimestamps(a, b)` as an ordering.
fn compare_replay_timestamps(left: &JsObject, right: &JsObject) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let left = normalize_replay_timestamp(left.get("timestamp"));
    let right = normalize_replay_timestamp(right.get("timestamp"));
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => date_parse(&left).cmp(&date_parse(&right)),
    }
}

fn timeline(id: &str, item: JsValue, timestamp: Option<String>) -> SubagentObservation {
    SubagentObservation::Timeline {
        id: id.to_owned(),
        item,
        timestamp,
    }
}

fn message_item(kind: &str, text: &str) -> JsValue {
    let mut item = JsObject::new();
    item.insert("type", JsValue::String(kind.to_owned()));
    item.insert("text", JsValue::String(text.to_owned()));
    JsValue::Object(item)
}

/// `observeReplayWorkflows({ workflows, parentEntries, entriesByRunId,
/// convertEntry })`.
///
/// # Errors
///
/// A throw from `convert_entry`.
pub fn observe_replay_workflows<S: std::hash::BuildHasher>(
    workflows: &[ClaudeWorkflowRun],
    parent_entries: &[JsObject],
    entries_by_run_id: &HashMap<String, Vec<JsObject>, S>,
    convert_entry: ConvertEntry<'_>,
) -> Result<Vec<SubagentObservation>, AgentError> {
    let links = read_workflow_links(parent_entries);
    let mut observations = Vec::new();
    for workflow in workflows {
        let Some(id) = links.get(&workflow.run_id) else {
            continue;
        };
        let description = read_string(workflow.summary.as_deref())
            .or_else(|| read_string(workflow.workflow_name.as_deref()));
        let started_at = workflow.start_time.and_then(iso_from_time_value);
        let finished_at = workflow
            .timestamp
            .as_deref()
            .filter(|timestamp| !timestamp.is_empty())
            .and_then(iso_from_date_string);
        observations.push(SubagentObservation::Declared {
            id: id.clone(),
            title: Some("Workflow".to_owned()),
            description: description.clone(),
            tool_call_id: Some(id.clone()),
            parent_subagent_id: None,
            timestamp: started_at.clone(),
        });
        if let Some(description) = &description {
            observations.push(timeline(
                id,
                message_item("user_message", description),
                started_at.clone(),
            ));
        }
        let mut entries = entries_by_run_id
            .get(&workflow.run_id)
            .cloned()
            .unwrap_or_default();
        entries.sort_by(compare_replay_timestamps);
        let mut replayed_assistant_text = HashSet::new();
        for entry in &entries {
            let timestamp = normalize_replay_timestamp(entry.get("timestamp"));
            for item in convert_entry(entry)? {
                if item.get("type").and_then(JsValue::as_str) == Some("assistant_message") {
                    replayed_assistant_text.insert(
                        js_trim(
                            item.get("text")
                                .and_then(JsValue::as_str)
                                .unwrap_or_default(),
                        )
                        .to_owned(),
                    );
                }
                observations.push(timeline(id, item, timestamp.clone()));
            }
        }
        if let Some(result_text) = workflow
            .result
            .as_ref()
            .and_then(format_claude_workflow_result)
            .filter(|text| !replayed_assistant_text.contains(js_trim(text)))
        {
            observations.push(timeline(
                id,
                message_item("assistant_message", &result_text),
                finished_at.clone(),
            ));
        }
        let facts = PresentationFacts {
            title: Some("Workflow".to_owned()),
            model: workflow
                .default_model
                .clone()
                .filter(|model| read_string(Some(model)).is_some()),
            effort: None,
            total_tokens: workflow.total_tokens,
        };
        if let Some(subtitle) = build_claude_subagent_subtitle(&facts) {
            observations.push(SubagentObservation::Subtitle {
                id: id.clone(),
                subtitle,
                timestamp: finished_at.clone(),
            });
        }
        observations.push(SubagentObservation::Status {
            id: id.clone(),
            status: replay_workflow_status(workflow.status.as_deref()),
            timestamp: finished_at,
        });
    }
    Ok(observations)
}
