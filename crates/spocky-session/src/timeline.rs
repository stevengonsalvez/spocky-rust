//! In-memory agent timeline from pinned Paseo `agent/agent-timeline-store.ts`
//! and `agent/timeline-projection.ts`.
//!
//! Items stay JavaScript values ([`JsValue`]) so merged items keep the key
//! order the baseline's object spreads produce. Rows carry a sequence number
//! per agent epoch; the projection merges tool-call lifecycles and plugin
//! items by identity and adjacent assistant and reasoning chunks, as rows
//! arrive and again when a page is selected.

use std::collections::HashMap;

use spocky_store::js_value::{JsObject, JsValue, js_number};

use crate::clock::{now_iso, random_uuid};

/// `AgentTimelineRow`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineRow {
    pub seq: i64,
    pub timestamp: String,
    pub item: JsValue,
    pub turn_id: Option<String>,
    pub provider_message_id: Option<String>,
}

/// `TimelineProjectionKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapseKind {
    AssistantMerge,
    ReasoningMerge,
    ToolLifecycle,
    Identity,
}

impl CollapseKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AssistantMerge => "assistant_merge",
            Self::ReasoningMerge => "reasoning_merge",
            Self::ToolLifecycle => "tool_lifecycle",
            Self::Identity => "identity",
        }
    }
}

/// `TimelineSeqRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqRange {
    pub start_seq: i64,
    pub end_seq: i64,
}

/// `ProjectedTimelineRow`: a `TimelineProjectionEntry` plus `seq`.
///
/// Key order as the baseline's spreads build it: `item, timestamp, turnId?,
/// providerMessageId?, seqStart, seqEnd, sourceSeqRanges, collapsed, seq`,
/// except that a `providerMessageId` added by
/// `enrichSubmittedUserMessage` comes last (`provider_message_id_last`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedRow {
    pub item: JsValue,
    pub turn_id: Option<String>,
    pub provider_message_id: Option<String>,
    pub timestamp: String,
    pub seq_start: i64,
    pub seq_end: i64,
    pub source_seq_ranges: Vec<SeqRange>,
    pub collapsed: Vec<CollapseKind>,
    /// The projection sets it to `seq_end` when it stores a row; a row merged
    /// again while a page is selected keeps the `seq` of the row it spreads.
    pub seq: i64,
    /// `providerMessageId` was added after `seq` by enrichment.
    pub provider_message_id_last: bool,
}

fn item_type(item: &JsValue) -> Option<&str> {
    item.get("type").and_then(JsValue::as_str)
}

/// `timelineItemIdentity` from `protocol/timeline-identity.ts`.
fn item_identity(item: &JsValue) -> Option<String> {
    match item_type(item) {
        Some("tool_call") => Some(js_to_string(item.get("callId"))),
        Some("plugin") => Some(format!(
            "{}/{}",
            js_to_string(item.get("pluginId")),
            js_to_string(item.get("id"))
        )),
        _ => None,
    }
}

/// JavaScript `String(value)` as template literals use it.
fn js_to_string(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".to_owned(),
        Some(JsValue::Null) => "null".to_owned(),
        Some(JsValue::Bool(flag)) => flag.to_string(),
        Some(JsValue::Number(number)) => {
            if number.is_nan() {
                "NaN".to_owned()
            } else if number.is_infinite() {
                if *number > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                }
                .to_owned()
            } else {
                js_number(*number)
            }
        }
        Some(JsValue::String(text)) => text.clone(),
        Some(JsValue::Array(items)) => items
            .iter()
            .map(|item| match item {
                JsValue::Undefined | JsValue::Null => String::new(),
                other => js_to_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(JsValue::Object(_)) => "[object Object]".to_owned(),
    }
}

/// JavaScript truthiness.
fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => false,
        Some(JsValue::Bool(flag)) => *flag,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
        Some(JsValue::Array(_) | JsValue::Object(_)) => true,
    }
}

/// `{ ...a, ...b }` for object values; other values spread no keys.
fn spread(into: &mut JsObject, value: Option<&JsValue>) {
    if let Some(JsValue::Object(object)) = value {
        for (key, item) in object.iter() {
            into.insert(key, item.clone());
        }
    }
}

/// `mergeToolCallDetail`.
fn merge_tool_detail(existing: Option<&JsValue>, incoming: Option<&JsValue>) -> JsValue {
    let is_unknown = |detail: Option<&JsValue>| {
        detail
            .and_then(|value| value.get("type"))
            .and_then(JsValue::as_str)
            == Some("unknown")
    };
    let chosen = if is_unknown(existing) && !is_unknown(incoming) {
        incoming
    } else if is_unknown(incoming) && !is_unknown(existing) {
        existing
    } else {
        incoming
    };
    chosen.cloned().unwrap_or(JsValue::Undefined)
}

/// `mergeToolCallItems`.
fn merge_tool_call_items(existing: &JsValue, incoming: &JsValue) -> JsValue {
    let detail = merge_tool_detail(existing.get("detail"), incoming.get("detail"));
    let metadata = if truthy(existing.get("metadata")) || truthy(incoming.get("metadata")) {
        let mut merged = JsObject::new();
        spread(&mut merged, existing.get("metadata"));
        spread(&mut merged, incoming.get("metadata"));
        JsValue::Object(merged)
    } else {
        JsValue::Undefined
    };
    let mut merged = JsObject::new();
    spread(&mut merged, Some(existing));
    spread(&mut merged, Some(incoming));
    merged.insert("detail", detail);
    merged.insert("metadata", metadata);
    match incoming.get("status").and_then(JsValue::as_str) {
        Some("failed") => {
            merged.insert(
                "error",
                incoming.get("error").cloned().unwrap_or(JsValue::Undefined),
            );
        }
        Some("completed" | "canceled") => merged.insert("error", JsValue::Null),
        _ => {
            if let Some(error) = incoming.get("error")
                && !matches!(error, JsValue::Undefined)
            {
                merged.insert("error", error.clone());
            }
        }
    }
    JsValue::Object(merged)
}

/// `appendSeqToRanges` applied over whole ranges (equivalent to per-seq appends).
fn merge_seq_ranges(existing: &[SeqRange], incoming: &[SeqRange]) -> Vec<SeqRange> {
    let mut merged = existing.to_vec();
    for range in incoming {
        match merged.last_mut() {
            Some(last) if range.start_seq <= last.end_seq + 1 => {
                last.end_seq = last.end_seq.max(range.end_seq);
            }
            _ => merged.push(*range),
        }
    }
    merged
}

/// `new Set([...a, ...b, kind])` in insertion order.
fn union_collapsed(
    left: &[CollapseKind],
    right: &[CollapseKind],
    kind: CollapseKind,
) -> Vec<CollapseKind> {
    let mut out = Vec::new();
    for value in left.iter().chain(right).chain(std::iter::once(&kind)) {
        if !out.contains(value) {
            out.push(*value);
        }
    }
    out
}

/// `makeCanonicalEntries` for one source row.
fn canonical_entry(row: &TimelineRow) -> ProjectedRow {
    ProjectedRow {
        item: row.item.clone(),
        turn_id: row.turn_id.clone().filter(|id| !id.is_empty()),
        provider_message_id: row.provider_message_id.clone().filter(|id| !id.is_empty()),
        timestamp: row.timestamp.clone(),
        seq_start: row.seq,
        seq_end: row.seq,
        source_seq_ranges: vec![SeqRange {
            start_seq: row.seq,
            end_seq: row.seq,
        }],
        collapsed: Vec::new(),
        seq: row.seq,
        provider_message_id_last: false,
    }
}

/// `mergeIdentityEntries`.
fn merge_identity_entries(existing: &ProjectedRow, entry: &ProjectedRow) -> Option<ProjectedRow> {
    let identity_metadata = |kind: CollapseKind| {
        let collapsed = if existing.collapsed.contains(&kind) {
            existing.collapsed.clone()
        } else {
            let mut next = existing.collapsed.clone();
            next.push(kind);
            next
        };
        (
            merge_seq_ranges(&existing.source_seq_ranges, &entry.source_seq_ranges),
            collapsed,
        )
    };
    match item_type(&entry.item) {
        Some("tool_call") => {
            if item_type(&existing.item) != Some("tool_call") || existing.turn_id != entry.turn_id {
                return None;
            }
            let (source_seq_ranges, collapsed) = identity_metadata(CollapseKind::ToolLifecycle);
            Some(ProjectedRow {
                item: merge_tool_call_items(&existing.item, &entry.item),
                timestamp: entry.timestamp.clone(),
                seq_end: existing.seq_end.max(entry.seq_end),
                source_seq_ranges,
                collapsed,
                ..existing.clone()
            })
        }
        Some("plugin") => {
            if item_type(&existing.item) != Some("plugin") {
                return None;
            }
            let (source_seq_ranges, collapsed) = identity_metadata(CollapseKind::Identity);
            Some(ProjectedRow {
                seq_start: existing.seq_start,
                source_seq_ranges,
                collapsed,
                ..entry.clone()
            })
        }
        _ => None,
    }
}

/// `mergeAssistantChunks` / `mergeReasoningChunks` for one adjacent pair.
fn merge_adjacent(
    previous: &ProjectedRow,
    entry: &ProjectedRow,
    assistant: bool,
) -> Option<ProjectedRow> {
    let kind = if assistant {
        "assistant_message"
    } else {
        "reasoning"
    };
    let mergeable = item_type(&previous.item) == Some(kind)
        && item_type(&entry.item) == Some(kind)
        && previous.seq_end + 1 == entry.seq_start
        && previous.turn_id == entry.turn_id;
    if !mergeable {
        return None;
    }
    let mut item = JsObject::new();
    item.insert("type", JsValue::String(kind.to_owned()));
    item.insert(
        "text",
        JsValue::String(format!(
            "{}{}",
            js_to_string(previous.item.get("text")),
            js_to_string(entry.item.get("text"))
        )),
    );
    let collapse = if assistant {
        let incoming_id = entry.item.get("messageId");
        if incoming_id.is_some_and(|id| !matches!(id, JsValue::Undefined))
            && previous.item.get("messageId") != incoming_id
        {
            return None;
        }
        if truthy(previous.item.get("messageId")) {
            item.insert(
                "messageId",
                previous
                    .item
                    .get("messageId")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            );
        }
        CollapseKind::AssistantMerge
    } else {
        CollapseKind::ReasoningMerge
    };
    Some(ProjectedRow {
        item: JsValue::Object(item),
        timestamp: entry.timestamp.clone(),
        seq_end: entry.seq_end,
        source_seq_ranges: merge_seq_ranges(&previous.source_seq_ranges, &entry.source_seq_ranges),
        collapsed: union_collapsed(&previous.collapsed, &entry.collapsed, collapse),
        ..previous.clone()
    })
}

fn merge_chunks(entries: Vec<ProjectedRow>, assistant: bool) -> Vec<ProjectedRow> {
    let mut output: Vec<ProjectedRow> = Vec::with_capacity(entries.len());
    for entry in entries {
        let merged = output
            .last()
            .and_then(|previous| merge_adjacent(previous, &entry, assistant));
        if let (Some(merged), Some(last)) = (merged, output.last_mut()) {
            *last = merged;
            continue;
        }
        output.push(entry);
    }
    output
}

/// `collapseByIdentity`.
fn collapse_by_identity(entries: Vec<ProjectedRow>) -> Vec<ProjectedRow> {
    let mut output: Vec<ProjectedRow> = Vec::with_capacity(entries.len());
    let mut index_by_identity: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let Some(identity) = item_identity(&entry.item) else {
            output.push(entry);
            continue;
        };
        let merged = index_by_identity.get(&identity).and_then(|index| {
            merge_identity_entries(&output[*index], &entry).map(|merged| (*index, merged))
        });
        if let Some((index, merged)) = merged {
            output[index] = merged;
        } else {
            index_by_identity.insert(identity, output.len());
            output.push(entry);
        }
    }
    output
}

/// `projectTimelineRows({ mode: "projected" })` over already projected rows.
#[must_use]
pub fn project_rows(rows: &[ProjectedRow]) -> Vec<ProjectedRow> {
    let collapsed = collapse_by_identity(rows.to_vec());
    merge_chunks(merge_chunks(collapsed, true), false)
}

/// `TimelineProjection`: owns projected rows; source rows are consumed.
#[derive(Debug, Clone, Default)]
pub struct TimelineProjection {
    rows: Vec<ProjectedRow>,
    identities: HashMap<String, usize>,
}

impl TimelineProjection {
    pub fn append(&mut self, row: &TimelineRow) {
        let entry = canonical_entry(row);
        let identity = item_identity(&row.item);
        if let Some(index) = identity
            .as_ref()
            .and_then(|key| self.identities.get(key))
            .copied()
            && let Some(merged) = merge_identity_entries(&self.rows[index], &entry)
        {
            self.rows[index] = ProjectedRow {
                seq: merged.seq_end,
                ..merged
            };
            return;
        }
        let adjacent = self.rows.last().and_then(|previous| {
            merge_adjacent(previous, &entry, true)
                .or_else(|| merge_adjacent(previous, &entry, false))
        });
        if let (Some(merged), Some(last)) = (adjacent, self.rows.last_mut()) {
            *last = ProjectedRow {
                seq: merged.seq_end,
                ..merged
            };
            return;
        }
        if let Some(identity) = identity {
            self.identities.insert(identity, self.rows.len());
        }
        self.rows.push(ProjectedRow {
            seq: entry.seq_end,
            ..entry
        });
    }

    #[must_use]
    pub fn rows(&self) -> &[ProjectedRow] {
        &self.rows
    }

    /// `enrichSubmittedUserMessage`.
    pub fn enrich_submitted_user_message(
        &mut self,
        client_message_id: &str,
        provider_message_id: &str,
    ) -> Option<ProjectedRow> {
        let row = self.rows.iter_mut().find(|row| {
            item_type(&row.item) == Some("user_message")
                && row.item.get("clientMessageId").and_then(JsValue::as_str)
                    == Some(client_message_id)
        })?;
        if row.provider_message_id.is_none() {
            row.provider_message_id_last = true;
        }
        row.provider_message_id = Some(provider_message_id.to_owned());
        Some(row.clone())
    }
}

/// `TimelineLimitDirection` / `AgentTimelineFetchDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchDirection {
    Tail,
    Before,
    After,
}

/// `ProjectedTimelinePageSelection`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSelection {
    pub entries: Vec<ProjectedRow>,
    pub start_seq: Option<i64>,
    pub end_seq: Option<i64>,
    pub has_older: bool,
    pub has_newer: bool,
}

/// Inclusive sequence bounds of the store window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqBounds {
    pub min_seq: i64,
    pub max_seq: i64,
}

fn first_source_seq_in_range(entry: &ProjectedRow, start_seq: i64, end_seq: i64) -> Option<i64> {
    entry.source_seq_ranges.iter().find_map(|range| {
        let first = range.start_seq.max(start_seq);
        (first <= range.end_seq.min(end_seq)).then_some(first)
    })
}

fn select_after(
    entries: &[ProjectedRow],
    start_seq: i64,
    max_seq: i64,
    limit: usize,
) -> (Vec<ProjectedRow>, Option<i64>) {
    let mut eligible: Vec<(usize, i64)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            first_source_seq_in_range(entry, start_seq, max_seq).map(|first| (index, first))
        })
        .collect();
    eligible.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    if limit > 0 {
        eligible.truncate(limit);
    }
    eligible.sort_by_key(|(index, _)| *index);
    let selected: Vec<ProjectedRow> = eligible
        .iter()
        .map(|(index, _)| entries[*index].clone())
        .collect();
    if selected.is_empty() {
        return (selected, None);
    }
    let mut ranges: Vec<SeqRange> = selected
        .iter()
        .flat_map(|entry| entry.source_seq_ranges.iter().copied())
        .collect();
    ranges.sort_by(|left, right| {
        left.start_seq
            .cmp(&right.start_seq)
            .then(left.end_seq.cmp(&right.end_seq))
    });
    let mut end_seq = start_seq - 1;
    for range in ranges {
        if range.end_seq <= end_seq {
            continue;
        }
        if range.start_seq > end_seq + 1 {
            break;
        }
        end_seq = range.end_seq.min(max_seq);
    }
    let end = (end_seq >= start_seq).then_some(end_seq);
    (selected, end)
}

/// `selectProjectedTimelinePage`, with `limit` already floored and clamped at 0.
#[must_use]
pub fn select_page(
    rows: &[ProjectedRow],
    bounds: SeqBounds,
    direction: FetchDirection,
    cursor_seq: Option<i64>,
    limit: usize,
) -> PageSelection {
    let all = project_rows(rows);
    let empty = |has_older: bool, has_newer: bool| PageSelection {
        entries: Vec::new(),
        start_seq: None,
        end_seq: None,
        has_older,
        has_newer,
    };
    if all.is_empty() {
        return match direction {
            FetchDirection::After => {
                let cursor = cursor_seq.unwrap_or(bounds.min_seq - 1);
                empty(cursor >= bounds.min_seq, cursor < bounds.max_seq)
            }
            FetchDirection::Before => {
                let cursor = cursor_seq.unwrap_or(bounds.max_seq + 1);
                empty(cursor > bounds.min_seq, cursor <= bounds.max_seq)
            }
            FetchDirection::Tail => empty(false, false),
        };
    }
    match direction {
        FetchDirection::Tail => {
            let mut start = if limit == 0 {
                0
            } else {
                all.len().saturating_sub(limit)
            };
            for index in (0..start).rev() {
                if all[index].seq_end >= all[start].seq_start {
                    start = index;
                }
            }
            let entries = all[start..].to_vec();
            PageSelection {
                start_seq: entries.first().map(|entry| entry.seq_start),
                end_seq: Some(bounds.max_seq),
                has_older: start > 0,
                has_newer: false,
                entries,
            }
        }
        FetchDirection::After => {
            let cursor = cursor_seq.unwrap_or(bounds.min_seq - 1);
            let start_seq = bounds.min_seq.max(cursor + 1);
            let (entries, end_seq) = select_after(&all, start_seq, bounds.max_seq, limit);
            PageSelection {
                entries,
                start_seq: end_seq.map(|_| start_seq),
                end_seq,
                has_older: start_seq > bounds.min_seq,
                has_newer: end_seq.is_some_and(|end| end < bounds.max_seq),
            }
        }
        FetchDirection::Before => {
            let cursor = cursor_seq.unwrap_or(bounds.max_seq + 1);
            let end_seq = bounds.max_seq.min(cursor - 1);
            if end_seq < bounds.min_seq {
                return empty(false, end_seq < bounds.max_seq);
            }
            let eligible: Vec<&ProjectedRow> = all
                .iter()
                .filter(|entry| entry.seq_start <= end_seq)
                .collect();
            let selected: Vec<ProjectedRow> = if limit == 0 || limit >= eligible.len() {
                eligible.iter().map(|entry| (*entry).clone()).collect()
            } else {
                eligible[eligible.len() - limit..]
                    .iter()
                    .map(|entry| (*entry).clone())
                    .collect()
            };
            PageSelection {
                start_seq: selected.first().map(|entry| entry.seq_start),
                end_seq: Some(end_seq),
                has_older: selected.len() < eligible.len(),
                has_newer: end_seq < bounds.max_seq,
                entries: selected,
            }
        }
    }
}

/// `AgentTimelineCursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineCursor {
    pub epoch: String,
    pub seq: i64,
}

/// `AgentTimelineWindow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineWindow {
    pub min_seq: i64,
    pub max_seq: i64,
    pub next_seq: i64,
}

/// `AgentTimelineFetchResult`.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the baseline result object field for field"
)]
pub struct TimelineFetch {
    pub epoch: String,
    pub direction: FetchDirection,
    pub reset: bool,
    pub stale_cursor: bool,
    pub gap: bool,
    pub window: TimelineWindow,
    pub has_older: bool,
    pub has_newer: bool,
    pub start_seq: Option<i64>,
    pub end_seq: Option<i64>,
    pub rows: Vec<ProjectedRow>,
}

#[derive(Debug, Clone)]
struct AgentTimeline {
    epoch: String,
    projection: TimelineProjection,
    min_seq: i64,
    next_seq: i64,
}

/// Default page size of `fetch` when the caller gives no limit.
pub const DEFAULT_TIMELINE_FETCH_LIMIT: usize = 200;

/// `InMemoryAgentTimelineStore`.
#[derive(Debug, Default)]
pub struct TimelineStore {
    states: HashMap<String, AgentTimeline>,
}

/// The error the baseline throws for an agent without a timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAgent(pub String);

impl std::fmt::Display for UnknownAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Unknown agent '{}'", self.0)
    }
}

impl std::error::Error for UnknownAgent {}

impl TimelineStore {
    #[must_use]
    pub fn has(&self, agent_id: &str) -> bool {
        self.states.contains_key(agent_id)
    }

    /// `initialize(agentId, { items, epoch, nextSeq, timestamp })`: seeds rows
    /// from items (one sequence number each) under a new or given epoch.
    pub fn initialize(
        &mut self,
        agent_id: &str,
        items: Vec<JsValue>,
        epoch: Option<String>,
        next_seq: Option<i64>,
        timestamp: Option<String>,
    ) {
        let timestamp = timestamp.unwrap_or_else(now_iso);
        let mut seq = next_seq.unwrap_or(1);
        let mut projection = TimelineProjection::default();
        let mut highest_next = next_seq.unwrap_or(1);
        for item in items {
            let row = TimelineRow {
                seq,
                timestamp: timestamp.clone(),
                item,
                turn_id: None,
                provider_message_id: None,
            };
            highest_next = highest_next.max(seq + 1);
            projection.append(&row);
            seq += 1;
        }
        let min_seq = projection.rows().first().map_or(0, |row| row.seq_start);
        self.states.insert(
            agent_id.to_owned(),
            AgentTimeline {
                epoch: epoch.unwrap_or_else(random_uuid),
                projection,
                min_seq,
                next_seq: highest_next,
            },
        );
    }

    pub fn delete(&mut self, agent_id: &str) {
        self.states.remove(agent_id);
    }

    fn state(&self, agent_id: &str) -> Result<&AgentTimeline, UnknownAgent> {
        self.states
            .get(agent_id)
            .ok_or_else(|| UnknownAgent(agent_id.to_owned()))
    }

    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn epoch(&self, agent_id: &str) -> Result<&str, UnknownAgent> {
        self.state(agent_id).map(|state| state.epoch.as_str())
    }

    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn rows(&self, agent_id: &str) -> Result<&[ProjectedRow], UnknownAgent> {
        self.state(agent_id).map(|state| state.projection.rows())
    }

    /// `append`: assigns the next sequence number and projects the row.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn append(
        &mut self,
        agent_id: &str,
        item: JsValue,
        timestamp: Option<String>,
        turn_id: Option<String>,
        provider_message_id: Option<String>,
    ) -> Result<TimelineRow, UnknownAgent> {
        let state = self
            .states
            .get_mut(agent_id)
            .ok_or_else(|| UnknownAgent(agent_id.to_owned()))?;
        let row = TimelineRow {
            seq: state.next_seq,
            timestamp: timestamp.unwrap_or_else(now_iso),
            item,
            turn_id: turn_id.filter(|id| !id.is_empty()),
            provider_message_id: provider_message_id.filter(|id| !id.is_empty()),
        };
        state.next_seq += 1;
        if state.min_seq == 0 {
            state.min_seq = row.seq;
        }
        state.projection.append(&row);
        Ok(row)
    }

    /// `fetch`: a projected page with cursor staleness and gap detection.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn fetch(
        &self,
        agent_id: &str,
        direction: FetchDirection,
        cursor: Option<&TimelineCursor>,
        limit: Option<usize>,
    ) -> Result<TimelineFetch, UnknownAgent> {
        let state = self.state(agent_id)?;
        let rows = state.projection.rows();
        let window = TimelineWindow {
            min_seq: state.min_seq,
            max_seq: state.next_seq - 1,
            next_seq: state.next_seq,
        };
        let stale_cursor = cursor.is_some_and(|cursor| cursor.epoch != state.epoch);
        let gap = !stale_cursor
            && direction == FetchDirection::After
            && cursor.is_some_and(|cursor| !rows.is_empty() && cursor.seq < state.min_seq - 1);
        let reset = stale_cursor || gap;
        let page = select_page(
            rows,
            SeqBounds {
                min_seq: window.min_seq,
                max_seq: window.max_seq,
            },
            if reset {
                FetchDirection::Tail
            } else {
                direction
            },
            cursor.map(|cursor| cursor.seq),
            limit.unwrap_or(DEFAULT_TIMELINE_FETCH_LIMIT),
        );
        Ok(TimelineFetch {
            epoch: state.epoch.clone(),
            direction,
            reset,
            stale_cursor,
            gap,
            window,
            has_older: page.has_older,
            has_newer: page.has_newer,
            start_seq: page.start_seq,
            end_seq: page.end_seq,
            rows: page.entries,
        })
    }

    /// `getLastItem`: the row whose `seqEnd` is the latest sequence number.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn last_item(&self, agent_id: &str) -> Result<Option<JsValue>, UnknownAgent> {
        let state = self.state(agent_id)?;
        Ok(state
            .projection
            .rows()
            .iter()
            .find(|row| row.seq_end == state.next_seq - 1)
            .map(|row| row.item.clone()))
    }

    /// `getLastAssistantMessage`.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn last_assistant_message(&self, agent_id: &str) -> Result<Option<String>, UnknownAgent> {
        let state = self.state(agent_id)?;
        Ok(state
            .projection
            .rows()
            .iter()
            .rev()
            .find(|row| item_type(&row.item) == Some("assistant_message"))
            .map(|row| js_to_string(row.item.get("text"))))
    }

    /// `getSubmittedUserMessage`.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn submitted_user_message(
        &self,
        agent_id: &str,
        client_message_id: &str,
    ) -> Result<Option<ProjectedRow>, UnknownAgent> {
        Ok(self
            .state(agent_id)?
            .projection
            .rows()
            .iter()
            .find(|row| {
                item_type(&row.item) == Some("user_message")
                    && row.item.get("clientMessageId").and_then(JsValue::as_str)
                        == Some(client_message_id)
            })
            .cloned())
    }

    /// `enrichSubmittedUserMessage`.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownAgent`] when the agent has no timeline.
    pub fn enrich_submitted_user_message(
        &mut self,
        agent_id: &str,
        client_message_id: &str,
        provider_message_id: &str,
    ) -> Result<Option<ProjectedRow>, UnknownAgent> {
        let state = self
            .states
            .get_mut(agent_id)
            .ok_or_else(|| UnknownAgent(agent_id.to_owned()))?;
        Ok(state
            .projection
            .enrich_submitted_user_message(client_message_id, provider_message_id))
    }
}
