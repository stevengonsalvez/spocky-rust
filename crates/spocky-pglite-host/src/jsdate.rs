//! JavaScript `Date` local-time semantics for the Emscripten time imports.
//!
//! The glue's `_localtime_js`, `_tzset_js` and `_mktime_js` read the host
//! zone through `Date`. Node resolves the zone from `TZ` or the system zone
//! database; `jiff::tz::TimeZone::system()` follows the same sources. Gaps
//! and folds resolve the way ECMAScript `UTC(t)` does (`compatible`).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "time values stay within the Date range of 8.64e15 ms, below 2^53"
)]

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;

/// Largest absolute time value a `Date` can hold, in milliseconds.
const MAX_TIME_MS: f64 = 8.64e15;
const MS_PER_DAY: f64 = 86_400_000.0;

/// Broken-down local or UTC time, as the glue writes it into `struct tm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fields {
    pub second: i32,
    pub minute: i32,
    pub hour: i32,
    pub day: i32,
    pub month: i32,
    pub year: i32,
    pub weekday: i32,
    pub yearday: i32,
}

pub struct JsClock {
    zone: TimeZone,
}

impl Default for JsClock {
    fn default() -> Self {
        Self::new()
    }
}

impl JsClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            zone: TimeZone::system(),
        }
    }

    #[cfg(test)]
    fn with_zone(zone: TimeZone) -> Self {
        Self { zone }
    }

    /// A clock for a named zone, for tests.
    #[cfg(test)]
    pub(crate) fn for_zone(name: &str) -> Self {
        Self {
            zone: TimeZone::get(name).expect("zone"),
        }
    }

    /// Offset of local time from UTC in milliseconds at UTC time `ms`.
    fn offset_ms(&self, ms: f64) -> f64 {
        let clamped = ms.clamp(
            Timestamp::MIN.as_millisecond() as f64,
            Timestamp::MAX.as_millisecond() as f64,
        );
        let timestamp =
            Timestamp::from_millisecond(clamped as i64).unwrap_or(Timestamp::UNIX_EPOCH);
        f64::from(self.zone.to_offset(timestamp).seconds()) * 1000.0
    }

    /// `Date.prototype.getTimezoneOffset` in minutes.
    #[must_use]
    pub fn timezone_offset_minutes(&self, ms: f64) -> f64 {
        // V8 divides the millisecond offset by 60000 in integer arithmetic.
        (-self.offset_ms(ms) / 60_000.0).trunc() + 0.0
    }

    /// `LocalTime(t)`.
    fn local_ms(&self, ms: f64) -> f64 {
        ms + self.offset_ms(ms)
    }

    /// `UTC(t)` for a local time value, with `compatible` disambiguation.
    fn utc_from_local(&self, local: f64) -> f64 {
        if !local.is_finite() || local.abs() > MAX_TIME_MS + MS_PER_DAY {
            return f64::NAN;
        }
        let fields = utc_fields(local);
        let millisecond = local - (local / 1000.0).floor() * 1000.0;
        let datetime = DateTime::new(
            i16::try_from(fields.year).unwrap_or(9999),
            i8::try_from(fields.month + 1).unwrap_or(1),
            i8::try_from(fields.day).unwrap_or(1),
            i8::try_from(fields.hour).unwrap_or(0),
            i8::try_from(fields.minute).unwrap_or(0),
            i8::try_from(fields.second).unwrap_or(0),
            0,
        );
        let Ok(datetime) = datetime else {
            // Outside jiff's civil range: apply the offset at the bound.
            return local - self.offset_ms(local);
        };
        match self.zone.to_ambiguous_zoned(datetime).compatible() {
            Ok(zoned) => zoned.timestamp().as_millisecond() as f64 + millisecond,
            Err(_) => local - self.offset_ms(local),
        }
    }

    /// `new Date(year, month, day, hours, minutes, seconds, ms).getTime()`.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the seven arguments of the Date constructor"
    )]
    pub fn local_constructor(
        &self,
        year: f64,
        month: f64,
        day: f64,
        hours: f64,
        minutes: f64,
        seconds: f64,
        milliseconds: f64,
    ) -> f64 {
        let year = if (0.0..=99.0).contains(&year.trunc()) {
            1900.0 + year.trunc()
        } else {
            year
        };
        let local = make_date(
            make_day(year, month, day),
            make_time(hours, minutes, seconds, milliseconds),
        );
        time_clip(self.utc_from_local(local))
    }

    /// Local fields of a `Date` with time value `ms`; `None` for an invalid
    /// date, where every getter returns `NaN`.
    #[must_use]
    pub fn local_fields(&self, ms: f64) -> Option<Fields> {
        if !ms.is_finite() || ms.abs() > MAX_TIME_MS {
            return None;
        }
        let local = self.local_ms(ms);
        let mut fields = utc_fields(local);
        fields.yearday = yearday(fields.year, fields.month, fields.day);
        Some(fields)
    }

    /// Whether the zone observes daylight time this year, and the January and
    /// July offsets in minutes, as `_tzset_js` and `_localtime_js` compute it.
    #[must_use]
    pub fn january_july_offsets(&self, local_year: f64) -> (f64, f64) {
        let january = self.local_constructor(local_year, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0);
        let july = self.local_constructor(local_year, 6.0, 1.0, 0.0, 0.0, 0.0, 0.0);
        (
            self.timezone_offset_minutes(january),
            self.timezone_offset_minutes(july),
        )
    }
}

/// UTC fields of a time value (`getUTC*`). `yearday` is the UTC day of year.
#[must_use]
pub fn utc_fields(ms: f64) -> Fields {
    let days = (ms / MS_PER_DAY).floor();
    let within = ms - days * MS_PER_DAY;
    let (year, month, day) = civil_from_days(days as i64);
    let seconds_of_day = (within / 1000.0).floor() as i64;
    let weekday = (days as i64 + 4).rem_euclid(7);
    let year = i32::try_from(year).unwrap_or(i32::MAX);
    let month = i32::try_from(month).unwrap_or(0) - 1;
    let day = i32::try_from(day).unwrap_or(1);
    Fields {
        second: i32::try_from(seconds_of_day % 60).unwrap_or(0),
        minute: i32::try_from((seconds_of_day / 60) % 60).unwrap_or(0),
        hour: i32::try_from(seconds_of_day / 3600).unwrap_or(0),
        day,
        month,
        year,
        weekday: i32::try_from(weekday).unwrap_or(0),
        yearday: yearday(year, month, day),
    }
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// `ydayFromDate`: zero-based day of the year.
fn yearday(year: i32, month: i32, day: i32) -> i32 {
    const LEAP: [i32; 12] = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];
    const REGULAR: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let table = if is_leap(year) { LEAP } else { REGULAR };
    table[usize::try_from(month).unwrap_or(0).min(11)] + day - 1
}

/// Days since 1970-01-01 to proleptic Gregorian year, month (1-12), day.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// ECMAScript `MakeDay`.
pub(crate) fn make_day_public(year: f64, month: f64, date: f64) -> f64 {
    make_day(year, month, date)
}

fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !year.is_finite() || !month.is_finite() || !date.is_finite() {
        return f64::NAN;
    }
    let year = year.trunc();
    let month = month.trunc();
    let date = date.trunc();
    let full_year = year + (month / 12.0).floor();
    if full_year.abs() > 400_000.0 {
        return f64::NAN;
    }
    let month_in_year = month.rem_euclid(12.0);
    let days = days_from_civil(full_year as i64, month_in_year as i64 + 1, 1);
    days as f64 + date - 1.0
}

/// ECMAScript `MakeTime`.
fn make_time(hours: f64, minutes: f64, seconds: f64, milliseconds: f64) -> f64 {
    if !hours.is_finite()
        || !minutes.is_finite()
        || !seconds.is_finite()
        || !milliseconds.is_finite()
    {
        return f64::NAN;
    }
    hours.trunc() * 3_600_000.0
        + minutes.trunc() * 60_000.0
        + seconds.trunc() * 1000.0
        + milliseconds.trunc()
}

fn make_date(day: f64, time: f64) -> f64 {
    day * MS_PER_DAY + time
}

/// ECMAScript `TimeClip`.
#[must_use]
pub fn time_clip(time: f64) -> f64 {
    if !time.is_finite() || time.abs() > MAX_TIME_MS {
        return f64::NAN;
    }
    time.trunc() + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn london_summer_and_winter_offsets_match_javascript() {
        let clock = JsClock::with_zone(TimeZone::get("Europe/London").expect("zone"));
        // 2026-07-01T12:00:00Z is 13:00 BST, getTimezoneOffset() = -60.
        let summer = 1_782_907_200_000.0;
        assert!((clock.timezone_offset_minutes(summer) + 60.0).abs() < f64::EPSILON);
        let fields = clock.local_fields(summer).expect("valid");
        assert_eq!(
            (fields.hour, fields.day, fields.month, fields.year),
            (13, 1, 6, 2026)
        );
        let (january, july) = clock.january_july_offsets(2026.0);
        assert!((january - 0.0).abs() < f64::EPSILON);
        assert!((july + 60.0).abs() < f64::EPSILON);
        // new Date(2026, 2, 29, 1, 30) falls in the spring gap and moves
        // forward to 02:30 BST, which is 01:30Z.
        let gap = clock.local_constructor(2026.0, 2.0, 29.0, 1.0, 30.0, 0.0, 0.0);
        assert!((gap - 1_774_747_800_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn utc_fields_cover_negative_and_leap_days() {
        let fields = utc_fields(-1.0);
        assert_eq!((fields.year, fields.month, fields.day), (1969, 11, 31));
        assert_eq!((fields.hour, fields.minute, fields.second), (23, 59, 59));
        let leap = utc_fields(951_782_400_000.0);
        assert_eq!(
            (leap.year, leap.month, leap.day, leap.yearday),
            (2000, 1, 29, 59)
        );
        assert_eq!(leap.weekday, 2);
    }
}
