//! `Date.prototype.toISOString` and the `Date.parse` forms the daemon writes.
//!
//! Used for the `startedAt` field of `paseo.pid`. Parsing covers the ISO 8601
//! forms with an explicit `Z` or numeric offset and the date-only form; V8's
//! legacy and local-time fallbacks are not ported, and an unrecognised string
//! parses as `NaN` (`None`), which the caller treats as "not before boot".

use std::time::{SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Milliseconds since the Unix epoch, like `Date.now()`.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// `new Date(ms).toISOString()` for years 0000 to 9999.
#[must_use]
pub fn to_iso_string(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let of_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

fn digits(text: &str, length: usize) -> Option<i64> {
    (text.len() == length && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `Date.parse` of `YYYY-MM-DD` and `YYYY-MM-DDTHH:mm[:ss[.sss]]` with `Z` or
/// `+HH:mm` and `-HH:mm`, as milliseconds since the epoch.
///
/// Not the whole of `Date.parse`: V8 also accepts legacy forms (`2026/10/01`,
/// `Oct 1 2026`, RFC 2822 dates, `+YYYYYY` extended years, a bare `T` time),
/// which this returns `None` for. The values read here are `startedAt` fields
/// the baseline wrote with `toISOString`, which are always the ISO form.
#[must_use]
pub fn parse_iso(text: &str) -> Option<i64> {
    let (date, time) = text
        .split_once('T')
        .map_or((text, None), |(d, t)| (d, Some(t)));
    let mut date_parts = date.split('-');
    let year = digits(date_parts.next()?, 4)?;
    let month = digits(date_parts.next()?, 2)?;
    let day = digits(date_parts.next()?, 2)?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=month_days[usize::try_from(month - 1).ok()?]).contains(&day) {
        return None;
    }
    let midnight = days_from_civil(year, month, day) * 86_400_000;
    let Some(time) = time else {
        return Some(midnight);
    };
    let (clock, offset_ms) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else {
        let split = time.rfind(['+', '-'])?;
        let (clock, offset) = time.split_at(split);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset[1..].split_once(':')?;
        let (hours, minutes) = (digits(hours, 2)?, digits(minutes, 2)?);
        if hours > 23 || minutes > 59 {
            return None;
        }
        (clock, sign * (hours * 60 + minutes) * 60_000)
    };
    let mut parts = clock.splitn(3, ':');
    let hours = digits(parts.next()?, 2)?;
    let minutes = digits(parts.next()?, 2)?;
    let (seconds, fraction_ms) = match parts.next() {
        None => (0, 0),
        Some(rest) => {
            let (seconds, fraction) = rest
                .split_once('.')
                .map_or((rest, None), |(s, f)| (s, Some(f)));
            let fraction_ms = match fraction {
                None => 0,
                Some(f) if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) => {
                    digits(&format!("{:0<3}", &f[..f.len().min(3)]), 3)?
                }
                Some(_) => return None,
            };
            (digits(seconds, 2)?, fraction_ms)
        }
    };
    if hours > 24
        || minutes > 59
        || seconds > 59
        || (hours == 24 && (minutes, seconds, fraction_ms) != (0, 0, 0))
    {
        return None;
    }
    Some(midnight + ((hours * 60 + minutes) * 60 + seconds) * 1000 + fraction_ms - offset_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_to_iso_string() {
        assert_eq!(to_iso_string(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(to_iso_string(1_790_866_448_931), "2026-10-01T14:54:08.931Z");
        assert_eq!(to_iso_string(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(to_iso_string(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn round_trips_the_written_form() {
        for ms in [
            0,
            1,
            999,
            86_399_999,
            1_790_866_448_931,
            4_102_444_799_999,
            -86_400_000,
        ] {
            assert_eq!(parse_iso(&to_iso_string(ms)), Some(ms), "{ms}");
        }
    }

    #[test]
    fn parses_offsets_and_the_date_only_form() {
        assert_eq!(parse_iso("2026-10-01"), Some(1_790_812_800_000));
        assert_eq!(parse_iso("2026-10-01T00:00Z"), Some(1_790_812_800_000));
        assert_eq!(
            parse_iso("2026-10-01T02:00:00+02:00"),
            Some(1_790_812_800_000)
        );
        assert_eq!(
            parse_iso("2026-09-30T22:00:00.5-02:00"),
            Some(1_790_812_800_500)
        );
        assert_eq!(
            parse_iso("2026-10-01T00:00:00.123456Z"),
            Some(1_790_812_800_123)
        );
    }

    #[test]
    fn rejects_malformed_iso_forms() {
        for bad in [
            "",
            "x",
            "2026-13-01",
            "2026-10-01T25:00:00Z",
            "2026-10-01T00:00:60Z",
            "2026-10-01T00:00:00.Z",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad}");
        }
    }
}
