//! The two clocks the supervisor needs, and the runtime's date format.
//!
//! Scheduling uses a monotonic clock; the health cache and `startedDate` use
//! wall-clock time. `startedDate` is RFC 3339 in UTC with whole seconds
//! (`2026-10-24T04:51:46Z`), which is the only shape the runtime emits, so the
//! parser here accepts exactly that rather than pulling in a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock milliseconds since the Unix epoch.
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` into seconds since the epoch.
pub fn parse_rfc3339_seconds(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: u32 = text.get(5..7)?.parse().ok()?;
    let day: u32 = text.get(8..10)?.parse().ok()?;
    let hour: u64 = text.get(11..13)?.parse().ok()?;
    let minute: u64 = text.get(14..16)?.parse().ok()?;
    let second: u64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let seconds = days as i128 * 86_400
        + i128::from(hour) * 3_600
        + i128::from(minute) * 60
        + i128::from(second);
    u64::try_from(seconds).ok()
}

/// Format seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_rfc3339_seconds(seconds: u64) -> String {
    let seconds = i128::from(seconds);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::{format_rfc3339_seconds, parse_rfc3339_seconds};

    #[test]
    fn the_epoch_round_trips() {
        assert_eq!(format_rfc3339_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_rfc3339_seconds("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn a_captured_started_date_round_trips() {
        let text = "2026-10-24T04:51:46Z";
        let seconds = parse_rfc3339_seconds(text).expect("the captured format parses");
        assert_eq!(format_rfc3339_seconds(seconds), text);
    }

    #[test]
    fn anything_but_the_runtime_format_is_refused() {
        assert_eq!(parse_rfc3339_seconds("2026-10-24T04:51:46.123Z"), None);
        assert_eq!(parse_rfc3339_seconds("2026-10-24T04:51:46+00:00"), None);
        assert_eq!(parse_rfc3339_seconds("not a date"), None);
        assert_eq!(parse_rfc3339_seconds("2026-13-24T04:51:46Z"), None);
    }
}
