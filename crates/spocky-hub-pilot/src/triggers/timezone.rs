//! Host time zone for `receivedAt` date-times written without an offset.
//!
//! The baseline Node process reads the host zone (`TZ`, else `/etc/localtime`) and applies its
//! daylight saving rules to the wall-clock time it is given. This module reads the same zone from
//! the `TZif` database and answers the same question: which offset applies to this local time.
//! Gaps and overlaps resolve as `ECMAScript` `UTC(t)` does: a skipped local time uses the offset
//! before the transition, a repeated local time uses the first of its two instants.

use std::path::{Component, Path, PathBuf};

const DAY_MS: i64 = 86_400_000;
const ZONE_DIRECTORIES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/var/db/timezone/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
];

/// Offset of a local wall-clock time from UTC.
pub trait LocalOffset {
    /// Milliseconds east of UTC that apply to `local_ms`, the wall clock read as if it were UTC.
    fn offset_ms_at_local(&self, local_ms: i128) -> i128;
}

/// A fixed offset in milliseconds, used by tests that pin the host offset.
impl LocalOffset for i64 {
    fn offset_ms_at_local(&self, _local_ms: i128) -> i128 {
        i128::from(*self)
    }
}

impl<T: LocalOffset + ?Sized> LocalOffset for &T {
    fn offset_ms_at_local(&self, local_ms: i128) -> i128 {
        (**self).offset_ms_at_local(local_ms)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostTimeZone {
    rules: Rules,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Rules {
    Fixed(i64),
    Zone(Box<Zone>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Zone {
    transitions: Vec<i64>,
    transition_types: Vec<usize>,
    offsets: Vec<i64>,
    footer: Option<Posix>,
}

impl HostTimeZone {
    #[must_use]
    pub const fn utc() -> Self {
        Self::fixed_ms(0)
    }

    #[must_use]
    pub const fn fixed_minutes(minutes: i32) -> Self {
        Self::fixed_ms(minutes as i64 * 60_000)
    }

    const fn fixed_ms(offset_ms: i64) -> Self {
        Self {
            rules: Rules::Fixed(offset_ms),
        }
    }

    /// The zone Node would pick: `TZ`, else `/etc/localtime`. An unreadable, unknown or
    /// unsupported zone (including a version 1 `TZif` file) is UTC, as an unknown `TZ` is for the
    /// baseline runtime.
    #[must_use]
    pub fn from_env() -> Self {
        let zone = match std::env::var("TZ") {
            Ok(value) if !value.is_empty() => Self::from_tz_value(&value),
            _ => std::fs::read("/etc/localtime")
                .ok()
                .and_then(|bytes| Self::from_tzif(&bytes)),
        };
        zone.unwrap_or_else(Self::utc)
    }

    /// Reads a `TZ` value: a zone name, optionally after a colon. Known gap: a POSIX rule string
    /// such as `ABC5DEF` that has no zoneinfo file is not interpreted and gives `None`.
    #[must_use]
    pub fn from_tz_value(value: &str) -> Option<Self> {
        Self::named(value.trim_start_matches(':'))
    }

    /// Loads an IANA zone such as `Europe/London` from the system zoneinfo directories.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        if name == "UTC" {
            return Some(Self::utc());
        }
        let relative = Path::new(name);
        if name.is_empty()
            || !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        let mut directories: Vec<PathBuf> = std::env::var_os("TZDIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        directories.extend(ZONE_DIRECTORIES.iter().map(PathBuf::from));
        directories
            .iter()
            .find_map(|directory| std::fs::read(directory.join(relative)).ok())
            .and_then(|bytes| Self::from_tzif(&bytes))
    }

    /// Parses a `TZif` file (RFC 8536), version 2 or later; the POSIX footer supplies the rules for
    /// instants after the last stored transition.
    #[must_use]
    pub fn from_tzif(bytes: &[u8]) -> Option<Self> {
        let first = Header::parse(bytes)?;
        if first.version < b'2' {
            return None;
        }
        let second_start = Header::LENGTH + first.body_length(4);
        let second = Header::parse(bytes.get(second_start..)?)?;
        let body_start = second_start + Header::LENGTH;
        let body = bytes.get(body_start..body_start + second.body_length(8))?;
        let footer_text = bytes.get(body_start + second.body_length(8)..)?;
        let mut cursor = 0;
        let mut take = |length: usize| {
            let slice = body.get(cursor..cursor + length);
            cursor += length;
            slice
        };
        let transitions = take(second.timecnt * 8)?
            .chunks_exact(8)
            .map(|chunk| chunk.try_into().ok().map(i64::from_be_bytes))
            .collect::<Option<Vec<i64>>>()?;
        let transition_types: Vec<usize> = take(second.timecnt)?
            .iter()
            .map(|&t| usize::from(t))
            .collect();
        let offsets = take(second.typecnt * 6)?
            .chunks_exact(6)
            .map(|chunk| {
                let seconds = chunk[..4].try_into().ok().map(i32::from_be_bytes)?;
                Some(i64::from(seconds))
            })
            .collect::<Option<Vec<i64>>>()?;
        if offsets.is_empty() || transition_types.iter().any(|&t| t >= offsets.len()) {
            return None;
        }
        let footer = footer_text
            .strip_prefix(b"\n")
            .and_then(|rest| rest.split(|&byte| byte == b'\n').next())
            .filter(|text| !text.is_empty())
            .and_then(|text| std::str::from_utf8(text).ok())
            .and_then(Posix::parse);
        Some(Self {
            rules: Rules::Zone(Box::new(Zone {
                transitions,
                transition_types,
                offsets,
                footer,
            })),
        })
    }

    /// Milliseconds east of UTC in effect at the UTC instant `utc_ms`.
    #[must_use]
    pub fn offset_ms_at_instant(&self, utc_ms: i64) -> i64 {
        match &self.rules {
            Rules::Fixed(offset_ms) => *offset_ms,
            Rules::Zone(zone) => zone.offset_seconds_at(utc_ms.div_euclid(1000)) * 1000,
        }
    }
}

impl LocalOffset for HostTimeZone {
    fn offset_ms_at_local(&self, local_ms: i128) -> i128 {
        if let Rules::Fixed(offset_ms) = &self.rules {
            return i128::from(*offset_ms);
        }
        let local =
            i64::try_from(local_ms).unwrap_or(if local_ms < 0 { i64::MIN } else { i64::MAX });
        let before = self.offset_ms_at_instant(local.saturating_sub(DAY_MS));
        let after = self.offset_ms_at_instant(local.saturating_add(DAY_MS));
        let applies =
            |offset: i64| self.offset_ms_at_instant(local.saturating_sub(offset)) == offset;
        let offset = match (applies(before), applies(after)) {
            // A repeated wall-clock time: the first instant is the one with the larger offset.
            (true, true) => before.max(after),
            (false, true) => after,
            // Valid before the transition, or skipped entirely: the offset before the transition.
            (true | false, false) => before,
        };
        i128::from(offset)
    }
}

impl Zone {
    fn offset_seconds_at(&self, utc: i64) -> i64 {
        let position = self.transitions.partition_point(|&at| at <= utc);
        if position == self.transitions.len()
            && let Some(footer) = &self.footer
        {
            return footer.offset_seconds_at(utc);
        }
        match position.checked_sub(1) {
            Some(index) => self.offsets[self.transition_types[index]],
            None => self.offsets[0],
        }
    }
}

struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    const LENGTH: usize = 44;

    fn parse(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..Self::LENGTH)?;
        if &header[..4] != b"TZif" {
            return None;
        }
        let count = |index: usize| {
            let start = 20 + index * 4;
            u32::from_be_bytes(header[start..start + 4].try_into().expect("four bytes")) as usize
        };
        Some(Self {
            version: header[4],
            isutcnt: count(0),
            isstdcnt: count(1),
            leapcnt: count(2),
            timecnt: count(3),
            typecnt: count(4),
            charcnt: count(5),
        })
    }

    /// Bytes of data after the header; transition times are `time_size` bytes wide.
    const fn body_length(&self, time_size: usize) -> usize {
        self.timecnt * (time_size + 1)
            + self.typecnt * 6
            + self.charcnt
            + self.leapcnt * (time_size + 4)
            + self.isstdcnt
            + self.isutcnt
    }
}

/// The POSIX `TZ` rule in a `TZif` footer, for example `GMT0BST,M3.5.0/1,M10.5.0`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Posix {
    standard_offset: i64,
    daylight: Option<Daylight>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Daylight {
    offset: i64,
    start: Transition,
    end: Transition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Transition {
    day: Day,
    seconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Day {
    /// `Jn`: day 1 to 365, February 29 is never counted.
    Julian(i64),
    /// `n`: day 0 to 365, February 29 is counted.
    ZeroBased(i64),
    /// `Mm.w.d`: weekday `d` (Sunday is 0) of week `w` (5 is the last) of month `m`.
    Month(i64, i64, i64),
}

impl Posix {
    fn parse(text: &str) -> Option<Self> {
        let mut reader = Reader(text.as_bytes());
        reader.name()?;
        let standard_offset = -reader.clock()?;
        if reader.0.is_empty() {
            return Some(Self {
                standard_offset,
                daylight: None,
            });
        }
        reader.name()?;
        let daylight_offset = if reader.0.first().is_some_and(|&byte| byte != b',') {
            -reader.clock()?
        } else {
            standard_offset + 3600
        };
        let (start, end) = if reader.eat(b',') {
            let start = reader.transition()?;
            reader.eat(b',').then_some(())?;
            (start, reader.transition()?)
        } else {
            let us = |month, week| Transition {
                day: Day::Month(month, week, 0),
                seconds: 7200,
            };
            (us(3, 2), us(11, 1))
        };
        reader.0.is_empty().then_some(Self {
            standard_offset,
            daylight: Some(Daylight {
                offset: daylight_offset,
                start,
                end,
            }),
        })
    }

    fn offset_seconds_at(&self, utc: i64) -> i64 {
        let Some(daylight) = &self.daylight else {
            return self.standard_offset;
        };
        let year = civil_year((utc + self.standard_offset).div_euclid(86_400));
        let start = daylight.start.utc_seconds(year, self.standard_offset);
        let end = daylight.end.utc_seconds(year, daylight.offset);
        let in_daylight = if start < end {
            (start..end).contains(&utc)
        } else {
            !(end..start).contains(&utc)
        };
        if in_daylight {
            daylight.offset
        } else {
            self.standard_offset
        }
    }
}

impl Transition {
    /// The transition instant in `year`, given the offset in force just before it.
    fn utc_seconds(&self, year: i64, offset_before: i64) -> i64 {
        let january_first = days_from_civil(year, 1, 1);
        let leap = is_leap(year);
        let day = match self.day {
            Day::Julian(n) => january_first + n - 1 + i64::from(leap && n >= 60),
            Day::ZeroBased(n) => january_first + n,
            Day::Month(month, week, weekday) => {
                let first = days_from_civil(year, month, 1);
                let first_weekday = (first + 4).rem_euclid(7);
                let mut day = first + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
                let length =
                    days_from_civil(year + i64::from(month == 12), month % 12 + 1, 1) - first;
                while day >= first + length {
                    day -= 7;
                }
                day
            }
        };
        day * 86_400 + self.seconds - offset_before
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn eat(&mut self, byte: u8) -> bool {
        let found = self.0.first() == Some(&byte);
        if found {
            self.0 = &self.0[1..];
        }
        found
    }

    fn number(&mut self) -> Option<i64> {
        let length = self
            .0
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let (digits, rest) = self.0.split_at(length);
        let value = std::str::from_utf8(digits).ok()?.parse().ok()?;
        self.0 = rest;
        Some(value)
    }

    /// A zone abbreviation: `<...>` quoted, or three or more letters.
    fn name(&mut self) -> Option<()> {
        if self.eat(b'<') {
            let length = self.0.iter().position(|&byte| byte == b'>')?;
            self.0 = &self.0[length + 1..];
            return Some(());
        }
        let length = self
            .0
            .iter()
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        (length >= 3).then(|| self.0 = &self.0[length..])
    }

    /// `[+-]hh[:mm[:ss]]` in seconds, positive as written.
    fn clock(&mut self) -> Option<i64> {
        let negative = self.eat(b'-');
        if !negative {
            self.eat(b'+');
        }
        let mut seconds = self.number()? * 3600;
        if self.eat(b':') {
            seconds += self.number()? * 60;
            if self.eat(b':') {
                seconds += self.number()?;
            }
        }
        Some(if negative { -seconds } else { seconds })
    }

    fn transition(&mut self) -> Option<Transition> {
        let day = if self.eat(b'M') {
            let month = self.number()?;
            self.eat(b'.').then_some(())?;
            let week = self.number()?;
            self.eat(b'.').then_some(())?;
            let weekday = self.number()?;
            ((1..=12).contains(&month) && (1..=5).contains(&week) && (0..=6).contains(&weekday))
                .then_some(Day::Month(month, week, weekday))?
        } else if self.eat(b'J') {
            Day::Julian(self.number()?)
        } else {
            Day::ZeroBased(self.number()?)
        };
        let seconds = if self.eat(b'/') { self.clock()? } else { 7200 };
        Some(Transition { day, seconds })
    }
}

const fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian year containing the day number `days` since 1970-01-01.
const fn civil_year(days: i64) -> i64 {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    year_of_era + era * 400 + if month_index >= 10 { 1 } else { 0 }
}
