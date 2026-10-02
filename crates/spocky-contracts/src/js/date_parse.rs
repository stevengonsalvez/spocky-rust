//! `Date.parse(string)` as V8 (Node 22.20.0, V8 12.4) computes it: the
//! ECMAScript date-time format first, then the legacy free-form parser
//! (`src/date/dateparser-inl.h`), then the local time zone for strings that
//! name none (`ParseDateTimeString` in `builtins-date.cc`).
//!
//! The string is read as UTF-16 code units, and the first NUL unit ends the
//! input, as in V8.

use crate::js_value::js_text_utf16;
use crate::text::is_js_whitespace;
use jiff::civil::DateTime;
use jiff::tz::{AmbiguousOffset, TimeZone};

/// `DateParser::kNone`.
const NONE: i32 = i32::MAX;
/// `kMaxSignificantDigits`.
const MAX_SIGNIFICANT_DIGITS: i32 = 9;
const MS_PER_DAY: f64 = 86_400_000.0;
/// `kMaxTimeInMs`.
const MAX_TIME_MS: f64 = 8.64e15;
/// `DateCache::kMaxTimeBeforeUTCInMs`: `kMaxTimeInMs` plus one day.
const MAX_TIME_BEFORE_UTC_MS: f64 = MAX_TIME_MS + MS_PER_DAY;
/// `Smi::kMaxValue` for a 32-bit Smi.
const SMI_MAX: u32 = i32::MAX as u32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keyword {
    Invalid,
    MonthName,
    TimeZoneName,
    TimeSeparator,
    AmPm,
}

#[derive(Clone, Copy)]
enum Token {
    Invalid,
    Unknown,
    Number {
        value: i32,
        length: i32,
    },
    Symbol(u8),
    WhiteSpace,
    Keyword {
        kind: Keyword,
        value: i32,
        length: i32,
    },
    EndOfInput,
}

impl Token {
    const fn is_number(self) -> bool {
        matches!(self, Self::Number { .. })
    }

    const fn number(self) -> i32 {
        match self {
            Self::Number { value, .. } => value,
            _ => 0,
        }
    }

    const fn length(self) -> i32 {
        match self {
            Self::Number { length, .. } | Self::Keyword { length, .. } => length,
            Self::WhiteSpace => 1,
            _ => 0,
        }
    }

    const fn is_symbol(self, expected: u8) -> bool {
        matches!(self, Self::Symbol(symbol) if symbol == expected)
    }

    const fn is_ascii_sign(self) -> bool {
        self.is_symbol(b'+') || self.is_symbol(b'-')
    }

    /// `ascii_sign()`: `44 - value`.
    const fn ascii_sign(self) -> i32 {
        match self {
            Self::Symbol(symbol) => 44 - symbol as i32,
            _ => 0,
        }
    }

    const fn is_fixed_length_number(self, length: i32) -> bool {
        matches!(self, Self::Number { length: actual, .. } if actual == length)
    }

    const fn is_keyword_type(self, expected: Keyword) -> bool {
        matches!(self, Self::Keyword { kind, .. } if kind as u8 == expected as u8)
    }

    const fn is_keyword_z(self) -> bool {
        matches!(
            self,
            Self::Keyword {
                kind: Keyword::TimeZoneName,
                length: 1,
                ..
            }
        )
    }

    const fn is_end_of_input(self) -> bool {
        matches!(self, Self::EndOfInput)
    }

    const fn is_white_space(self) -> bool {
        matches!(self, Self::WhiteSpace)
    }

    const fn is_invalid(self) -> bool {
        matches!(self, Self::Invalid)
    }
}

/// `KeywordTable`: three-letter prefix (NUL padded), type, value.
const KEYWORDS: [([u8; 3], Keyword, i32); 28] = [
    (*b"jan", Keyword::MonthName, 1),
    (*b"feb", Keyword::MonthName, 2),
    (*b"mar", Keyword::MonthName, 3),
    (*b"apr", Keyword::MonthName, 4),
    (*b"may", Keyword::MonthName, 5),
    (*b"jun", Keyword::MonthName, 6),
    (*b"jul", Keyword::MonthName, 7),
    (*b"aug", Keyword::MonthName, 8),
    (*b"sep", Keyword::MonthName, 9),
    (*b"oct", Keyword::MonthName, 10),
    (*b"nov", Keyword::MonthName, 11),
    (*b"dec", Keyword::MonthName, 12),
    (*b"am\0", Keyword::AmPm, 0),
    (*b"pm\0", Keyword::AmPm, 12),
    (*b"ut\0", Keyword::TimeZoneName, 0),
    (*b"utc", Keyword::TimeZoneName, 0),
    (*b"z\0\0", Keyword::TimeZoneName, 0),
    (*b"gmt", Keyword::TimeZoneName, 0),
    (*b"cdt", Keyword::TimeZoneName, -5),
    (*b"cst", Keyword::TimeZoneName, -6),
    (*b"edt", Keyword::TimeZoneName, -4),
    (*b"est", Keyword::TimeZoneName, -5),
    (*b"mdt", Keyword::TimeZoneName, -6),
    (*b"mst", Keyword::TimeZoneName, -7),
    (*b"pdt", Keyword::TimeZoneName, -7),
    (*b"pst", Keyword::TimeZoneName, -8),
    (*b"t\0\0", Keyword::TimeSeparator, 0),
    // `Lookup` returns the sentinel entry when nothing matches.
    (*b"\0\0\0", Keyword::Invalid, 0),
];

/// `KeywordTable::Lookup(prefix, length)`: a word longer than the prefix
/// only matches a month name.
fn lookup_keyword(prefix: [u32; 3], length: i32) -> (Keyword, i32) {
    for (letters, kind, value) in &KEYWORDS[..KEYWORDS.len() - 1] {
        let same = (0..3).all(|index| prefix[index] == u32::from(letters[index]));
        if same && (length <= 3 || *kind == Keyword::MonthName) {
            return (*kind, *value);
        }
    }
    (Keyword::Invalid, 0)
}

/// `InputReader` and `DateStringTokenizer`, with one token of lookahead.
struct Scanner {
    units: Vec<u16>,
    position: usize,
    next: Token,
}

impl Scanner {
    fn new(units: Vec<u16>) -> Self {
        let mut scanner = Self {
            units,
            position: 0,
            next: Token::EndOfInput,
        };
        scanner.next = scanner.scan();
        scanner
    }

    /// `ch_`: the current unit, `0` at the end.
    fn ch(&self) -> u32 {
        self.units
            .get(self.position)
            .map_or(0, |unit| u32::from(*unit))
    }

    fn advance(&mut self) {
        self.position += 1;
    }

    fn is_end(&self) -> bool {
        self.ch() == 0
    }

    fn is_ascii_digit(&self) -> bool {
        (u32::from(b'0')..=u32::from(b'9')).contains(&self.ch())
    }

    fn is_ascii_alpha_or_above(&self) -> bool {
        self.ch() >= u32::from(b'A')
    }

    /// V8's tokenizer treats U+2028 and U+2029 as word characters although
    /// `String.prototype.trim` strips them (checked against Node 22.20.0).
    fn is_white_space_char(&self) -> bool {
        char::from_u32(self.ch()).is_some_and(|character| {
            is_js_whitespace(character) && !matches!(character, '\u{2028}' | '\u{2029}')
        })
    }

    fn skip(&mut self, expected: u8) -> bool {
        if self.ch() == u32::from(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// `ReadUnsignedNumeral()`.
    fn read_unsigned_numeral(&mut self) -> i32 {
        let mut value: i32 = 0;
        let mut digits = 0;
        while self.ch() == u32::from(b'0') {
            self.advance();
        }
        while self.is_ascii_digit() {
            if digits < MAX_SIGNIFICANT_DIGITS {
                value = value * 10 + i32::try_from(self.ch() - u32::from(b'0')).unwrap_or(0);
            }
            digits += 1;
            self.advance();
        }
        value
    }

    /// `ReadWord(prefix, 3)`: the lowered first three units and the length.
    fn read_word(&mut self) -> ([u32; 3], i32) {
        let mut prefix = [0_u32; 3];
        let mut length = 0_usize;
        while self.is_ascii_alpha_or_above() && !self.is_white_space_char() {
            if let Some(slot) = prefix.get_mut(length) {
                *slot = self.ch() | 0x20;
            }
            self.advance();
            length += 1;
        }
        (prefix, i32::try_from(length).unwrap_or(i32::MAX))
    }

    /// `SkipParentheses()`.
    fn skip_parentheses(&mut self) -> bool {
        if self.ch() != u32::from(b'(') {
            return false;
        }
        let mut balance = 0;
        loop {
            if self.ch() == u32::from(b')') {
                balance -= 1;
            } else if self.ch() == u32::from(b'(') {
                balance += 1;
            }
            self.advance();
            if balance <= 0 || self.ch() == 0 {
                break;
            }
        }
        true
    }

    /// `Scan()`.
    fn scan(&mut self) -> Token {
        let start = self.position;
        if self.is_end() {
            return Token::EndOfInput;
        }
        if self.is_ascii_digit() {
            let value = self.read_unsigned_numeral();
            let length = i32::try_from(self.position - start).unwrap_or(i32::MAX);
            return Token::Number { value, length };
        }
        for symbol in [b':', b'-', b'+', b'.', b')'] {
            if self.skip(symbol) {
                return Token::Symbol(symbol);
            }
        }
        if self.is_ascii_alpha_or_above() && !self.is_white_space_char() {
            let (prefix, length) = self.read_word();
            let (kind, value) = lookup_keyword(prefix, length);
            return Token::Keyword {
                kind,
                value,
                length,
            };
        }
        if self.is_white_space_char() {
            self.advance();
            return Token::WhiteSpace;
        }
        if self.skip_parentheses() {
            return Token::Unknown;
        }
        self.advance();
        Token::Unknown
    }

    fn peek(&self) -> Token {
        self.next
    }

    fn next(&mut self) -> Token {
        let current = self.next;
        self.next = self.scan();
        current
    }

    fn skip_symbol(&mut self, symbol: u8) -> bool {
        if self.next.is_symbol(symbol) {
            self.next = self.scan();
            true
        } else {
            false
        }
    }
}

const fn between(value: i32, low: i32, high: i32) -> bool {
    low <= value && value <= high
}

#[derive(Default)]
struct DayComposer {
    comps: [i32; 3],
    index: usize,
    named_month: Option<i32>,
    is_iso_date: bool,
}

impl DayComposer {
    const fn is_day(value: i32) -> bool {
        between(value, 1, 31)
    }

    const fn is_month(value: i32) -> bool {
        between(value, 1, 12)
    }

    const fn is_empty(&self) -> bool {
        self.index == 0
    }

    fn add(&mut self, value: i32) -> bool {
        if self.index < 3 {
            self.comps[self.index] = value;
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// `Write`: `(year, month 0-based, day)`.
    fn write(&mut self) -> Option<(i32, i32, i32)> {
        if self.index < 1 {
            return None;
        }
        while self.index < 3 {
            self.comps[self.index] = 1;
            self.index += 1;
        }
        let comps = self.comps;
        let (mut year, month, day);
        if let Some(named) = self.named_month {
            month = named;
            if Self::is_day(comps[0]) {
                day = comps[0];
                year = comps[1];
            } else {
                year = comps[0];
                day = comps[1];
            }
        } else if self.is_iso_date || !Self::is_day(comps[0]) {
            year = comps[0];
            month = comps[1];
            day = comps[2];
        } else {
            month = comps[0];
            day = comps[1];
            year = comps[2];
        }
        if !self.is_iso_date {
            if between(year, 0, 49) {
                year += 2000;
            } else if between(year, 50, 99) {
                year += 1900;
            }
        }
        (Self::is_month(month) && Self::is_day(day)).then_some((year, month - 1, day))
    }
}

#[derive(Default)]
struct TimeComposer {
    comps: [i32; 4],
    index: usize,
    hour_offset: Option<i32>,
}

impl TimeComposer {
    const fn is_minute(value: i32) -> bool {
        between(value, 0, 59)
    }

    const fn is_hour(value: i32) -> bool {
        between(value, 0, 23)
    }

    const fn is_second(value: i32) -> bool {
        between(value, 0, 59)
    }

    const fn is_hour12(value: i32) -> bool {
        between(value, 0, 12)
    }

    const fn is_millisecond(value: i32) -> bool {
        between(value, 0, 999)
    }

    const fn is_empty(&self) -> bool {
        self.index == 0
    }

    const fn is_expecting(&self, value: i32) -> bool {
        (self.index == 1 && Self::is_minute(value))
            || (self.index == 2 && Self::is_second(value))
            || (self.index == 3 && Self::is_millisecond(value))
    }

    fn add(&mut self, value: i32) -> bool {
        if self.index < 4 {
            self.comps[self.index] = value;
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn add_final(&mut self, value: i32) -> bool {
        if !self.add(value) {
            return false;
        }
        while self.index < 4 {
            self.comps[self.index] = 0;
            self.index += 1;
        }
        true
    }

    /// `Write`: `(hour, minute, second, millisecond)`.
    fn write(&mut self) -> Option<(i32, i32, i32, i32)> {
        while self.index < 4 {
            self.comps[self.index] = 0;
            self.index += 1;
        }
        let [mut hour, minute, second, millisecond] = self.comps;
        if let Some(offset) = self.hour_offset {
            if !Self::is_hour12(hour) {
                return None;
            }
            hour %= 12;
            hour += offset;
        }
        let valid = Self::is_hour(hour)
            && Self::is_minute(minute)
            && Self::is_second(second)
            && Self::is_millisecond(millisecond);
        // A 24th hour is allowed when the rest is zero.
        if !valid && (hour != 24 || minute != 0 || second != 0 || millisecond != 0) {
            return None;
        }
        Some((hour, minute, second, millisecond))
    }
}

/// The zone a parsed string names: none (local time) or a UTC offset.
enum Offset {
    Local,
    Seconds(i32),
}

struct TimeZoneComposer {
    sign: i32,
    hour: i32,
    minute: i32,
}

impl TimeZoneComposer {
    const fn new() -> Self {
        Self {
            sign: NONE,
            hour: NONE,
            minute: NONE,
        }
    }

    const fn set(&mut self, offset_in_hours: i32) {
        self.sign = if offset_in_hours < 0 { -1 } else { 1 };
        self.hour = offset_in_hours * self.sign;
        self.minute = 0;
    }

    const fn set_sign(&mut self, sign: i32) {
        self.sign = if sign < 0 { -1 } else { 1 };
    }

    const fn is_expecting(&self, value: i32) -> bool {
        self.hour != NONE && self.minute == NONE && TimeComposer::is_minute(value)
    }

    const fn is_utc(&self) -> bool {
        self.hour == 0 && self.minute == 0
    }

    const fn is_empty(&self) -> bool {
        self.hour == NONE
    }

    /// `Write`: the zone, or `None` when the offset is out of range.
    fn write(&mut self) -> Option<Offset> {
        if self.sign == NONE {
            return Some(Offset::Local);
        }
        if self.hour == NONE {
            self.hour = 0;
        }
        if self.minute == NONE {
            self.minute = 0;
        }
        #[allow(clippy::cast_sign_loss)] // `hour_ * 3600U` is unsigned arithmetic.
        let total = (self.hour as u32)
            .wrapping_mul(3600)
            .wrapping_add((self.minute as u32).wrapping_mul(60));
        if total > SMI_MAX {
            return None;
        }
        #[allow(clippy::cast_possible_wrap)] // Checked against `SMI_MAX` above.
        let seconds = total as i32;
        Some(Offset::Seconds(if self.sign < 0 {
            -seconds
        } else {
            seconds
        }))
    }
}

/// `ReadMilliseconds(token)`: the first three significant digits.
fn read_milliseconds(token: Token) -> i32 {
    let mut number = token.number();
    let mut length = token.length();
    if length < 3 {
        if length == 1 {
            number *= 100;
        } else if length == 2 {
            number *= 10;
        }
    } else if length > 3 {
        length = length.min(MAX_SIGNIFICANT_DIGITS);
        let mut factor = 1;
        loop {
            factor *= 10;
            length -= 1;
            if length <= 3 {
                break;
            }
        }
        number /= factor;
    }
    number
}

/// `ParseES5DateTime`: the next unhandled token, or `Invalid`.
#[allow(clippy::too_many_lines)] // One function in V8.
fn parse_es5_date_time(
    scanner: &mut Scanner,
    day: &mut DayComposer,
    time: &mut TimeComposer,
    tz: &mut TimeZoneComposer,
) -> Token {
    if scanner.peek().is_ascii_sign() {
        let sign_token = scanner.next();
        if !scanner.peek().is_fixed_length_number(6) {
            return sign_token;
        }
        let sign = sign_token.ascii_sign();
        let year = scanner.next().number();
        if sign < 0 && year == 0 {
            return sign_token;
        }
        day.add(sign * year);
    } else if scanner.peek().is_fixed_length_number(4) {
        day.add(scanner.next().number());
    } else {
        return scanner.next();
    }
    if scanner.skip_symbol(b'-') {
        if !scanner.peek().is_fixed_length_number(2)
            || !DayComposer::is_month(scanner.peek().number())
        {
            return scanner.next();
        }
        day.add(scanner.next().number());
        if scanner.skip_symbol(b'-') {
            if !scanner.peek().is_fixed_length_number(2)
                || !DayComposer::is_day(scanner.peek().number())
            {
                return scanner.next();
            }
            day.add(scanner.next().number());
        }
    }
    if scanner.peek().is_keyword_type(Keyword::TimeSeparator) {
        scanner.next();
        if !scanner.peek().is_fixed_length_number(2) || !between(scanner.peek().number(), 0, 24) {
            return Token::Invalid;
        }
        let hour_is_24 = scanner.peek().number() == 24;
        time.add(scanner.next().number());
        if !scanner.skip_symbol(b':') {
            return Token::Invalid;
        }
        if !scanner.peek().is_fixed_length_number(2)
            || !TimeComposer::is_minute(scanner.peek().number())
            || (hour_is_24 && scanner.peek().number() > 0)
        {
            return Token::Invalid;
        }
        time.add(scanner.next().number());
        if scanner.skip_symbol(b':') {
            if !scanner.peek().is_fixed_length_number(2)
                || !TimeComposer::is_second(scanner.peek().number())
                || (hour_is_24 && scanner.peek().number() > 0)
            {
                return Token::Invalid;
            }
            time.add(scanner.next().number());
            if scanner.skip_symbol(b'.') {
                if !scanner.peek().is_number() || (hour_is_24 && scanner.peek().number() > 0) {
                    return Token::Invalid;
                }
                let millis = read_milliseconds(scanner.next());
                time.add(millis);
            }
        }
        if scanner.peek().is_keyword_z() {
            scanner.next();
            tz.set(0);
        } else if scanner.peek().is_symbol(b'+') || scanner.peek().is_symbol(b'-') {
            tz.set_sign(if scanner.next().is_symbol(b'+') {
                1
            } else {
                -1
            });
            if scanner.peek().is_fixed_length_number(4) {
                let hour_minute = scanner.next().number();
                let hour = hour_minute / 100;
                let minute = hour_minute % 100;
                if !TimeComposer::is_hour(hour) || !TimeComposer::is_minute(minute) {
                    return Token::Invalid;
                }
                tz.hour = hour;
                tz.minute = minute;
            } else {
                if !scanner.peek().is_fixed_length_number(2)
                    || !TimeComposer::is_hour(scanner.peek().number())
                {
                    return Token::Invalid;
                }
                tz.hour = scanner.next().number();
                if !scanner.skip_symbol(b':') {
                    return Token::Invalid;
                }
                if !scanner.peek().is_fixed_length_number(2)
                    || !TimeComposer::is_minute(scanner.peek().number())
                {
                    return Token::Invalid;
                }
                tz.minute = scanner.next().number();
            }
        }
        if !scanner.peek().is_end_of_input() {
            return Token::Invalid;
        }
    } else if !scanner.peek().is_end_of_input() {
        return scanner.next();
    }
    if tz.is_empty() && time.is_empty() {
        tz.set(0);
    }
    day.is_iso_date = true;
    Token::EndOfInput
}

/// `DateParser::Parse`: the fields, with `None` for "local time".
struct Fields {
    year: i32,
    month: i32,
    day: i32,
    time: (i32, i32, i32, i32),
    utc_offset: Offset,
}

#[allow(clippy::too_many_lines)] // One function in V8.
fn parse_fields(units: Vec<u16>) -> Option<Fields> {
    let mut scanner = Scanner::new(units);
    let mut tz = TimeZoneComposer::new();
    let mut time = TimeComposer::default();
    let mut day = DayComposer::default();
    let first = parse_es5_date_time(&mut scanner, &mut day, &mut time, &mut tz);
    if first.is_invalid() {
        return None;
    }
    let mut has_read_number = !day.is_empty();
    let mut token = first;
    while !token.is_end_of_input() {
        match token {
            Token::Number { value: n, .. } => {
                has_read_number = true;
                if scanner.skip_symbol(b':') {
                    if scanner.skip_symbol(b':') {
                        if !time.is_empty() {
                            return None;
                        }
                        time.add(n);
                        time.add(0);
                    } else {
                        if !time.add(n) {
                            return None;
                        }
                        if scanner.peek().is_symbol(b'.') {
                            scanner.next();
                        }
                    }
                } else if scanner.skip_symbol(b'.') && time.is_expecting(n) {
                    time.add(n);
                    if !scanner.peek().is_number() {
                        return None;
                    }
                    let millis = read_milliseconds(scanner.next());
                    if millis < 0 {
                        return None;
                    }
                    time.add_final(millis);
                } else if tz.is_expecting(n) {
                    tz.minute = n;
                } else if time.is_expecting(n) {
                    time.add_final(n);
                    let peek = scanner.peek();
                    if !peek.is_end_of_input()
                        && !peek.is_white_space()
                        && !peek.is_keyword_z()
                        && !peek.is_ascii_sign()
                    {
                        return None;
                    }
                } else {
                    if !day.add(n) {
                        return None;
                    }
                    scanner.skip_symbol(b'-');
                }
            }
            Token::Keyword { kind, value, .. } => {
                if kind == Keyword::AmPm && !time.is_empty() {
                    time.hour_offset = Some(value);
                } else if kind == Keyword::MonthName {
                    day.named_month = Some(value);
                    scanner.skip_symbol(b'-');
                } else if kind == Keyword::TimeZoneName && has_read_number {
                    tz.set(value);
                } else {
                    if has_read_number {
                        return None;
                    }
                    if scanner.peek().is_number() {
                        return None;
                    }
                }
            }
            sign if sign.is_ascii_sign() && (tz.is_utc() || !time.is_empty()) => {
                tz.set_sign(sign.ascii_sign());
                let mut n = 0;
                let mut length = 0;
                if scanner.peek().is_number() {
                    let next = scanner.next();
                    length = next.length();
                    n = next.number();
                }
                has_read_number = true;
                if scanner.peek().is_symbol(b':') {
                    tz.hour = n;
                    tz.minute = NONE;
                } else if length == 2 || length == 1 {
                    tz.hour = n;
                    tz.minute = 0;
                } else if length == 4 || length == 3 {
                    tz.hour = n / 100;
                    tz.minute = n % 100;
                } else {
                    return None;
                }
            }
            other if (other.is_ascii_sign() || other.is_symbol(b')')) && has_read_number => {
                return None;
            }
            _ => {}
        }
        token = scanner.next();
    }
    let (year, month, day_of_month) = day.write()?;
    let time = time.write()?;
    let utc_offset = tz.write()?;
    Some(Fields {
        year,
        month,
        day: day_of_month,
        time,
        utc_offset,
    })
}

/// `MakeDay`'s year offset and day tables.
const YEAR_DELTA: i64 = 399_999;
const BASE_DAY: i64 = 365 * (1970 + YEAR_DELTA) + (1970 + YEAR_DELTA) / 4
    - (1970 + YEAR_DELTA) / 100
    + (1970 + YEAR_DELTA) / 400;
const COMMON: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const LEAP: [i64; 12] = [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335];

/// `MakeDay(year, month, date)`, with an integer day-of-month.
fn make_day(year: i32, month: i32, date: i32) -> Option<f64> {
    const MIN_YEAR: i32 = -1_000_000;
    const MAX_YEAR: i32 = 1_000_000;
    const MIN_MONTH: i32 = -10_000_000;
    const MAX_MONTH: i32 = 10_000_000;
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) || !(MIN_MONTH..=MAX_MONTH).contains(&month) {
        return None;
    }
    let mut y = i64::from(year);
    let mut m = i64::from(month);
    y += m / 12;
    m %= 12;
    if m < 0 {
        m += 12;
        y -= 1;
    }
    let shifted = y + YEAR_DELTA;
    let mut day_from_year = 365 * shifted + shifted / 4 - shifted / 100 + shifted / 400 - BASE_DAY;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let table = if leap { LEAP } else { COMMON };
    day_from_year += table[usize::try_from(m).ok()?];
    #[allow(clippy::cast_precision_loss)] // Within +/-4e8 days.
    Some((day_from_year - 1) as f64 + f64::from(date))
}

/// `DateCache::ToUTC` for a local time value: the offset in force before a
/// transition applies to skipped and repeated local times (ICU `kFormer`).
///
/// A year outside jiff's range keeps its month, day and time but moves by
/// whole 400-year Gregorian cycles, which leave every weekday rule intact.
fn local_to_utc(local_ms: f64, zone: &TimeZone) -> f64 {
    #[allow(clippy::cast_possible_truncation)] // Range checked by the caller.
    let local = local_ms as i64;
    let days = local.div_euclid(86_400_000);
    let of_day = local.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let cycles = if year > 9999 {
        (year - 9999 + 399) / 400
    } else if year < -9999 {
        -((-9999 - year + 399) / 400)
    } else {
        0
    };
    let built = i16::try_from(year - cycles * 400).ok().and_then(|year| {
        DateTime::new(
            year,
            i8::try_from(month).ok()?,
            i8::try_from(day).ok()?,
            i8::try_from(of_day / 3_600_000).ok()?,
            i8::try_from(of_day / 60_000 % 60).ok()?,
            i8::try_from(of_day / 1000 % 60).ok()?,
            i32::try_from(of_day % 1000 * 1_000_000).ok()?,
        )
        .ok()
    });
    let Some(civil) = built else {
        return local_ms;
    };
    // A skipped or repeated local time takes the offset in force before the
    // transition.
    let offset = match zone.to_ambiguous_timestamp(civil).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset,
        AmbiguousOffset::Gap { before, .. } | AmbiguousOffset::Fold { before, .. } => before,
    };
    let offset_seconds = i64::from(offset.seconds());
    #[allow(clippy::cast_precision_loss)] // Offsets are a few hours.
    let offset_ms = (offset_seconds * 1000) as f64;
    local_ms - offset_ms
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

/// `Date.parse(text)` with local time read from `zone`: epoch milliseconds,
/// or `None` where JavaScript yields `NaN`.
#[must_use]
pub fn date_parse_in(text: &str, zone: &TimeZone) -> Option<i64> {
    let fields = parse_fields(js_text_utf16(text).collect())?;
    let day = make_day(fields.year, fields.month, fields.day)?;
    let (hour, minute, second, millisecond) = fields.time;
    let time = f64::from(hour) * 3_600_000.0
        + f64::from(minute) * 60_000.0
        + f64::from(second) * 1000.0
        + f64::from(millisecond);
    let mut date = day * MS_PER_DAY + time;
    match fields.utc_offset {
        Offset::Local => {
            if !(-MAX_TIME_BEFORE_UTC_MS..=MAX_TIME_BEFORE_UTC_MS).contains(&date) {
                return None;
            }
            date = local_to_utc(date, zone);
        }
        Offset::Seconds(offset) => date -= f64::from(offset) * 1000.0,
    }
    if !date.is_finite() || date.abs() > MAX_TIME_MS {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // TimeClip: within +/-8.64e15.
    Some(date.trunc() as i64)
}

/// `Date.parse(text)` in the process's local time zone (`TZ`, else
/// `/etc/localtime`), read once like V8's date cache.
#[must_use]
pub fn date_parse(text: &str) -> Option<i64> {
    static ZONE: std::sync::OnceLock<TimeZone> = std::sync::OnceLock::new();
    date_parse_in(text, ZONE.get_or_init(TimeZone::system))
}
