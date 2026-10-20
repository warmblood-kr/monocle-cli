//! Table cell formats shared by the list commands, so the same fact reads the
//! same everywhere: times as `YYYY-MM-DD HH:MM` in UTC (the column header says
//! `(UTC)`), booleans as `yes`/`no`. `--json` output is never routed through
//! here and keeps the server's full values.

use chrono::{DateTime, Utc};

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";

/// Epoch seconds -> `YYYY-MM-DD HH:MM` (UTC); `-` when out of range.
pub fn time_from_unix(secs: i64) -> String {
    DateTime::from_timestamp(secs, 0).map_or("-".into(), |t| t.format(TIME_FORMAT).to_string())
}

/// RFC 3339 / ISO 8601 timestamp (any offset) -> `YYYY-MM-DD HH:MM` (UTC).
/// Text that does not parse is returned unchanged, so nothing is hidden.
pub fn time_from_iso(text: &str) -> String {
    DateTime::parse_from_rfc3339(text).map_or_else(
        |_| text.to_string(),
        |t| t.with_timezone(&Utc).format(TIME_FORMAT).to_string(),
    )
}

pub fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_become_minute_precision_utc() {
        assert_eq!(time_from_unix(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(time_from_unix(i64::MAX), "-");
    }

    #[test]
    fn iso_timestamps_are_converted_to_utc_and_unparsable_text_is_kept() {
        assert_eq!(
            time_from_iso("2026-10-20T11:07:14.486240+00:00"),
            "2026-10-20 11:07"
        );
        assert_eq!(
            time_from_iso("2026-10-20T20:07:14+09:00"),
            "2026-10-20 11:07"
        );
        assert_eq!(time_from_iso("yesterday"), "yesterday");
    }

    #[test]
    fn booleans_are_yes_no() {
        assert_eq!((yes_no(true), yes_no(false)), ("yes", "no"));
    }
}
