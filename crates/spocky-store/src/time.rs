//! `Date.parse` for the ECMAScript date-time string format, as the registries
//! compare `createdAt` values. Day overflow up to 31 rolls into the next
//! month, `24:00` is the next midnight, and fractions truncate to
//! milliseconds, all as V8 does.

// ponytail: only ISO strings with `Z`, an offset, or a date-only form parse;
// V8's legacy fallback (local-time and free-form strings) yields `None` here.
// Every registry timestamp comes from `toISOString`, so none take that path.

/// Returns epoch milliseconds, or `None` where JavaScript yields `NaN`.
#[must_use]
pub fn parse_iso_millis(value: &str) -> Option<i64> {
    let mut cursor = Cursor {
        bytes: value.as_bytes(),
        index: 0,
    };
    let year = cursor.year()?;
    let mut month = 1;
    let mut day = 1;
    if cursor.eat(b'-') {
        month = cursor.digits(2)?;
        if cursor.eat(b'-') {
            day = cursor.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut millis_of_day = 0;
    if cursor.eat(b'T') || cursor.eat(b't') {
        let hour = cursor.digits(2)?;
        cursor.expect(b':')?;
        let minute = cursor.digits(2)?;
        let mut second = 0;
        let mut millis = 0;
        if cursor.eat(b':') {
            second = cursor.digits(2)?;
            if cursor.eat(b'.') {
                millis = cursor.fraction_millis()?;
            }
        }
        if hour > 24 || minute > 59 || second > 59 {
            return None;
        }
        if hour == 24 && (minute != 0 || second != 0 || millis != 0) {
            return None;
        }
        millis_of_day = ((hour * 60 + minute) * 60 + second) * 1000 + millis;
        let offset_minutes = cursor.offset()?;
        millis_of_day -= offset_minutes * 60_000;
    }
    if cursor.index != cursor.bytes.len() {
        return None;
    }
    Some(days_from_civil(year, month, 1) * 86_400_000 + (day - 1) * 86_400_000 + millis_of_day)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl Cursor<'_> {
    fn eat(&mut self, expected: u8) -> bool {
        if self.bytes.get(self.index) == Some(&expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: u8) -> Option<()> {
        self.eat(expected).then_some(())
    }

    fn digits(&mut self, count: usize) -> Option<i64> {
        let slice = self.bytes.get(self.index..self.index + count)?;
        let mut value = 0_i64;
        for byte in slice {
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(byte - b'0');
        }
        self.index += count;
        Some(value)
    }

    fn year(&mut self) -> Option<i64> {
        if self.eat(b'+') {
            return self.digits(6);
        }
        if self.eat(b'-') {
            let year = self.digits(6)?;
            return (year != 0).then_some(-year);
        }
        self.digits(4)
    }

    fn fraction_millis(&mut self) -> Option<i64> {
        let start = self.index;
        while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
            self.index += 1;
        }
        let digits = &self.bytes[start..self.index];
        if digits.is_empty() {
            return None;
        }
        let mut millis = 0_i64;
        for position in 0..3 {
            millis = millis * 10
                + digits
                    .get(position)
                    .map_or(0, |byte| i64::from(byte - b'0'));
        }
        Some(millis)
    }

    /// `Z`, `z`, or `+HH:mm` / `-HH:mm`, in minutes east of UTC. A date-time
    /// without any offset is local time in JavaScript and is rejected here.
    fn offset(&mut self) -> Option<i64> {
        if self.eat(b'Z') || self.eat(b'z') {
            return Some(0);
        }
        let sign = if self.eat(b'+') {
            1
        } else if self.eat(b'-') {
            -1
        } else {
            return None;
        };
        let hours = self.digits(2)?;
        self.expect(b':')?;
        let minutes = self.digits(2)?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        Some(sign * (hours * 60 + minutes))
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse_iso_millis;

    #[test]
    fn matches_node_date_parse() {
        for (input, expected) in [
            ("2026-10-01T12:34:56.789Z", Some(1_790_858_096_789)),
            ("2026-10-01T12:34:56Z", Some(1_790_858_096_000)),
            ("2026-10-01T12:34Z", Some(1_790_858_040_000)),
            ("2026-10-01", Some(1_790_812_800_000)),
            ("2026-10", Some(1_790_812_800_000)),
            ("2026", Some(1_767_225_600_000)),
            ("2026-02-30", Some(1_772_409_600_000)),
            ("2026-02-31T00:00:00Z", Some(1_772_496_000_000)),
            ("2026-02-32", None),
            ("2026-10-01T24:00:00Z", Some(1_790_899_200_000)),
            ("2026-10-01T24:00:01Z", None),
            ("2026-10-01T12:34:56.7891Z", Some(1_790_858_096_789)),
            ("2026-10-01T12:34:56.7Z", Some(1_790_858_096_700)),
            ("2026-10-01T12:34:56+02:00", Some(1_790_850_896_000)),
            ("+002026-10-01T00:00:00Z", Some(1_790_812_800_000)),
            ("1970-01-01T00:00:00.000Z", Some(0)),
            ("1969-12-31T23:59:59.999Z", Some(-1)),
            ("garbage", None),
            ("2026-10-01T12:34:56.Z", None),
        ] {
            assert_eq!(parse_iso_millis(input), expected, "input {input:?}");
        }
    }
}
