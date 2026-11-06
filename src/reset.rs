//! Converts the reset times from the agents into one ISO 8601 format.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;

/// The output format: ISO 8601 with the UTC offset, for example `2026-11-07T07:56:12-08:00`.
const ISO_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%:z";

/// Converts a reset time to ISO 8601 in the time zone `target`.
/// Returns `None` if the text has a format that this function does not know.
///
/// Known formats:
/// - RFC 3339 from agy, for example `2026-11-07T15:56:12Z`.
/// - Text from claude, for example `resets Nov 10, 7am (America/Los_Angeles)`.
///   This text has no year. The function uses the year that puts the date nearest to `now`.
///   If the text has no date, the function uses the next occurrence of the time.
pub fn to_iso<T: TimeZone>(raw: &str, now: DateTime<Utc>, target: &T) -> Option<String>
where
    T::Offset: std::fmt::Display,
{
    let utc = parse_reset(raw.trim(), now)?;
    Some(utc.with_timezone(target).format(ISO_FORMAT).to_string())
}

fn parse_reset(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    parse_claude_reset(raw, now)
}

fn parse_claude_reset(raw: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let re = Regex::new(
        r"(?i)^(?:resets\s+)?(?:(?P<mon>[a-z]{3})[a-z]*\.?\s+(?P<day>\d{1,2})(?:,|\s+at)?\s+)?(?P<hour>\d{1,2})(?::(?P<min>\d{2}))?\s*(?P<ampm>am|pm)(?:\s*\((?P<tz>[^)]+)\))?$",
    )
    .expect("valid claude reset regex");
    let caps = re.captures(raw)?;

    let tz: Tz = match caps.name("tz") {
        Some(m) => m.as_str().trim().parse().ok()?,
        None => Tz::UTC,
    };

    let hour12: u32 = caps["hour"].parse().ok()?;
    let minute: u32 = caps
        .name("min")
        .map_or(Some(0), |m| m.as_str().parse().ok())?;
    if !(1..=12).contains(&hour12) {
        return None;
    }
    let is_pm = caps["ampm"].eq_ignore_ascii_case("pm");
    let hour = (hour12 % 12) + if is_pm { 12 } else { 0 };
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;

    let now_in_tz = now.with_timezone(&tz);

    match (caps.name("mon"), caps.name("day")) {
        (Some(mon), Some(day)) => {
            let month = month_number(mon.as_str())?;
            let day: u32 = day.as_str().parse().ok()?;
            // Use the year that puts the date nearest to now.
            (now_in_tz.year() - 1..=now_in_tz.year() + 1)
                .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
                .filter_map(|date| resolve(&tz, date, time))
                .min_by_key(|dt| (*dt - now).num_seconds().abs())
        }
        _ => {
            // Only a time: use the next occurrence of that time.
            let today = now_in_tz.date_naive();
            let first = resolve(&tz, today, time)?;
            if first >= now {
                Some(first)
            } else {
                resolve(&tz, today + Duration::days(1), time)
            }
        }
    }
}

/// Converts a wall-clock date and time in `tz` to UTC.
/// For a time that occurs two times (end of daylight saving time), use the first occurrence.
fn resolve(tz: &Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&date.and_time(time)) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

fn month_number(name: &str) -> Option<u32> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let key = name.get(..3)?.to_ascii_lowercase();
    months.iter().position(|m| *m == key).map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 16, 0, 0).unwrap()
    }

    fn pst() -> FixedOffset {
        FixedOffset::west_opt(8 * 3600).unwrap()
    }

    #[test]
    fn test_rfc3339_to_local_offset() {
        assert_eq!(
            to_iso("2026-11-07T15:56:12Z", now(), &pst()).as_deref(),
            Some("2026-11-07T07:56:12-08:00")
        );
    }

    #[test]
    fn test_claude_text_with_iana_zone() {
        assert_eq!(
            to_iso("resets Nov 10, 7am (America/Los_Angeles)", now(), &pst()).as_deref(),
            Some("2026-11-10T07:00:00-08:00")
        );
        assert_eq!(
            to_iso("resets Nov 12, 10pm (America/Los_Angeles)", now(), &pst()).as_deref(),
            Some("2026-11-12T22:00:00-08:00")
        );
    }

    #[test]
    fn test_claude_text_to_other_zone() {
        let cet = FixedOffset::east_opt(3600).unwrap();
        assert_eq!(
            to_iso("resets Nov 10, 7am (America/Los_Angeles)", now(), &cet).as_deref(),
            Some("2026-11-10T16:00:00+01:00")
        );
    }

    #[test]
    fn test_claude_text_with_minutes_and_utc() {
        assert_eq!(
            to_iso("resets Nov 10, 7:30pm (UTC)", now(), &pst()).as_deref(),
            Some("2026-11-10T11:30:00-08:00")
        );
    }

    #[test]
    fn test_claude_text_uses_next_year_at_year_end() {
        let late_dec = Utc.with_ymd_and_hms(2026, 12, 30, 12, 0, 0).unwrap();
        assert_eq!(
            to_iso("resets Jan 2, 7am (UTC)", late_dec, &Utc).as_deref(),
            Some("2027-01-02T07:00:00+00:00")
        );
    }

    #[test]
    fn test_claude_text_with_only_a_time() {
        // 16:00 UTC is 08:00 in Los Angeles, so 7am is tomorrow and 9am is today.
        assert_eq!(
            to_iso("resets 7am (America/Los_Angeles)", now(), &pst()).as_deref(),
            Some("2026-11-07T07:00:00-08:00")
        );
        assert_eq!(
            to_iso("resets 9am (America/Los_Angeles)", now(), &pst()).as_deref(),
            Some("2026-11-06T09:00:00-08:00")
        );
    }

    #[test]
    fn test_unknown_text_returns_none() {
        assert_eq!(to_iso("reset 1", now(), &pst()), None);
        assert_eq!(
            to_iso("resets Nov 10, 7am (Mars/Base)", now(), &pst()),
            None
        );
        assert_eq!(to_iso("resets Nov 10, 13pm (UTC)", now(), &pst()), None);
    }
}
