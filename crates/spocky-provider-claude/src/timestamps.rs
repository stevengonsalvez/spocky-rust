//! `provider-history-timestamps.ts` and the `Date` operations the replay
//! sources use: `Date.parse`, `new Date(value).toISOString()`.
//!
//! `Date.parse` is [`crate::date_parse::date_parse`]: V8's ECMAScript and
//! legacy parsers, with offset-less date-times in the host's local zone.

use spocky_contracts::text::js_trim;

pub use crate::date_parse::date_parse;

/// ECMAScript time values span +/-8.64e15 ms.
const MAX_TIME_MILLIS: f64 = 8_640_000_000_000_000.0;

/// `new Date(millis).toISOString()` for a time value, or `None` for an
/// invalid date (`toISOString` throws; callers check `getTime()` first).
#[must_use]
pub fn iso_from_time_value(millis: f64) -> Option<String> {
    if !millis.is_finite() || millis.abs() > MAX_TIME_MILLIS {
        return None;
    }
    // TimeClip truncates toward zero and the range check keeps it exact.
    #[allow(clippy::cast_possible_truncation)]
    let millis = millis.trunc() as i64;
    let days = millis.div_euclid(86_400_000);
    let of_day = millis.rem_euclid(86_400_000);
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
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    ))
}

/// Proleptic Gregorian date for days since 1970-01-01 (Howard Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `new Date(text).toISOString()`, or `None` for an invalid date.
#[must_use]
pub fn iso_from_date_string(text: &str) -> Option<String> {
    #[allow(clippy::cast_precision_loss)] // Time values fit a double exactly.
    date_parse(text).and_then(|millis| iso_from_time_value(millis as f64))
}

/// `normalizeProviderReplayTimestamp(value)` for a string.
#[must_use]
pub fn normalize_replay_timestamp_text(value: &str) -> Option<String> {
    let timestamp = js_trim(value);
    if timestamp.is_empty() || date_parse(timestamp).is_none() {
        return None;
    }
    Some(timestamp.to_owned())
}

/// `normalizeProviderReplayTimestamp(value)`.
#[must_use]
pub fn normalize_replay_timestamp(
    value: Option<&spocky_contracts::js_value::JsValue>,
) -> Option<String> {
    use spocky_contracts::js_value::JsValue;
    match value? {
        JsValue::String(text) => normalize_replay_timestamp_text(text),
        JsValue::Number(number) if number.is_finite() => {
            let millis = if *number > 1_000_000_000_000.0 {
                *number
            } else {
                number * 1000.0
            };
            iso_from_time_value(millis)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{iso_from_time_value, normalize_replay_timestamp};
    use spocky_contracts::js_value::JsValue;

    // node: new Date(n).toISOString() and normalizeProviderReplayTimestamp.
    #[test]
    fn time_values_format_like_javascript() {
        assert_eq!(
            iso_from_time_value(-1.5).as_deref(),
            Some("1969-12-31T23:59:59.999Z")
        );
        assert_eq!(
            iso_from_time_value(253_402_300_800_000.0).as_deref(),
            Some("+010000-01-01T00:00:00.000Z")
        );
        assert_eq!(
            iso_from_time_value(-62_198_755_200_000.0).as_deref(),
            Some("-000001-01-01T00:00:00.000Z")
        );
        assert_eq!(iso_from_time_value(8.64e15 + 1.0), None);
        let number = |value: f64| normalize_replay_timestamp(Some(&JsValue::Number(value)));
        assert_eq!(
            number(1_700_000_000.5).as_deref(),
            Some("2023-11-14T22:13:20.500Z")
        );
        assert_eq!(
            number(1_700_000_000_123.0).as_deref(),
            Some("2023-11-14T22:13:20.123Z")
        );
        let text = |value: &str| normalize_replay_timestamp(Some(&JsValue::String(value.into())));
        assert_eq!(
            text(" 2026-10-01T10:00:00.000Z ").as_deref(),
            Some("2026-10-01T10:00:00.000Z")
        );
        assert_eq!(text("nope"), None);
        assert_eq!(text("  "), None);
    }
}
