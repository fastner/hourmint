//! Timestamp helpers. Every timestamp in the database is UTC ISO 8601 with
//! millisecond precision and a trailing `Z` (identical to JavaScript's
//! `Date.prototype.toISOString`), so string comparison equals time comparison.

use chrono::{DateTime, Local, NaiveDate, SecondsFormat, TimeZone, Utc};

use crate::error::{AppError, AppResult};

pub fn to_iso(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn now_iso() -> String {
    to_iso(now())
}

/// Parse any RFC 3339 timestamp and normalise it to the canonical UTC form.
pub fn parse_iso(s: &str) -> AppResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| AppError::Validation(format!("Invalid timestamp: {s}")))
}

pub fn normalise_iso(s: &str) -> AppResult<String> {
    parse_iso(s).map(to_iso)
}

pub fn parse_date(s: &str) -> AppResult<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| AppError::Validation(format!("Invalid date: {s}")))
}

/// Seconds between two timestamps, truncated to whole seconds, never negative.
pub fn seconds_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    (end - start).num_seconds().max(0)
}

/// The local calendar date of a UTC timestamp.
pub fn local_date(dt: DateTime<Utc>) -> NaiveDate {
    dt.with_timezone(&Local).date_naive()
}

/// UTC instant of local midnight at the start of `date`.
pub fn local_day_start(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_hms_opt(0, 0, 0).expect("valid midnight");
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(d) => d.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        // Midnight skipped by a DST change: fall back to treating it as UTC.
        chrono::LocalResult::None => Utc.from_utc_datetime(&naive),
    }
}

/// Format a date with one of the user-selectable patterns from settings.
pub fn format_date(date: NaiveDate, pattern: &str) -> String {
    match pattern {
        "DD/MM/YYYY" => date.format("%d/%m/%Y").to_string(),
        "MM/DD/YYYY" => date.format("%m/%d/%Y").to_string(),
        "DD.MM.YYYY" => date.format("%d.%m.%Y").to_string(),
        "D MMM YYYY" => date.format("%-d %b %Y").to_string(),
        "MMM D, YYYY" => date.format("%b %-d, %Y").to_string(),
        _ => date.format("%Y-%m-%d").to_string(),
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trip_matches_js_format() {
        let s = normalise_iso("2026-03-04T05:06:07+02:00").unwrap();
        assert_eq!(s, "2026-03-04T03:06:07.000Z");
        assert_eq!(
            normalise_iso("2026-03-04T03:06:07.123Z").unwrap(),
            "2026-03-04T03:06:07.123Z"
        );
    }

    #[test]
    fn formats_dates() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(format_date(d, "YYYY-MM-DD"), "2026-01-05");
        assert_eq!(format_date(d, "DD/MM/YYYY"), "05/01/2026");
        assert_eq!(format_date(d, "D MMM YYYY"), "5 Jan 2026");
    }
}
