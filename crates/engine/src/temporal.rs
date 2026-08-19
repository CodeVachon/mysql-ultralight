//! Dates and times: parsing them, formatting them, and doing arithmetic on them.
//!
//! The engine stores temporal values as text, because that is how MySQL sends
//! them and how they arrive. Everything here converts to a civil date, does the
//! work, and converts back — no epoch seconds in the middle, because month and
//! year arithmetic is not expressible in seconds and calendar clamping
//! (31 January plus one month) has to happen in the calendar.
//!
//! Hand-rolled rather than pulled from a date crate: the engine needs one
//! format and a handful of operations, and a dependency here is paid on every
//! cold start.

use crate::error::{unsupported, EngineResult};

/// A civil date, with a time when the value carried one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micros: u32,
    /// Whether the text this came from had a time at all. A `DATE` keeps its
    /// shape through arithmetic that does not touch the clock.
    pub has_time: bool,
}

/// The unit an interval or a difference is measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Microsecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Unit {
    /// The unit named in `INTERVAL 1 DAY` or `TIMESTAMPDIFF(DAY, ...)`.
    pub fn parse(name: &str) -> EngineResult<Self> {
        Ok(match name.trim().to_ascii_uppercase().as_str() {
            "MICROSECOND" => Unit::Microsecond,
            "SECOND" => Unit::Second,
            "MINUTE" => Unit::Minute,
            "HOUR" => Unit::Hour,
            "DAY" => Unit::Day,
            "WEEK" => Unit::Week,
            "MONTH" => Unit::Month,
            "QUARTER" => Unit::Quarter,
            "YEAR" => Unit::Year,
            // The compound units — `DAY_SECOND` and friends — take a composite
            // literal this does not parse, so they are refused rather than
            // read as their leading part.
            other => return Err(unsupported(format!("the `{other}` interval unit"))),
        })
    }

    /// Whether using this unit implies a result with a time component.
    fn touches_clock(self) -> bool {
        matches!(
            self,
            Unit::Microsecond | Unit::Second | Unit::Minute | Unit::Hour
        )
    }
}

/// Days since 1970-01-01 for a civil date, proleptic Gregorian.
///
/// Howard Hinnant's `days_from_civil`, which is exact for any year rather than
/// only the range a lookup table would cover.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse: a civil date from days since the epoch.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Days in a month, so calendar arithmetic can clamp rather than overflow.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Reads `YYYY-MM-DD`, optionally followed by a time, with `T` or a space
/// between them and an optional fractional part.
///
/// `None` rather than an error, because that is what MySQL does with a bad
/// datetime *inside an expression*: `SELECT DATE_ADD('2026-13-45', INTERVAL 1
/// DAY)` is NULL and a warning, not a failed statement. Strict mode governs
/// data-change statements, not expression evaluation, so aborting here would
/// kill a query MySQL answers.
pub fn parse(text: &str) -> Option<Stamp> {
    let trimmed = text.trim();
    let (date, rest) = match trimmed.find(['T', ' ']) {
        Some(at) => (&trimmed[..at], trimmed[at + 1..].trim()),
        None => (trimmed, ""),
    };

    let mut parts = date.split('-');
    // Parsed as `i64` and range-checked before narrowing: `as u32` on an
    // out-of-range day wraps, and 4294967296 would land on zero — which the
    // check below would then accept.
    let mut number = || -> Option<i64> {
        parts
            .next()
            .filter(|p| !p.is_empty())
            .and_then(|p| p.parse().ok())
    };
    let year = number()?;
    let month = number()?;
    let day = number()?;
    // The year is bounded to MySQL's range, which also keeps every later
    // multiplication inside `i64`: a year of 10^18 would overflow `to_micros`
    // long before anything checked it.
    if parts.next().is_some() || !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let month = month as u32;
    // Day zero is refused rather than read as the last of the previous month.
    // MySQL accepts `2026-01-00` only with `ALLOW_INVALID_DATES`; by default it
    // is NULL, and reading it as 2025-12-31 would answer a different question
    // without saying so. Range-checked as `i64` before narrowing, because
    // `as u32` wraps 4294967296 to zero and that would pass this very check.
    if !(1..=days_in_month(year, month) as i64).contains(&day) {
        return None;
    }
    let day = day as u32;

    let mut stamp = Stamp {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
        micros: 0,
        has_time: !rest.is_empty(),
    };
    if rest.is_empty() {
        return Some(stamp);
    }

    // A trailing zone marker is accepted and ignored: the engine has one zone,
    // and refusing a value MySQL's own DATE_FORMAT produces would be worse.
    let clock = rest.trim_end_matches(['Z', 'z']);
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (clock, ""),
    };

    let (hour, minute, second, micros) = read_clock(clock, fraction, 23)?;
    stamp.hour = hour;
    stamp.minute = minute;
    stamp.second = second;
    stamp.micros = micros;
    Some(stamp)
}

/// The clock half of a value: `HH:MM:SS` with an optional fraction.
///
/// `max_hour` differs by caller: a DATETIME's clock stops at 23, but a bare
/// TIME runs to 838, which is the range MySQL gives the type.
fn read_clock(clock: &str, fraction: &str, max_hour: u32) -> Option<(u32, u32, u32, u32)> {
    let mut units = clock.split(':');
    let mut piece = |max: u32| -> Option<u32> {
        let value: u32 = units.next().unwrap_or("0").parse().ok()?;
        (value <= max).then_some(value)
    };
    let hour = piece(max_hour)?;
    let minute = piece(59)?;
    let second = piece(59)?;
    if units.next().is_some() {
        return None;
    }

    let mut micros = 0;
    if !fraction.is_empty() {
        let digits: String = fraction.chars().take(6).collect();
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let scale = 10u32.pow(6 - digits.len() as u32);
        micros = digits.parse::<u32>().ok()? * scale;
    }
    Some((hour, minute, second, micros))
}

/// A bare `HH:MM:SS`, which `HOUR`, `MINUTE`, `SECOND` and `MICROSECOND`
/// accept as readily as a full datetime.
///
/// The date is left at the epoch and unusable: only the clock fields mean
/// anything here, and the callers that want a calendar use `parse` instead.
pub fn parse_clock(text: &str) -> Option<Stamp> {
    let trimmed = text.trim();
    let (clock, fraction) = trimmed.split_once('.').unwrap_or((trimmed, ""));
    // A TIME has to have a colon, or "2026" would read as 2026 o'clock.
    if !clock.contains(':') {
        return None;
    }
    let (hour, minute, second, micros) = read_clock(clock.trim_start_matches('-'), fraction, 838)?;
    Some(Stamp {
        year: 1970,
        month: 1,
        day: 1,
        hour,
        minute,
        second,
        micros,
        has_time: true,
    })
}

impl Stamp {
    /// The text MySQL would show: a bare date, or a date and time, with the
    /// fraction only when there is one.
    pub fn render(&self) -> String {
        let Stamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
            micros,
            has_time,
        } = *self;

        if !has_time {
            return format!("{year:04}-{month:02}-{day:02}");
        }
        let clock = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}");
        if micros == 0 {
            clock
        } else {
            // Padded, not trimmed: MySQL renders `.100000`, and `%f` in
            // DATE_FORMAT pads too, so trimming here would make the two
            // disagree about the same instant.
            format!("{clock}.{micros:06}")
        }
    }

    /// Seconds since the epoch, ignoring the fraction.
    pub fn to_seconds(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
    }

    /// Microseconds since the epoch, which is what a sub-second difference
    /// needs.
    ///
    /// Checked because the year runs to 9999: that is inside `i64` here, but
    /// the multiply is the first place a wider value would wrap silently.
    pub fn to_micros(&self) -> Option<i64> {
        self.to_seconds()
            .checked_mul(1_000_000)?
            .checked_add(self.micros as i64)
    }

    fn from_seconds(seconds: i64, micros: u32, has_time: bool) -> Stamp {
        // Floor division, so a time before the epoch lands on the right day.
        let days = seconds.div_euclid(86_400);
        let rest = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Stamp {
            year,
            month,
            day,
            hour: (rest / 3600) as u32,
            minute: (rest % 3600 / 60) as u32,
            second: (rest % 60) as u32,
            micros,
            has_time,
        }
    }

    /// Adds an interval, clamping the day when a month is shorter.
    ///
    /// `2026-01-31` plus one month is `2026-02-28`, as MySQL does — the
    /// alternative, rolling into March, silently answers a different question.
    ///
    /// `None` when the result leaves MySQL's year range or the arithmetic
    /// would overflow. Every multiply here is reachable from a literal —
    /// `INTERVAL 9223372036854775807 YEAR` is a statement anyone can write —
    /// and unchecked it panics in debug and wraps to a plausible-looking
    /// wrong date in release.
    pub fn add(&self, unit: Unit, amount: i64) -> Option<Stamp> {
        let has_time = self.has_time || unit.touches_clock();

        let stamp = match unit {
            Unit::Year | Unit::Quarter | Unit::Month => {
                let months = match unit {
                    Unit::Year => amount.checked_mul(12)?,
                    Unit::Quarter => amount.checked_mul(3)?,
                    _ => amount,
                };
                let total = self
                    .year
                    .checked_mul(12)?
                    .checked_add(self.month as i64 - 1)?
                    .checked_add(months)?;
                let year = total.div_euclid(12);
                let month = total.rem_euclid(12) as u32 + 1;
                Stamp {
                    year,
                    month,
                    day: self.day.min(days_in_month(year, month)),
                    has_time,
                    ..*self
                }
            }
            Unit::Microsecond => {
                let total = self.to_micros()?.checked_add(amount)?;
                Stamp::from_seconds(
                    total.div_euclid(1_000_000),
                    total.rem_euclid(1_000_000) as u32,
                    has_time,
                )
            }
            _ => {
                let seconds = match unit {
                    Unit::Second => amount,
                    Unit::Minute => amount.checked_mul(60)?,
                    Unit::Hour => amount.checked_mul(3600)?,
                    Unit::Day => amount.checked_mul(86_400)?,
                    Unit::Week => amount.checked_mul(7 * 86_400)?,
                    _ => unreachable!("handled above"),
                };
                Stamp::from_seconds(
                    self.to_seconds().checked_add(seconds)?,
                    self.micros,
                    has_time,
                )
            }
        };
        // MySQL's own range. Past it the value is not a datetime it would
        // print, so NULL is the honest answer rather than a year 5 digits wide.
        (0..=9999).contains(&stamp.year).then_some(stamp)
    }

    /// Whole units from this stamp to another, truncated toward zero, which is
    /// what `TIMESTAMPDIFF` reports.
    pub fn diff(&self, unit: Unit, to: &Stamp) -> Option<i64> {
        Some(match unit {
            Unit::Year | Unit::Quarter | Unit::Month => {
                let months =
                    (to.year * 12 + to.month as i64) - (self.year * 12 + self.month as i64);
                // A partial month does not count, and the truncation is
                // toward zero: 31 Jan to 1 Mar is one month, not two, and the
                // same span backwards is minus one.
                let partial = (to.day, to.hour, to.minute, to.second)
                    < (self.day, self.hour, self.minute, self.second);
                let whole = if partial && months > 0 {
                    months - 1
                } else if !partial && months < 0 {
                    months + 1
                } else {
                    months
                };
                match unit {
                    Unit::Year => whole / 12,
                    Unit::Quarter => whole / 3,
                    _ => whole,
                }
            }
            Unit::Microsecond => to.to_micros()?.checked_sub(self.to_micros()?)?,
            _ => {
                let seconds = to.to_seconds() - self.to_seconds();
                match unit {
                    Unit::Second => seconds,
                    Unit::Minute => seconds / 60,
                    Unit::Hour => seconds / 3600,
                    Unit::Day => seconds / 86_400,
                    Unit::Week => seconds / (7 * 86_400),
                    _ => unreachable!("handled above"),
                }
            }
        })
    }

    /// The day of the week, Sunday = 1, as `DAYOFWEEK` reports it.
    pub fn day_of_week(&self) -> u32 {
        let days = days_from_civil(self.year, self.month, self.day);
        // 1970-01-01 was a Thursday, which is 5 in this numbering.
        (days + 4).rem_euclid(7) as u32 + 1
    }

    /// The day of the year, 1 for the first of January.
    pub fn day_of_year(&self) -> u32 {
        (days_from_civil(self.year, self.month, self.day) - days_from_civil(self.year, 1, 1) + 1)
            as u32
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `DATE_FORMAT`'s specifier language.
///
/// An unknown specifier is an error rather than passed through: MySQL renders
/// `%q` as `q`, but a specifier this engine has simply not implemented would
/// then be indistinguishable from a literal, and the caller would get a string
/// that looks almost right.
pub fn format(stamp: &Stamp, spec: &str) -> EngineResult<String> {
    let mut out = String::with_capacity(spec.len() + 16);
    let mut chars = spec.chars();

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(directive) = chars.next() else {
            // A trailing `%` is literal, as MySQL treats it.
            out.push('%');
            break;
        };
        match directive {
            'Y' => out.push_str(&format!("{:04}", stamp.year)),
            'y' => out.push_str(&format!("{:02}", stamp.year.rem_euclid(100))),
            'm' => out.push_str(&format!("{:02}", stamp.month)),
            'c' => out.push_str(&stamp.month.to_string()),
            'd' => out.push_str(&format!("{:02}", stamp.day)),
            'e' => out.push_str(&stamp.day.to_string()),
            'H' => out.push_str(&format!("{:02}", stamp.hour)),
            'k' => out.push_str(&stamp.hour.to_string()),
            'h' | 'I' => {
                let twelve = match stamp.hour % 12 {
                    0 => 12,
                    other => other,
                };
                out.push_str(&format!("{twelve:02}"));
            }
            'i' => out.push_str(&format!("{:02}", stamp.minute)),
            'S' | 's' => out.push_str(&format!("{:02}", stamp.second)),
            'f' => out.push_str(&format!("{:06}", stamp.micros)),
            'p' => out.push_str(if stamp.hour < 12 { "AM" } else { "PM" }),
            'M' => out.push_str(MONTHS[stamp.month as usize - 1]),
            'b' => out.push_str(&MONTHS[stamp.month as usize - 1][..3]),
            'W' => out.push_str(WEEKDAYS[stamp.day_of_week() as usize - 1]),
            'a' => out.push_str(&WEEKDAYS[stamp.day_of_week() as usize - 1][..3]),
            'j' => out.push_str(&format!("{:03}", stamp.day_of_year())),
            'T' => out.push_str(&format!(
                "{:02}:{:02}:{:02}",
                stamp.hour, stamp.minute, stamp.second
            )),
            '%' => out.push('%'),
            other => {
                return Err(unsupported(format!(
                    "the `%{other}` specifier in DATE_FORMAT()"
                )))
            }
        }
    }
    Ok(out)
}
