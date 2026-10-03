//! Codex thread history replay.
//!
//! Port of `loadCodexThreadHistoryTimeline`, `CodexThreadReadResponseSchema`,
//! `readCodexHistoryTimestamp`, `readCodexTurnHistoryTimestamp`, and
//! `normalizeProviderReplayTimestamp` from pinned Paseo. A resumed session
//! rebuilds its timeline from `thread/read` (timelines are in memory only).
//!
//! Slice scope: sub-agent history (`subAgentActivity` and spawned
//! `collabAgentToolCall` children) and items whose mapper is not ported are
//! reported as unported instead of being replayed differently.

use serde_json::{Map, Value};
use spocky_contracts::js::date_parse;
use spocky_contracts::text::js_trim;

use crate::items::{ThreadItemMapping, item_type, thread_item_to_timeline};

/// One replayed timeline entry (`PersistedTimelineEntry`).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub item: Value,
    pub timestamp: Option<String>,
    pub provider_turn_id: Option<String>,
}

/// The replayed timeline plus anything Paseo would replay through an
/// unported path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HistoryProjection {
    pub timeline: Vec<HistoryEntry>,
    pub unported: Vec<String>,
}

/// One turn object and its items.
type Turn<'a> = (&'a Map<String, Value>, Vec<&'a Value>);

/// Validated `CodexThreadReadResponseSchema` turns: each turn object and
/// its `items` (default `[]`).
fn parse_turns(response: &Value) -> Result<Vec<Turn<'_>>, String> {
    let invalid = || "Invalid Codex thread/read response".to_owned();
    let record = response.as_object().ok_or_else(invalid)?;
    let thread = match record.get("thread") {
        None => return Ok(Vec::new()),
        Some(Value::Object(thread)) => thread,
        Some(_) => return Err(invalid()),
    };
    let turns = match thread.get("turns") {
        None => return Ok(Vec::new()),
        Some(Value::Array(turns)) => turns,
        Some(_) => return Err(invalid()),
    };
    let mut parsed = Vec::with_capacity(turns.len());
    for turn in turns {
        let turn = turn.as_object().ok_or_else(invalid)?;
        let items = match turn.get("items") {
            None => Vec::new(),
            Some(Value::Array(items)) => items.iter().collect(),
            Some(_) => return Err(invalid()),
        };
        parsed.push((turn, items));
    }
    Ok(parsed)
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `new Date(ms).toISOString()`, or `None` for an invalid time value.
#[must_use]
pub fn iso_string_from_millis(millis: f64) -> Option<String> {
    if !millis.is_finite() || millis.abs() > 8.64e15 {
        return None;
    }
    // TimeClip truncates toward zero; the range check above keeps it exact.
    let text = format!("{:.0}", millis.trunc());
    let millis: i64 = text.parse().ok()?;
    let days = millis.div_euclid(86_400_000);
    let in_day = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let year_text = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", -year)
    } else {
        format!("+{year:06}")
    };
    Some(format!(
        "{year_text}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000
    ))
}

/// `normalizeProviderReplayTimestamp(value)`: a string is kept (trimmed) when
/// `Date.parse` reads it (`spocky_contracts::js::date_parse`, V8's parser,
/// ECMAScript format and legacy forms alike); a finite number is read as
/// seconds or milliseconds and written as an ISO string.
#[must_use]
pub fn normalize_replay_timestamp(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = js_trim(text);
            (!trimmed.is_empty() && date_parse(trimmed).is_some()).then(|| trimmed.to_owned())
        }
        Some(Value::Number(number)) => {
            let value = number.as_f64().unwrap_or(f64::NAN);
            if !value.is_finite() {
                return None;
            }
            let millis = if value > 1_000_000_000_000.0 {
                value
            } else {
                value * 1000.0
            };
            iso_string_from_millis(millis)
        }
        _ => None,
    }
}

fn first_timestamp(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| normalize_replay_timestamp(record.get(*key)))
}

/// `loadCodexThreadHistoryTimeline` over a `thread/read` response.
///
/// # Errors
/// Returns an error when the response fails Paseo's response schema.
pub fn project_thread_history(response: &Value) -> Result<HistoryProjection, String> {
    let mut projection = HistoryProjection::default();
    for (turn, items) in parse_turns(response)? {
        for item in items {
            let record = item.as_object();
            if let Some(record) = record
                && let Some(kind) = item_type(record)
                && (kind == "subAgentActivity" || kind == "collabAgentToolCall")
            {
                projection
                    .unported
                    .push(format!("history thread item {kind}"));
                continue;
            }
            let timeline_item = match thread_item_to_timeline(item, true) {
                ThreadItemMapping::Item(item) => item,
                ThreadItemMapping::Skip => continue,
                ThreadItemMapping::Unported { item_type } => {
                    projection
                        .unported
                        .push(format!("history thread item {item_type}"));
                    continue;
                }
            };
            let is_user = timeline_item["type"] == "user_message";
            let item_timestamp = record.and_then(|record| {
                first_timestamp(record, &["timestamp", "createdAt", "created_at"])
            });
            let timestamp = item_timestamp.or_else(|| {
                let started = first_timestamp(turn, &["startedAt", "started_at"]);
                let completed = first_timestamp(turn, &["completedAt", "completed_at"]);
                if is_user {
                    started.or(completed)
                } else {
                    completed.or(started)
                }
            });
            let provider_turn_id = if is_user {
                turn.get("id").and_then(Value::as_str).map(str::to_owned)
            } else {
                None
            };
            projection.timeline.push(HistoryEntry {
                item: timeline_item,
                timestamp,
                provider_turn_id,
            });
        }
    }
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numeric_timestamps_become_iso_strings() {
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(1_790_866_643))),
            Some("2026-10-01T14:57:23.000Z".to_owned())
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(1_790_866_643_125_u64))),
            Some("2026-10-01T14:57:23.125Z".to_owned())
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(0))),
            Some("1970-01-01T00:00:00.000Z".to_owned())
        );
        assert_eq!(normalize_replay_timestamp(Some(&json!(9e15))), None);
        assert_eq!(
            iso_string_from_millis(-1.0),
            Some("1969-12-31T23:59:59.999Z".to_owned())
        );
        assert_eq!(
            iso_string_from_millis(253_402_300_800_000.0),
            Some("+010000-01-01T00:00:00.000Z".to_owned())
        );
    }

    /// `Number.isNaN(Date.parse(text.trim()))` negated, printed by node
    /// v22.20.0 (the pinned runtime) for each text:
    /// `node -e 'for (const c of cases) console.log(!Number.isNaN(Date.parse(c.trim())))'`.
    const NODE_DATE_PARSE: &[(&str, bool)] = &[
        ("Oct 1 2026", true),
        ("October 1, 2026 14:57:23 UTC", true),
        ("Thu, 01 Oct 2026 14:57:23 GMT", true),
        ("1/2/2026", true),
        ("10/01/2026 2:57 PM", true),
        ("2026-10-01 14:57:23", true),
        ("2026-10-01 14:57:23Z", true),
        ("2026-10-01T14:57:23", true),
        ("2026/10/01", true),
        ("2026-02-30", true),
        ("2026-02-31", true),
        ("2026-04-31", true),
        ("2026-02-30T00:00:00Z", true),
        ("2026-13-01", false),
        ("2026-00-10", false),
        ("2026-10-00", false),
        ("20261001", false),
        ("2026-10-01T25:00:00Z", false),
        ("2026-10-01T24:00:00Z", true),
        ("2026-10-01T24:00:01Z", false),
        ("2026", true),
        ("12", true),
        ("1", true),
        ("0", true),
        ("garbage", false),
        ("not a date", false),
        ("2026-10-01T14:57:23.123456789Z", true),
        ("2026-10-01T14:57Z", true),
        ("+275760-09-13T00:00:00.000Z", true),
        ("+275760-09-13T00:00:00.001Z", false),
        ("-000000-01-01T00:00:00Z", false),
        ("-000004-02-29", true),
        ("-000003-02-29", true),
        ("2024-02-29T00:00:00.5+01:00", true),
        ("Tue Oct 01 2026 14:57:23 GMT+0100 (BST)", true),
        ("2026-10-01T14:57:23+0100", true),
        ("2026-10-01T14:57:23 +01:00", false),
        ("Sep 31 2026", true),
        ("Feb 30 2026", true),
        ("31 Sep 2026", true),
        ("Oct 2026", true),
        ("2026 Oct 1", true),
        ("1 Oct", true),
        ("Oct 1", true),
        ("T", false),
        ("2026-10-01T", false),
        ("2026-10-01Tz", false),
        ("12:34", false),
        ("12:34:56", false),
        ("1e3", false),
        ("1,2,3", true),
        ("(2026)", false),
        ("Z", false),
        ("2026-10-01T14:57:23Z\0junk", true),
        (" 2026-10-01T14:57:23Z ", true),
        (" 2026-02-30 ", true),
        ("", false),
    ];

    #[test]
    fn string_timestamps_follow_date_parse_and_are_kept_trimmed() {
        for &(text, parses) in NODE_DATE_PARSE {
            let kept = normalize_replay_timestamp(Some(&json!(text)));
            let expected = parses.then(|| js_trim(text).to_owned());
            assert_eq!(kept, expected, "{text:?}");
        }
        assert_eq!(normalize_replay_timestamp(Some(&json!(true))), None);
        assert_eq!(normalize_replay_timestamp(None), None);
    }

    #[test]
    fn real_codex_0_159_history_projects_with_turn_timestamps() {
        let projection = project_thread_history(&json!({"thread": {"id": "t", "turns": [{
            "id": "turn-1",
            "items": [
                {"type": "userMessage", "id": "u1", "clientId": null,
                 "content": [{"type": "text", "text": "Say hello", "text_elements": []}]},
                {"type": "agentMessage", "id": "msg_1", "text": "Hello from stub.",
                 "phase": null, "memoryCitation": null, "delivery": null, "questions": null}
            ],
            "itemsView": "full", "status": "completed", "error": null,
            "startedAt": 1_790_866_642, "completedAt": 1_790_866_643, "durationMs": 143
        }]}}))
        .unwrap();
        assert!(projection.unported.is_empty());
        assert_eq!(
            projection.timeline,
            vec![
                HistoryEntry {
                    item: json!({"type": "user_message", "text": "Say hello", "messageId": "u1"}),
                    timestamp: Some("2026-10-01T14:57:22.000Z".to_owned()),
                    provider_turn_id: Some("turn-1".to_owned()),
                },
                HistoryEntry {
                    item: json!({"type": "assistant_message", "text": "Hello from stub.", "messageId": "msg_1"}),
                    timestamp: Some("2026-10-01T14:57:23.000Z".to_owned()),
                    provider_turn_id: None,
                },
            ]
        );
    }

    #[test]
    fn response_schema_defaults_and_rejections() {
        assert_eq!(project_thread_history(&json!({})).unwrap().timeline, vec![]);
        assert_eq!(
            project_thread_history(&json!({"thread": {"turns": [{}]}}))
                .unwrap()
                .timeline,
            vec![]
        );
        assert!(project_thread_history(&json!({"thread": {"turns": [1]}})).is_err());
        assert!(project_thread_history(&json!([])).is_err());
    }

    #[test]
    fn item_timestamps_win_over_turn_timestamps() {
        let projection = project_thread_history(&json!({"thread": {"turns": [{
            "items": [{"type": "agentMessage", "id": "m", "text": "x", "createdAt": "2026-01-02T03:04:05.000Z"}],
            "startedAt": 1, "completedAt": 2
        }]}}))
        .unwrap();
        assert_eq!(
            projection.timeline[0].timestamp.as_deref(),
            Some("2026-01-02T03:04:05.000Z")
        );
    }
}
