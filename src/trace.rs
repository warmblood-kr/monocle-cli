//! W3C `traceparent` for Monocle hosts, so client timings join server spans.
//!
//! The trace id is random correlation only (CSPRNG; never derived from tokens,
//! users, prompts or hosts). The header is produced ONLY for hosts registered
//! via [`register_host`] (the same-env Monocle tenant / router / Craft hosts),
//! so it never reaches GitHub, Stark (unless it is the same host) or any
//! redirect target. `MONOCLE_NO_TRACEPARENT=1` disables it.

use std::sync::Mutex;

use rand::RngCore;

struct State {
    trace: Option<String>,
    /// A `Timings` already owns `trace` (later ones start a new trace).
    claimed: bool,
    hosts: Vec<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    trace: None,
    claimed: false,
    hosts: Vec::new(),
});

fn random_hex(bytes: usize) -> String {
    loop {
        let mut b = vec![0u8; bytes];
        rand::thread_rng().fill_bytes(&mut b);
        if b.iter().any(|x| *x != 0) {
            return b.iter().map(|x| format!("{x:02x}")).collect();
        }
    }
}

/// 32 lowercase hex chars, never all-zero.
pub fn new_trace_id() -> String {
    random_hex(16)
}

/// Start the trace of one run (`Timings`): the first run adopts the id already
/// sent by pre-turn requests (auth etc.), later runs (`-i` turns) get a new one.
pub fn begin_run() -> String {
    let mut st = STATE.lock().unwrap();
    if st.claimed || st.trace.is_none() {
        st.trace = Some(new_trace_id());
    }
    st.claimed = true;
    st.trace.clone().unwrap()
}

/// Bare lowercase host of a URL or `host[:port]` string (no scheme/port/userinfo).
fn host_of(s: &str) -> String {
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let auth = s.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or("");
    let host = match auth.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => auth.split(':').next().unwrap_or(""),
    };
    host.to_ascii_lowercase()
}

/// Allow the header for the host of `url_or_host` (a Monocle host of this env).
pub fn register_host(url_or_host: &str) {
    let host = host_of(url_or_host);
    let mut st = STATE.lock().unwrap();
    if !host.is_empty() && !st.hosts.contains(&host) {
        st.hosts.push(host);
    }
}

fn disabled() -> bool {
    std::env::var("MONOCLE_NO_TRACEPARENT").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Whether the header may be sent to `url`'s host.
pub fn is_registered(url: &str) -> bool {
    !disabled() && STATE.lock().unwrap().hosts.contains(&host_of(url))
}

/// `00-<trace>-<fresh span>-01` for a registered host, else `None`.
pub fn header_for(url: &str) -> Option<String> {
    if !is_registered(url) {
        return None;
    }
    let mut st = STATE.lock().unwrap();
    let trace = st.trace.get_or_insert_with(new_trace_id).clone();
    Some(format!("00-{trace}-{}-01", random_hex(8)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_handles_urls_ports_and_userinfo() {
        assert_eq!(host_of("https://A.example.com:8443/x?y"), "a.example.com");
        assert_eq!(host_of("localhost:1234"), "localhost");
        assert_eq!(host_of("http://u:p@[::1]:80/"), "::1");
    }

    #[test]
    fn only_registered_hosts_get_a_header() {
        register_host("https://tenant.trace-test.example");
        let h = header_for("https://tenant.trace-test.example/api").unwrap();
        assert_eq!(h.len(), 55);
        assert!(h.starts_with("00-") && h.ends_with("-01"));
        assert_ne!(
            h,
            header_for("https://tenant.trace-test.example/api").unwrap()
        );
        for u in [
            "https://api.github.com/repos/warmblood-kr/monocle-cli/releases/latest",
            "https://github.com/warmblood-kr/monocle-cli/releases/download/v1/x.tgz",
            "https://evil.example/",
        ] {
            assert!(header_for(u).is_none(), "{u}");
        }
    }

    #[test]
    fn trace_id_is_32_hex_nonzero() {
        let t = new_trace_id();
        assert_eq!(t.len(), 32);
        assert!(t
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(t, "0".repeat(32));
    }
}
