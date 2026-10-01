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

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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

/// Parses `digits` exactly `count` ASCII digits long.
fn fixed_digits(text: &str, count: usize) -> Option<i64> {
    (text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// ECMAScript Date Time String Format validity, the form `Date.parse` is
/// specified to accept. `None` means not this format.
fn iso_date_time_is_valid(text: &str) -> Option<bool> {
    let (date, time) = match text.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let (year, rest) = if let Some(rest) = date.strip_prefix(['+', '-']) {
        let year = fixed_digits(rest.get(..6)?, 6)?;
        if date.starts_with('-') && year == 0 {
            return Some(false);
        }
        (if date.starts_with('-') { -year } else { year }, &rest[6..])
    } else {
        (fixed_digits(date.get(..4)?, 4)?, &date[4..])
    };
    let mut parts = rest.split('-').skip(1);
    if !rest.is_empty() && !rest.starts_with('-') {
        return None;
    }
    let month = parts.next().map(|month| fixed_digits(month, 2));
    let day = parts.next().map(|day| fixed_digits(day, 2));
    if parts.next().is_some() {
        return None;
    }
    let month = match month {
        None => 1,
        Some(month) => month?,
    };
    let day = match day {
        None => 1,
        Some(day) => day?,
    };
    if !(1..=12).contains(&month) {
        return Some(false);
    }
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let days_in_month = days_from_civil(next_year, next_month, 1) - days_from_civil(year, month, 1);
    if day < 1 || day > days_in_month {
        return Some(false);
    }
    let Some(time) = time else {
        return Some(true);
    };
    let (clock, zone) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, None)
    } else if let Some(index) = time.rfind(['+', '-']) {
        (&time[..index], Some(&time[index..]))
    } else {
        (time, None)
    };
    if let Some(zone) = zone {
        let offset = &zone[1..];
        let (hours, minutes) = offset.split_once(':')?;
        let (hours, minutes) = (fixed_digits(hours, 2)?, fixed_digits(minutes, 2)?);
        if hours > 23 || minutes > 59 {
            return Some(false);
        }
    }
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (clock, None),
    };
    if let Some(fraction) = fraction
        && (fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let fields: Vec<&str> = clock.split(':').collect();
    if fields.len() < 2 || fields.len() > 3 || (fraction.is_some() && fields.len() != 3) {
        return None;
    }
    let values: Option<Vec<i64>> = fields.iter().map(|field| fixed_digits(field, 2)).collect();
    let values = values?;
    let hours = values[0];
    let minutes = values[1];
    let seconds = values.get(2).copied().unwrap_or(0);
    let is_midnight_24 = hours == 24
        && minutes == 0
        && seconds == 0
        && fraction.is_none_or(|fraction| fraction.bytes().all(|byte| byte == b'0'));
    Some((hours < 24 || is_midnight_24) && minutes < 60 && seconds < 60)
}

/// `normalizeProviderReplayTimestamp(value)`: `Ok(None)` for no timestamp,
/// `Err` for a string outside the ECMAScript format, whose `Date.parse`
/// result depends on V8's legacy parser.
///
/// # Errors
/// Returns the unported kind for a non-ISO string; the log keys by kind.
pub fn normalize_replay_timestamp(value: Option<&Value>) -> Result<Option<String>, String> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = js_trim(text);
            if trimmed.is_empty() {
                return Ok(None);
            }
            match iso_date_time_is_valid(trimmed) {
                Some(true) => Ok(Some(trimmed.to_owned())),
                Some(false) => Ok(None),
                None => Err("non-ISO history timestamp".to_owned()),
            }
        }
        Some(Value::Number(number)) => {
            let value = number.as_f64().unwrap_or(f64::NAN);
            if !value.is_finite() {
                return Ok(None);
            }
            let millis = if value > 1_000_000_000_000.0 {
                value
            } else {
                value * 1000.0
            };
            Ok(iso_string_from_millis(millis))
        }
        _ => Ok(None),
    }
}

fn first_timestamp(
    record: &Map<String, Value>,
    keys: &[&str],
    unported: &mut Vec<String>,
) -> Option<String> {
    for key in keys {
        match normalize_replay_timestamp(record.get(*key)) {
            Ok(Some(timestamp)) => return Some(timestamp),
            Ok(None) => {}
            Err(what) => {
                unported.push(what);
                return None;
            }
        }
    }
    None
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
                first_timestamp(
                    record,
                    &["timestamp", "createdAt", "created_at"],
                    &mut projection.unported,
                )
            });
            let timestamp = item_timestamp.or_else(|| {
                let started =
                    first_timestamp(turn, &["startedAt", "started_at"], &mut projection.unported);
                let completed = first_timestamp(
                    turn,
                    &["completedAt", "completed_at"],
                    &mut projection.unported,
                );
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
            Ok(Some("2026-10-01T14:57:23.000Z".to_owned()))
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(1_790_866_643_125_u64))),
            Ok(Some("2026-10-01T14:57:23.125Z".to_owned()))
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(0))),
            Ok(Some("1970-01-01T00:00:00.000Z".to_owned()))
        );
        assert_eq!(normalize_replay_timestamp(Some(&json!(9e15))), Ok(None));
        assert_eq!(
            iso_string_from_millis(-1.0),
            Some("1969-12-31T23:59:59.999Z".to_owned())
        );
        assert_eq!(
            iso_string_from_millis(253_402_300_800_000.0),
            Some("+010000-01-01T00:00:00.000Z".to_owned())
        );
    }

    #[test]
    fn iso_strings_are_kept_trimmed_and_invalid_ones_dropped() {
        assert_eq!(
            normalize_replay_timestamp(Some(&json!(" 2026-10-01T14:57:23Z "))),
            Ok(Some("2026-10-01T14:57:23Z".to_owned()))
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!("2026-02-30"))),
            Ok(None)
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!("-000004-02-29"))),
            Ok(Some("-000004-02-29".to_owned())),
            "year -4 is a leap year"
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!("-000003-02-29"))),
            Ok(None)
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!("Oct 1 2026"))),
            Err("non-ISO history timestamp".to_owned())
        );
        assert_eq!(
            normalize_replay_timestamp(Some(&json!("2024-02-29T00:00:00.5+01:00"))),
            Ok(Some("2024-02-29T00:00:00.5+01:00".to_owned()))
        );
        assert_eq!(normalize_replay_timestamp(Some(&json!(""))), Ok(None));
        assert!(normalize_replay_timestamp(Some(&json!("Oct 1 2026"))).is_err());
        assert_eq!(normalize_replay_timestamp(Some(&json!(true))), Ok(None));
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
