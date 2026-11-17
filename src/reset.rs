//! Converts reset timestamps from provider APIs and CLI output into `(epoch, ISO 8601)`.

use jiff::civil::{Date, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::Timestamp;
use regex::Regex;

/// The output format: ISO 8601 with the UTC offset, for example `2026-11-07T07:56:12-08:00`.
const ISO_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%:z";

/// Normalizes a raw reset string into `(unix_epoch_seconds, local_iso_8601)` in `target`.
pub fn normalize_reset(raw: &str, now: Timestamp, target: &TimeZone) -> Option<(i64, String)> {
    let ts = parse_reset(raw.trim(), now)?;
    let epoch = ts.as_second();
    let iso = ts
        .to_zoned_datetime(target.clone())
        .strftime(ISO_FORMAT)
        .to_string();
    Some((epoch, iso))
}

/// Normalizes a raw reset string using `Timestamp::now()` and the machine's system time zone.
pub fn normalize_reset_local(raw: &str) -> Option<(i64, String)> {
    normalize_reset(raw, Timestamp::now(), &TimeZone::system())
}

/// Normalizes a Unix timestamp (seconds, or milliseconds if `>= 1_000_000_000_000`)
/// into `(unix_epoch_seconds, iso_8601)` in `target`.
pub fn normalize_epoch(raw_ts: i64, target: &TimeZone) -> Option<(i64, String)> {
    if raw_ts <= 0 {
        return None;
    }
    let epoch_secs = if raw_ts >= 1_000_000_000_000 {
        raw_ts / 1000
    } else {
        raw_ts
    };
    let ts = Timestamp::from_second(epoch_secs).ok()?;
    let iso = ts
        .to_zoned_datetime(target.clone())
        .strftime(ISO_FORMAT)
        .to_string();
    Some((epoch_secs, iso))
}

/// Normalizes a Unix timestamp (seconds or milliseconds) into `(epoch, local_iso_8601)`.
pub fn normalize_epoch_local(raw_ts: i64) -> Option<(i64, String)> {
    normalize_epoch(raw_ts, &TimeZone::system())
}

/// Converts a reset time to ISO 8601 in the time zone `target`.
/// Returns `None` if the text has a format that this function does not know.
///
/// Known formats:
/// - RFC 3339, for example `2026-11-07T15:56:12Z`.
/// - Unix epoch seconds or milliseconds (`>= 1_000_000_000`).
/// - Text from claude, for example `resets Nov 10, 7am (America/Los_Angeles)`.
///   This text has no year. The function uses the year that puts the date nearest to `now`.
///   If the text has no date, the function uses the next occurrence of the time.
pub fn to_iso(raw: &str, now: Timestamp, target: &TimeZone) -> Option<String> {
    normalize_reset(raw, now, target).map(|(_, iso)| iso)
}

#[cfg(test)]
pub(crate) fn utc_ts(year: i16, month: i8, day: i8, hour: i8, min: i8, sec: i8) -> Timestamp {
    Date::new(year, month, day)
        .expect("valid test date")
        .at(hour, min, sec, 0)
        .to_zoned_datetime(TimeZone::UTC)
        .expect("valid UTC timestamp")
        .timestamp()
}

fn parse_reset(raw: &str, now: Timestamp) -> Option<Timestamp> {
    if raw.is_empty() {
        return None;
    }
    // Require an explicit time component ('T', 't', or ' ') before delegating to Timestamp::parse
    // so plain date-only strings without offsets do not parse unintentionally.
    if (raw.contains(['T', 't']) || raw.contains(' ')) && raw.len() >= 10 {
        if let Ok(ts) = raw.parse::<Timestamp>() {
            return Some(ts);
        }
    }
    // Support plain integer epoch seconds or milliseconds (>= year 2001).
    if raw.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(val) = raw.parse::<i64>() {
            if val >= 1_000_000_000 {
                let secs = if val >= 1_000_000_000_000 {
                    val / 1000
                } else {
                    val
                };
                return Timestamp::from_second(secs).ok();
            }
        }
    }
    parse_claude_reset(raw, now)
}

fn parse_claude_reset(raw: &str, now: Timestamp) -> Option<Timestamp> {
    let re = Regex::new(
        r"(?i)^(?:resets(?:\s+at|\s+on)?\s+)?(?:(?P<mon>[a-z]{3})[a-z]*\.?\s+(?P<day>\d{1,2})(?:,|\s+at)?\s+)?(?P<hour>\d{1,2})(?::(?P<min>\d{2}))?\s*(?P<ampm>am|pm)(?:\s*\((?P<tz>[^)]+)\))?$",
    )
    .expect("valid claude reset regex");
    let caps = re.captures(raw)?;

    let tz = match caps.name("tz") {
        Some(m) => {
            let name = m.as_str().trim();
            if name.eq_ignore_ascii_case("UTC") {
                TimeZone::UTC
            } else {
                TimeZone::get(name).ok()?
            }
        }
        None => TimeZone::UTC,
    };

    let hour12: u8 = caps["hour"].parse().ok()?;
    let minute: u8 = caps
        .name("min")
        .map_or(Some(0), |m| m.as_str().parse().ok())?;
    if !(1..=12).contains(&hour12) || minute >= 60 {
        return None;
    }
    let is_pm = caps["ampm"].eq_ignore_ascii_case("pm");
    let hour = (hour12 % 12) + if is_pm { 12 } else { 0 };
    let time = Time::new(hour as i8, minute as i8, 0, 0).ok()?;

    let now_in_tz = now.to_zoned_datetime(tz.clone());

    match (caps.name("mon"), caps.name("day")) {
        (Some(mon), Some(day)) => {
            let month = month_number(mon.as_str())?;
            let day: i8 = day.as_str().parse().ok()?;
            // Use the year that puts the date nearest to now.
            (now_in_tz.year() - 1..=now_in_tz.year() + 1)
                .filter_map(|year| Date::new(year, month, day).ok())
                .filter_map(|date| resolve(&tz, date, time))
                .min_by_key(|ts| ts.as_second().abs_diff(now.as_second()))
        }
        _ => {
            // Only a time: use the next occurrence of that time.
            let today = now_in_tz.date();
            let first = resolve(&tz, today, time)?;
            if first >= now {
                Some(first)
            } else {
                let tomorrow = today.tomorrow().ok()?;
                resolve(&tz, tomorrow, time)
            }
        }
    }
}

/// Converts a wall-clock date and time in `tz` to UTC.
/// For a time that occurs two times (end of daylight saving time), use the first occurrence.
fn resolve(tz: &TimeZone, date: Date, time: Time) -> Option<Timestamp> {
    let amb = tz.to_ambiguous_timestamp(date.to_datetime(time));
    if matches!(amb.offset(), AmbiguousOffset::Gap { .. }) {
        None
    } else {
        amb.earlier().ok()
    }
}

fn month_number(name: &str) -> Option<i8> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let key = name.get(..3)?.to_ascii_lowercase();
    months.iter().position(|m| *m == key).map(|i| i as i8 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::offset;

    fn now() -> Timestamp {
        utc_ts(2026, 11, 6, 16, 0, 0)
    }

    fn pst() -> TimeZone {
        TimeZone::fixed(offset(-8))
    }

    #[test]
    fn test_rfc3339_to_local_offset_and_epoch() {
        let (epoch, iso) =
            normalize_reset("2026-11-07T15:56:12Z", now(), &pst()).expect("normalize rfc3339");
        assert_eq!(iso, "2026-11-07T07:56:12-08:00");
        assert_eq!(epoch, utc_ts(2026, 11, 7, 15, 56, 12).as_second());
    }

    #[test]
    fn test_epoch_seconds_and_milliseconds() {
        let expected_ts = utc_ts(2026, 11, 7, 15, 56, 12).as_second();
        let (e1, iso1) = normalize_epoch(expected_ts, &pst()).expect("secs");
        let (e2, iso2) = normalize_epoch(expected_ts * 1000 + 450, &pst()).expect("millis");
        assert_eq!(e1, expected_ts);
        assert_eq!(e2, expected_ts);
        assert_eq!(iso1, "2026-11-07T07:56:12-08:00");
        assert_eq!(iso2, iso1);

        let (e3, iso3) =
            normalize_reset(&expected_ts.to_string(), now(), &pst()).expect("string epoch");
        assert_eq!(e3, expected_ts);
        assert_eq!(iso3, iso1);
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
        let cet = TimeZone::fixed(offset(1));
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
        let late_dec = utc_ts(2026, 12, 30, 12, 0, 0);
        assert_eq!(
            to_iso("resets Jan 2, 7am (UTC)", late_dec, &TimeZone::UTC).as_deref(),
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
