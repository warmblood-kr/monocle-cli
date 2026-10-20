//! Small cross-cutting helpers.

use std::path::PathBuf;

use chrono::{SecondsFormat, TimeZone, Utc};

static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Global `-v/--verbose`: set once in `main`, read where protocol detail is printed.
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Global `-q/--quiet`: set once in `main`; silences [`note!`] (hints, banners, counts).
pub fn set_quiet(on: bool) {
    QUIET.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn quiet() -> bool {
    QUIET.load(std::sync::atomic::Ordering::Relaxed)
}

/// `eprintln!` for non-essential stderr (banners, hints, counts, guidance). Silenced
/// by `-q`; errors and warnings that matter keep using `eprintln!`.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => {
        if !$crate::util::quiet() {
            eprintln!($($arg)*);
        }
    };
}

/// F2: the `Profile: … — tenant …` line is noise for the default profile on prod,
/// so show it only for a non-default connection, or with `-v`. Never under `-q`
/// (unless `-v` asks for detail).
pub fn show_profile_line(profile: &str, env: &str, verbose: bool, quiet: bool) -> bool {
    verbose || (!quiet && (profile != "default" || env != "prod"))
}

/// JS `new Date(...).toISOString()` — UTC, millisecond precision, trailing `Z`.
pub fn to_iso(ts_ms: i64) -> String {
    Utc.timestamp_millis_opt(ts_ms)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Current wall-clock time in epoch milliseconds (JS `Date.now()`).
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Parse an ISO-8601 timestamp to epoch milliseconds. Returns `None` for
/// unparseable input (JS `new Date(bad).getTime()` is `NaN`).
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// `application/x-www-form-urlencoded` serialization, matching JS
/// `URLSearchParams.toString()` (space → `+`, unreserved `*-._` and alnum kept).
pub fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode for a URL path (space -> `%20`, unlike form encoding). Only
/// RFC 3986 unreserved bytes pass through, plus `/` when `keep_slash`.
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode one `application/x-www-form-urlencoded` component (`+` → space, `%XX`).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a URL query string into `(key, value)` pairs (values decoded).
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Equivalent of Node's `os.homedir()`. Tests never call this — they thread an
/// explicit base dir through `Credentials::with_home` / the `home` parameters.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    PathBuf::from(std::env::var_os(key).unwrap_or_default())
}

#[cfg(test)]
mod dx_tests {
    use super::show_profile_line;

    #[test]
    fn profile_line_only_for_non_default_or_verbose() {
        assert!(!show_profile_line("default", "prod", false, false));
        assert!(show_profile_line("work", "prod", false, false));
        assert!(show_profile_line("default", "stg", false, false));
        assert!(show_profile_line("default", "prod", true, false));
        assert!(!show_profile_line("work", "stg", false, true));
        assert!(show_profile_line("work", "stg", true, true));
    }
}
