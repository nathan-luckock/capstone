//! Date and timestamp arithmetic, parsing, and formatting, with no external
//! crate.
//!
//! A `DATE` is stored as a count of days from the Unix epoch (1970-01-01); a
//! `TIMESTAMP` as a count of microseconds from that same epoch midnight (UTC,
//! no time zone). The day <-> civil-date conversion is Howard Hinnant's
//! `days_from_civil` / `civil_from_days` algorithm, which is exact for the full
//! proleptic Gregorian calendar.

/// Microseconds in one day.
pub const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Days from the Unix epoch to the civil date `(year, month, day)`.
///
/// `month` is `[1, 12]` and `day` is `[1, 31]`; the caller validates ranges.
#[must_use]
pub const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // Mar = 0
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The civil date `(year, month, day)` for a day count from the Unix epoch.
#[must_use]
pub const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// Whether `(year, month, day)` is a real calendar date.
fn valid_civil(year: i64, month: i64, day: i64) -> bool {
    if !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let last = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day <= last
}

/// Parse a fixed-width unsigned integer field of exactly `width` ASCII digits.
fn parse_uint(s: &str, width: usize) -> Option<(i64, &str)> {
    if s.len() < width || !s.as_bytes()[..width].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let (head, rest) = s.split_at(width);
    head.parse().ok().map(|n| (n, rest))
}

/// Parse a `YYYY-MM-DD` date into days from the Unix epoch.
#[must_use]
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let (year, rest) = parse_uint(s, 4)?;
    let rest = rest.strip_prefix('-')?;
    let (month, rest) = parse_uint(rest, 2)?;
    let rest = rest.strip_prefix('-')?;
    let (day, rest) = parse_uint(rest, 2)?;
    if !rest.is_empty() || !valid_civil(year, month, day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Parse a `YYYY-MM-DD[ HH:MM:SS[.ffffff]]` timestamp into microseconds from the
/// Unix epoch. A bare date is accepted and taken at midnight; the separator
/// between date and time may be a space or `T`.
#[must_use]
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (year, rest) = parse_uint(s, 4)?;
    let rest = rest.strip_prefix('-')?;
    let (month, rest) = parse_uint(rest, 2)?;
    let rest = rest.strip_prefix('-')?;
    let (day, rest) = parse_uint(rest, 2)?;
    if !valid_civil(year, month, day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if rest.is_empty() {
        return Some(days * MICROS_PER_DAY);
    }
    // A time part follows, after a space or `T`.
    let rest = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('T'))?;
    let (hour, rest) = parse_uint(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (min, rest) = parse_uint(rest, 2)?;
    let rest = rest.strip_prefix(':')?;
    let (sec, rest) = parse_uint(rest, 2)?;
    if hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    // Optional fractional seconds, padded or truncated to microseconds.
    let micros = if let Some(frac) = rest.strip_prefix('.') {
        if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut digits = frac.to_string();
        digits.truncate(6);
        while digits.len() < 6 {
            digits.push('0');
        }
        digits.parse::<i64>().ok()?
    } else {
        if !rest.is_empty() {
            return None;
        }
        0
    };
    let time = (hour * 3600 + min * 60 + sec) * 1_000_000 + micros;
    Some(days * MICROS_PER_DAY + time)
}

/// Format days from the Unix epoch as `YYYY-MM-DD`.
#[must_use]
pub fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Format microseconds from the Unix epoch as `YYYY-MM-DD HH:MM:SS` (with a
/// `.ffffff` fraction only when nonzero).
#[must_use]
pub fn format_timestamp(micros: i64) -> String {
    // Floor-divide so a pre-epoch timestamp keeps a non-negative time of day.
    let days = micros.div_euclid(MICROS_PER_DAY);
    let rem = micros.rem_euclid(MICROS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let secs = rem / 1_000_000;
    let frac = rem % 1_000_000;
    let (hh, mm, ss) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if frac == 0 {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{frac:06}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn date_round_trips() {
        for s in ["1970-01-01", "2000-02-29", "2024-12-31", "1999-12-31"] {
            let days = parse_date(s).expect("parse");
            assert_eq!(format_date(days), s);
        }
    }

    #[test]
    fn timestamp_round_trips() {
        for s in [
            "2024-01-15 10:30:00",
            "1970-01-01 00:00:00",
            "2024-12-31 23:59:59",
        ] {
            let micros = parse_timestamp(s).expect("parse");
            assert_eq!(format_timestamp(micros), s);
        }
    }

    #[test]
    fn timestamp_accepts_t_separator_and_fraction() {
        let a = parse_timestamp("2024-01-15T10:30:00").expect("parse T");
        let b = parse_timestamp("2024-01-15 10:30:00").expect("parse space");
        assert_eq!(a, b);
        let f = parse_timestamp("2024-01-15 10:30:00.5").expect("parse frac");
        assert_eq!(format_timestamp(f), "2024-01-15 10:30:00.500000");
    }

    #[test]
    fn bare_date_is_midnight_timestamp() {
        assert_eq!(
            parse_timestamp("2024-01-15"),
            parse_date("2024-01-15").map(|d| d * MICROS_PER_DAY)
        );
    }

    #[test]
    fn rejects_garbage_and_out_of_range() {
        assert_eq!(parse_date("2024-13-01"), None);
        assert_eq!(parse_date("2024-02-30"), None);
        assert_eq!(parse_date("2023-02-29"), None); // not a leap year
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date("2024-1-1"), None); // needs zero-padding
        assert_eq!(parse_timestamp("2024-01-15 25:00:00"), None);
        assert_eq!(parse_timestamp("2024-01-15 10:30"), None);
    }

    #[test]
    fn pre_epoch_dates_work() {
        let d = parse_date("1900-01-01").expect("parse");
        assert!(d < 0);
        assert_eq!(format_date(d), "1900-01-01");
    }
}
