//! RFC 3339 timestamps in and out, without a date library.
//!
//! Transcripts and `sessions-index.json` write times such as
//! `2026-10-14T03:49:47.896Z`, and the ledger writes the same form. Only the
//! forms with an explicit offset are read: a time without one has no single
//! instant, and the caller falls back to the file's modification time.

use serde_json::Value;

/// The upper bound Desktop's millisecond fields stay below, from the earlier tool.
const MAX_MILLISECONDS: f64 = 100_000_000_000_000.0;

/// Read a Unix millisecond number or an RFC 3339 string.
pub(super) fn milliseconds(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => {
            let float = number.as_f64()?;
            if float > 0.0 && float < MAX_MILLISECONDS {
                // The range check above keeps the value inside i64, and a
                // fractional millisecond is dropped as the earlier tool did.
                #[allow(clippy::cast_possible_truncation, reason = "range checked above")]
                Some(float as i64)
            } else {
                None
            }
        }
        Value::String(text) => parse_rfc3339_ms(text),
        _ => None,
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+HH:MM|-HH:MM)` into Unix milliseconds.
pub(super) fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year = digits(bytes, 0, 4)?;
    let month = digits(bytes, 5, 2)?;
    let day = digits(bytes, 8, 2)?;
    let hour = digits(bytes, 11, 2)?;
    let minute = digits(bytes, 14, 2)?;
    let second = digits(bytes, 17, 2)?;
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    let mut position = 19;
    let mut millis = 0;
    if bytes.get(position) == Some(&b'.') {
        let start = position + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == start {
            return None;
        }
        for (index, byte) in bytes[start..end.min(start + 3)].iter().enumerate() {
            millis += i64::from(byte - b'0') * [100, 10, 1][index];
        }
        position = end;
    }

    let offset_seconds = match &bytes[position..] {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let hours = digits(bytes, position + 1, 2)?;
            let minutes = digits(bytes, position + 4, 2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'+' { magnitude } else { -magnitude }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(seconds * 1000 + millis)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub(super) fn format_utc(epoch_seconds: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let within = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        within / 3600,
        within % 3600 / 60,
        within % 60
    )
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(start..start + count)?;
    slice.iter().try_fold(0_i64, |total, byte| {
        byte.is_ascii_digit()
            .then(|| total * 10 + i64::from(byte - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which counts in 400-year eras so that
/// no table of month lengths is needed.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
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
