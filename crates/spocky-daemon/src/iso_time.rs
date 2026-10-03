//! `Date.prototype.toISOString` and `Date.parse` for the `startedAt` field of
//! `paseo.pid`. Parsing is V8's: `precedesThisBoot` calls `Date.parse(startedAt)`
//! on whatever a lock file holds, so every form V8 accepts, legacy ones and the
//! local time zone included, goes through `spocky_contracts::js::date_parse`. A
//! string V8 rejects is `NaN` (`None`), which the caller treats as "not before
//! boot".

use std::time::{SystemTime, UNIX_EPOCH};

use spocky_contracts::js::date_parse;

/// Calendar date for days since 1970-01-01 in the proleptic Gregorian calendar (Hinnant).
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

/// `Date.parse(startedAt)` at `pid-lock.ts:59`, as milliseconds since the epoch;
/// `None` is `NaN`. The whole of V8's date parser, legacy forms and the local
/// time zone included, comes from `spocky_contracts::js::date_parse`.
#[must_use]
pub fn parse_iso(text: &str) -> Option<i64> {
    date_parse(text)
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
