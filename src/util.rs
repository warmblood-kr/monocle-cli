//! Small cross-cutting helpers.

use std::path::PathBuf;

use chrono::{SecondsFormat, TimeZone, Utc};

/// JS `new Date(...).toISOString()` — UTC, millisecond precision, trailing `Z`.
pub fn to_iso(ts_ms: i64) -> String {
    Utc.timestamp_millis_opt(ts_ms)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Drop every control character (C0/C1, ESC, CR, LF, TAB, DEL). Server-supplied
/// strings (ids, titles, names) are printed to the terminal, so a hostile or
/// corrupted value must not be able to forge lines or drive escape sequences.
pub fn plain(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// serde `deserialize_with` for a server string that will be printed: [`plain`]ed
/// on the way in, so no print site can forget.
pub fn de_plain<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    <String as serde::Deserialize>::deserialize(d).map(|s| plain(&s))
}

/// [`de_plain`] for an optional field (pair with `#[serde(default)]`).
pub fn de_plain_opt<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(d).map(|o| o.map(|s| plain(&s)))
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
mod plain_tests {
    use super::plain;

    #[test]
    fn plain_drops_every_control_character() {
        assert_eq!(plain("a\nb\rc\x1b[2Jd\te\u{7f}f\u{85}g"), "abc[2Jdefg");
        assert_eq!(plain("한글 🙂 ok"), "한글 🙂 ok");
    }
}
