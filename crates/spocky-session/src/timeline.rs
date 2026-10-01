//! In-memory agent timeline from pinned Paseo `agent/agent-timeline-store.ts`
//! and `agent/timeline-projection.ts`.
//!
//! Items stay JavaScript values ([`JsValue`]) so merged items keep the key
//! order the baseline's object spreads produce. Rows carry a sequence number
//! per agent epoch; the projection merges tool-call lifecycles and plugin
//! items by identity and adjacent assistant and reasoning chunks, as rows
//! arrive and again when a page is selected.

use std::borrow::Cow;
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

/// A JavaScript `TypeError` the baseline throws, with V8's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsTypeError(pub String);

impl std::fmt::Display for JsTypeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JsTypeError {}

/// `detail.type`: reading a property of `undefined` or `null` throws.
fn detail_type(detail: Option<&JsValue>) -> Result<Option<&str>, JsTypeError> {
    let receiver = match detail {
        None | Some(JsValue::Undefined) => "undefined",
        Some(JsValue::Null) => "null",
        Some(value) => return Ok(value.get("type").and_then(JsValue::as_str)),
    };
    Err(JsTypeError(format!(
        "Cannot read properties of {receiver} (reading 'type')"
    )))
}

/// `mergeToolCallDetail`. Parity note: the baseline reads `existing.type`
/// and then `incoming.type`, so a missing or `null` detail on either side
/// throws, existing first.
fn merge_tool_detail(
    existing: Option<&JsValue>,
    incoming: Option<&JsValue>,
) -> Result<JsValue, JsTypeError> {
    let existing_unknown = detail_type(existing)? == Some("unknown");
    let incoming_unknown = detail_type(incoming)? == Some("unknown");
    let chosen = if !existing_unknown && incoming_unknown {
        existing
    } else {
        incoming
    };
    Ok(chosen.cloned().unwrap_or(JsValue::Undefined))
}

/// `mergeToolCallItems`.
fn merge_tool_call_items(existing: &JsValue, incoming: &JsValue) -> Result<JsValue, JsTypeError> {
    let detail = merge_tool_detail(existing.get("detail"), incoming.get("detail"))?;
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
    Ok(JsValue::Object(merged))
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
fn merge_identity_entries(
    existing: &ProjectedRow,
    entry: &ProjectedRow,
) -> Result<Option<ProjectedRow>, JsTypeError> {
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
                return Ok(None);
            }
            let item = merge_tool_call_items(&existing.item, &entry.item)?;
            let (source_seq_ranges, collapsed) = identity_metadata(CollapseKind::ToolLifecycle);
            Ok(Some(ProjectedRow {
                item,
                timestamp: entry.timestamp.clone(),
                seq_end: existing.seq_end.max(entry.seq_end),
                source_seq_ranges,
                collapsed,
                ..existing.clone()
            }))
        }
        Some("plugin") => {
            if item_type(&existing.item) != Some("plugin") {
                return Ok(None);
            }
            let (source_seq_ranges, collapsed) = identity_metadata(CollapseKind::Identity);
            Ok(Some(ProjectedRow {
                seq_start: existing.seq_start,
                source_seq_ranges,
                collapsed,
                ..entry.clone()
            }))
        }
        _ => Ok(None),
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

fn merge_chunks(
    entries: Vec<Cow<'_, ProjectedRow>>,
    assistant: bool,
) -> Vec<Cow<'_, ProjectedRow>> {
    let mut output: Vec<Cow<'_, ProjectedRow>> = Vec::with_capacity(entries.len());
    for entry in entries {
        let merged = output
            .last()
            .and_then(|previous| merge_adjacent(previous, &entry, assistant));
        if let (Some(merged), Some(last)) = (merged, output.last_mut()) {
            *last = Cow::Owned(merged);
            continue;
        }
        output.push(entry);
    }
    output
}

/// `collapseByIdentity`.
fn collapse_by_identity(
    entries: Vec<Cow<'_, ProjectedRow>>,
) -> Result<Vec<Cow<'_, ProjectedRow>>, JsTypeError> {
    let mut output: Vec<Cow<'_, ProjectedRow>> = Vec::with_capacity(entries.len());
    let mut index_by_identity: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let Some(identity) = item_identity(&entry.item) else {
            output.push(entry);
            continue;
        };
        let merged = match index_by_identity.get(&identity) {
            Some(&index) => {
                merge_identity_entries(&output[index], &entry)?.map(|merged| (index, merged))
            }
            None => None,
        };
        if let Some((index, merged)) = merged {
            output[index] = Cow::Owned(merged);
        } else {
            index_by_identity.insert(identity, output.len());
            output.push(entry);
        }
    }
    Ok(output)
}

/// `projectTimelineRows({ mode: "projected" })` over already projected rows.
/// Rows no merge touches are borrowed, not copied.
///
/// # Errors
///
/// Returns the baseline's [`JsTypeError`] when a tool-call merge reads a
/// missing detail.
pub fn project_rows(rows: &[ProjectedRow]) -> Result<Vec<Cow<'_, ProjectedRow>>, JsTypeError> {
    let collapsed = collapse_by_identity(rows.iter().map(Cow::Borrowed).collect())?;
    Ok(merge_chunks(merge_chunks(collapsed, true), false))
}

/// `TimelineProjection`: owns projected rows; source rows are consumed.
#[derive(Debug, Clone, Default)]
pub struct TimelineProjection {
    rows: Vec<ProjectedRow>,
    identities: HashMap<String, usize>,
}

impl TimelineProjection {
    /// # Errors
    ///
    /// Returns the baseline's [`JsTypeError`] when a tool-call merge reads a
    /// missing detail; the projection is then unchanged.
    pub fn append(&mut self, row: &TimelineRow) -> Result<(), JsTypeError> {
        self.append_entry(canonical_entry(row))
    }

    /// `append` of a row that is already projected (`"seqStart" in row`):
    /// the row is taken as it is.
    ///
    /// # Errors
    ///
    /// As [`Self::append`].
    pub fn append_entry(&mut self, entry: ProjectedRow) -> Result<(), JsTypeError> {
        let identity = item_identity(&entry.item);
        let index = identity
            .as_ref()
            .and_then(|key| self.identities.get(key))
            .copied();
        if let Some(index) = index
            && let Some(merged) = merge_identity_entries(&self.rows[index], &entry)?
        {
            self.rows[index] = ProjectedRow {
                seq: merged.seq_end,
                ..merged
            };
            return Ok(());
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
            return Ok(());
        }
        if let Some(identity) = identity {
            self.identities.insert(identity, self.rows.len());
        }
        self.rows.push(ProjectedRow {
            seq: entry.seq_end,
            ..entry
        });
        Ok(())
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
    entries: &[Cow<'_, ProjectedRow>],
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
        .map(|(index, _)| ProjectedRow::clone(&entries[*index]))
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
///
/// # Errors
///
/// Returns the [`JsTypeError`] of [`project_rows`].
pub fn select_page(
    rows: &[ProjectedRow],
    bounds: SeqBounds,
    direction: FetchDirection,
    cursor_seq: Option<i64>,
    limit: usize,
) -> Result<PageSelection, JsTypeError> {
    let all = project_rows(rows)?;
    Ok(select_projected(&all, bounds, direction, cursor_seq, limit))
}

fn select_projected(
    all: &[Cow<'_, ProjectedRow>],
    bounds: SeqBounds,
    direction: FetchDirection,
    cursor_seq: Option<i64>,
    limit: usize,
) -> PageSelection {
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
            let entries: Vec<ProjectedRow> = all[start..]
                .iter()
                .map(|entry| ProjectedRow::clone(entry))
                .collect();
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
            // A client cursor may be any integer; JavaScript numbers do not
            // overflow, so the arithmetic saturates.
            let start_seq = bounds.min_seq.max(cursor.saturating_add(1));
            let (entries, end_seq) = select_after(all, start_seq, bounds.max_seq, limit);
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
            let end_seq = bounds.max_seq.min(cursor.saturating_sub(1));
            if end_seq < bounds.min_seq {
                return empty(false, end_seq < bounds.max_seq);
            }
            let eligible: Vec<&Cow<'_, ProjectedRow>> = all
                .iter()
                .filter(|entry| entry.seq_start <= end_seq)
                .collect();
            let selected: Vec<ProjectedRow> = if limit == 0 || limit >= eligible.len() {
                eligible
                    .iter()
                    .map(|entry| ProjectedRow::clone(entry))
                    .collect()
            } else {
                eligible[eligible.len() - limit..]
                    .iter()
                    .map(|entry| ProjectedRow::clone(entry))
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

/// A row `initialize` seeds: a source row or an already projected one.
#[derive(Debug, Clone, PartialEq)]
pub enum SeedRow {
    Source(TimelineRow),
    Projected(ProjectedRow),
}

impl SeedRow {
    const fn seq(&self) -> i64 {
        match self {
            Self::Source(row) => row.seq,
            Self::Projected(row) => row.seq,
        }
    }
}

/// `SeedAgentTimelineOptions`. Non-empty `rows` win over `items`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineSeed {
    pub items: Vec<JsValue>,
    pub rows: Vec<SeedRow>,
    pub epoch: Option<String>,
    pub next_seq: Option<i64>,
    pub timestamp: Option<String>,
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

/// What `append` and `fetch` throw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineError {
    UnknownAgent(UnknownAgent),
    Type(JsTypeError),
}

impl std::fmt::Display for TimelineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAgent(error) => error.fmt(formatter),
            Self::Type(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TimelineError {}

impl From<UnknownAgent> for TimelineError {
    fn from(error: UnknownAgent) -> Self {
        Self::UnknownAgent(error)
    }
}

impl From<JsTypeError> for TimelineError {
    fn from(error: JsTypeError) -> Self {
        Self::Type(error)
    }
}

impl TimelineStore {
    #[must_use]
    pub fn has(&self, agent_id: &str) -> bool {
        self.states.contains_key(agent_id)
    }

    /// `initialize(agentId, { items, epoch, nextSeq, timestamp })`: seeds rows
    /// from items (one sequence number each) under a new or given epoch.
    ///
    /// # Errors
    ///
    /// As [`Self::initialize_with`].
    pub fn initialize(
        &mut self,
        agent_id: &str,
        items: Vec<JsValue>,
        epoch: Option<String>,
        next_seq: Option<i64>,
        timestamp: Option<String>,
    ) -> Result<(), JsTypeError> {
        self.initialize_with(
            agent_id,
            TimelineSeed {
                items,
                rows: Vec::new(),
                epoch,
                next_seq,
                timestamp,
            },
        )
    }

    /// `initialize(agentId, options)`: seeds the given rows as they are, or
    /// else one row per item from `next_seq` (default 1) at `timestamp`.
    /// `nextSeq` becomes the larger of `next_seq` and every row's `seq + 1`.
    ///
    /// # Errors
    ///
    /// Returns the baseline's [`JsTypeError`] when seeding merges a tool call
    /// with a missing detail; the agent's previous timeline, if any, stays.
    pub fn initialize_with(
        &mut self,
        agent_id: &str,
        seed: TimelineSeed,
    ) -> Result<(), JsTypeError> {
        let start_seq = seed.next_seq.unwrap_or(1);
        let rows = if seed.rows.is_empty() {
            let timestamp = seed.timestamp.unwrap_or_else(now_iso);
            (start_seq..)
                .zip(seed.items)
                .map(|(seq, item)| {
                    SeedRow::Source(TimelineRow {
                        seq,
                        timestamp: timestamp.clone(),
                        item,
                        turn_id: None,
                        provider_message_id: None,
                    })
                })
                .collect()
        } else {
            seed.rows
        };
        let next_seq = rows
            .iter()
            .fold(start_seq, |next, row| next.max(row.seq().saturating_add(1)));
        let mut projection = TimelineProjection::default();
        for row in rows {
            match row {
                SeedRow::Source(row) => projection.append(&row)?,
                SeedRow::Projected(row) => projection.append_entry(row)?,
            }
        }
        let min_seq = projection.rows().first().map_or(0, |row| row.seq_start);
        self.states.insert(
            agent_id.to_owned(),
            AgentTimeline {
                epoch: seed.epoch.unwrap_or_else(random_uuid),
                projection,
                min_seq,
                next_seq,
            },
        );
        Ok(())
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
    /// Returns [`TimelineError::UnknownAgent`] when the agent has no
    /// timeline, and [`TimelineError::Type`] when the row merges into a tool
    /// call with a missing detail. As in the baseline, `nextSeq` (and an
    /// unset `minSeq`) has then already advanced and the rows are unchanged.
    pub fn append(
        &mut self,
        agent_id: &str,
        item: JsValue,
        timestamp: Option<String>,
        turn_id: Option<String>,
        provider_message_id: Option<String>,
    ) -> Result<TimelineRow, TimelineError> {
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
        state.projection.append(&row)?;
        Ok(row)
    }

    /// `fetch`: a projected page with cursor staleness and gap detection.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineError::UnknownAgent`] when the agent has no
    /// timeline, and [`TimelineError::Type`] from [`select_page`].
    pub fn fetch(
        &self,
        agent_id: &str,
        direction: FetchDirection,
        cursor: Option<&TimelineCursor>,
        limit: Option<usize>,
    ) -> Result<TimelineFetch, TimelineError> {
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
        )?;
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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{FetchDirection, TimelineCursor, TimelineStore, project_rows};
    use spocky_store::js_value::parse;

    #[test]
    fn page_projection_borrows_rows_no_merge_touches() {
        let mut store = TimelineStore::default();
        let items = [
            r#"{"type":"user_message","text":"q"}"#,
            r#"{"type":"assistant_message","text":"a"}"#,
        ];
        let items = items
            .iter()
            .map(|item| parse(item).expect("item"))
            .collect();
        store
            .initialize("a", items, Some("E".to_owned()), None, Some("T".to_owned()))
            .expect("seed");
        let rows = store.rows("a").expect("timeline");
        let projected = project_rows(rows).expect("projection");
        assert_eq!(projected.len(), 2);
        assert!(projected.iter().all(|row| matches!(row, Cow::Borrowed(_))));
    }

    #[test]
    fn extreme_cursors_select_nothing_without_overflow() {
        let mut store = TimelineStore::default();
        let item = parse(r#"{"type":"assistant_message","text":"a"}"#).expect("item");
        store
            .initialize(
                "a",
                vec![item],
                Some("E".to_owned()),
                None,
                Some("T".to_owned()),
            )
            .expect("seed");
        let fetch = |direction, seq| {
            let cursor = TimelineCursor {
                epoch: "E".to_owned(),
                seq,
            };
            store
                .fetch("a", direction, Some(&cursor), None)
                .expect("timeline")
        };
        // node: fetch("a", { direction: "after", cursor: { epoch: "E", seq: 2 ** 63 } })
        let after = fetch(FetchDirection::After, i64::MAX);
        assert!(after.rows.is_empty() && after.has_older && !after.has_newer);
        assert_eq!((after.start_seq, after.end_seq), (None, None));
        let before = fetch(FetchDirection::Before, i64::MIN);
        assert!(before.rows.is_empty() && !before.has_older && before.has_newer);
    }
}
