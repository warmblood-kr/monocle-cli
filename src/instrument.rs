//! Shared turn instrumentation (`--timings` today; diagnostics file next).
//!
//! * Clocks are injectable ([`Clock`]): real = `Instant` + `SystemTime`, tests = a
//!   fake, so durations assert exactly.
//! * Everything that can be printed is an allowlisted, `Serialize`d struct made of
//!   enums, hostnames-free numbers and timestamps — no free-form strings, so no
//!   token, prompt, response text, header or URL can reach the output.
//! * Each turn gets a random, non-secret run id to correlate a line with server
//!   logs. It is never sent to any server.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::Serialize;

use crate::util::{now_ms, to_iso};

pub trait Clock {
    /// Monotonic time since an arbitrary fixed origin.
    fn mono(&self) -> Duration;
    /// Wall clock, epoch milliseconds (for timestamps only, never for durations).
    fn wall_ms(&self) -> i64;
}

pub struct SystemClock(Instant);

impl SystemClock {
    pub fn new() -> Self {
        SystemClock(Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn mono(&self) -> Duration {
        self.0.elapsed()
    }
    fn wall_ms(&self) -> i64 {
        now_ms()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Openworker,
    Sdk,
    Chat,
    /// `monocle router chat` (streamed `/v1/chat/completions`).
    Router,
}

/// 16 hex chars from the CSPRNG. Random, non-secret, correlation only.
pub fn new_run_id() -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The one printed line. Field set is fixed; `None` serializes as `null`.
#[derive(Debug, Serialize)]
pub struct TimingsBody {
    pub run_id: String,
    /// UTC RFC3339, when the turn began.
    pub started_at: String,
    pub lane: Lane,
    pub t_request_ms: Option<u64>,
    pub ttfe_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub delegation_ms: Option<u64>,
    pub session_create_ms: Option<u64>,
}

#[derive(Serialize)]
struct Line<'a> {
    timings: &'a TimingsBody,
}

/// One diagnostics-file phase. Closed set: nothing free-form can be logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Connect,
    Delegation,
    SessionCreate,
    FirstEvent,
    FirstText,
    Done,
    Error,
}

/// Coarse failure class, derived from our own error text — the text itself is
/// never logged (it can carry server-provided content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Timeout,
    Interrupted,
    Server,
    Other,
}

impl ErrorKind {
    pub fn classify(msg: &str) -> Self {
        if msg.starts_with("Timed out") {
            ErrorKind::Timeout
        } else if msg.contains("was interrupted") {
            ErrorKind::Interrupted
        } else if msg.starts_with("Turn failed")
            || msg.starts_with("Session reconnect failed")
            || msg.starts_with("The turn finished unsuccessfully")
        {
            ErrorKind::Server
        } else {
            ErrorKind::Other
        }
    }
}

/// One JSON line of the diagnostics file. The ONLY thing ever written to it.
/// `host` is a bare hostname (no scheme, port, path or query).
#[derive(Debug, Serialize)]
pub struct DiagLine<'a> {
    pub ts: String,
    pub run_id: &'a str,
    pub event: Event,
    pub lane: Lane,
    pub host: Option<&'a str>,
    pub error_kind: Option<ErrorKind>,
    pub duration_ms: Option<u64>,
}

/// Rotate to `PATH.1` (replacing any older one) once the file would pass this.
pub const DIAG_CAP_BYTES: u64 = 1 << 20;

/// `--diagnostics-file`, else `MONOCLE_DIAGNOSTICS_FILE` (empty = unset). Off by default.
pub fn diagnostics_path(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| std::env::var_os("MONOCLE_DIAGNOSTICS_FILE").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty())
}

/// Hostname of a URL, or `None`. Credentials, port, path and query are dropped.
pub fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(str::to_string)
}

struct Diag {
    path: PathBuf,
    host: Option<String>,
}

impl Diag {
    fn append(&self, line: &str) {
        // Best effort: diagnostics must never fail a turn.
        let len = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        if len > 0 && len + line.len() as u64 + 1 > DIAG_CAP_BYTES {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            let _ = std::fs::rename(&self.path, old);
        }
        let mut o = std::fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600); // only applies on create
        if let Ok(mut f) = o.open(&self.path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub struct Timings {
    diag: Option<Diag>,
    clock: Box<dyn Clock>,
    origin: Duration,
    sent: Option<Duration>,
    pub body: TimingsBody,
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

impl Timings {
    pub fn new(lane: Lane) -> Self {
        Self::with_clock(lane, Box::new(SystemClock::new()))
    }

    pub fn with_clock(lane: Lane, clock: Box<dyn Clock>) -> Self {
        let origin = clock.mono();
        let body = TimingsBody {
            run_id: new_run_id(),
            started_at: to_iso(clock.wall_ms()),
            lane,
            t_request_ms: None,
            ttfe_ms: None,
            ttft_ms: None,
            total_ms: None,
            connect_ms: None,
            delegation_ms: None,
            session_create_ms: None,
        };
        Timings {
            diag: None,
            clock,
            origin,
            sent: None,
            body,
        }
    }

    /// Also append one allowlisted JSON line per phase to `path`.
    pub fn with_diagnostics(mut self, path: Option<PathBuf>, host: Option<String>) -> Self {
        self.diag = path.map(|path| Diag { path, host });
        self
    }

    fn diag(&self, event: Event, error_kind: Option<ErrorKind>, duration_ms: Option<u64>) {
        let Some(d) = &self.diag else { return };
        let line = DiagLine {
            ts: to_iso(self.clock.wall_ms()),
            run_id: &self.body.run_id,
            event,
            lane: self.body.lane,
            host: d.host.as_deref(),
            error_kind,
            duration_ms,
        };
        if let Ok(l) = serde_json::to_string(&line) {
            d.append(&l);
        }
    }

    pub fn run_id(&self) -> &str {
        &self.body.run_id
    }

    /// Run `f`, returning its result and how long it took in ms.
    pub fn time<T>(&self, f: impl FnOnce() -> T) -> (T, u64) {
        let t = self.clock.mono();
        let v = f();
        (v, ms(self.clock.mono() - t))
    }

    pub fn time_connect<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (v, d) = self.time(f);
        self.body.connect_ms = Some(d);
        self.diag(Event::Connect, None, Some(d));
        v
    }

    pub fn time_delegation<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (v, d) = self.time(f);
        self.body.delegation_ms = Some(d);
        self.diag(Event::Delegation, None, Some(d));
        v
    }

    pub fn time_session_create<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (v, d) = self.time(f);
        self.body.session_create_ms = Some(d);
        self.diag(Event::SessionCreate, None, Some(d));
        v
    }

    /// t0: the user message is being handed to the transport.
    pub fn sent(&mut self) {
        if self.sent.is_none() {
            let now = self.clock.mono();
            self.body.t_request_ms = Some(ms(now - self.origin));
            self.sent = Some(now);
        }
    }

    fn since_sent(&self) -> Option<u64> {
        self.sent.map(|t0| ms(self.clock.mono() - t0))
    }

    /// Any server event/frame; only the first one after t0 counts.
    pub fn event(&mut self) {
        if let (None, Some(d)) = (self.body.ttfe_ms, self.since_sent()) {
            self.body.ttfe_ms = Some(d);
            self.diag(Event::FirstEvent, None, Some(d));
        }
    }

    /// Non-empty assistant text (implies an event).
    pub fn text(&mut self) {
        self.event();
        if let (None, Some(d)) = (self.body.ttft_ms, self.since_sent()) {
            self.body.ttft_ms = Some(d);
            self.diag(Event::FirstText, None, Some(d));
        }
    }

    /// End of turn; `error` is the failure class, if it failed.
    pub fn finish(&mut self, error: Option<ErrorKind>) {
        self.body.total_ms = self.since_sent();
        match error {
            None => self.diag(Event::Done, None, self.body.total_ms),
            Some(k) => self.diag(Event::Error, Some(k), self.body.total_ms),
        }
    }

    pub fn line(&self) -> String {
        serde_json::to_string(&Line {
            timings: &self.body,
        })
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Fake(Rc<Cell<u64>>);
    impl Clock for Fake {
        fn mono(&self) -> Duration {
            Duration::from_millis(self.0.get())
        }
        fn wall_ms(&self) -> i64 {
            1_700_000_000_000
        }
    }

    fn fake() -> (Timings, Rc<Cell<u64>>) {
        let t = Rc::new(Cell::new(1000));
        (Timings::with_clock(Lane::Sdk, Box::new(Fake(t.clone()))), t)
    }

    #[test]
    fn exact_durations_from_a_fake_clock() {
        let (mut tm, now) = fake();
        now.set(1030);
        tm.time_connect(|| now.set(1080));
        tm.sent();
        now.set(1100);
        tm.event();
        now.set(1140);
        tm.text();
        now.set(1140 + 60);
        tm.event(); // later events never move the first one
        tm.finish(None);
        let b = &tm.body;
        assert_eq!(b.connect_ms, Some(50));
        assert_eq!(b.t_request_ms, Some(80));
        assert_eq!(
            (b.ttfe_ms, b.ttft_ms, b.total_ms),
            (Some(20), Some(60), Some(120))
        );
        assert_eq!(b.delegation_ms, None);
        assert_eq!(b.started_at, "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn null_without_text_and_events_before_send_are_ignored() {
        let (mut tm, now) = fake();
        tm.event();
        tm.sent();
        now.set(1500);
        tm.finish(None);
        assert_eq!(tm.body.ttfe_ms, None);
        assert_eq!(tm.body.ttft_ms, None);
        assert_eq!(tm.body.total_ms, Some(500));
    }

    #[test]
    fn every_diag_variant_serializes_to_the_allowed_keys_only() {
        use Event::*;
        let kinds = [
            ErrorKind::Timeout,
            ErrorKind::Interrupted,
            ErrorKind::Server,
            ErrorKind::Other,
        ];
        for lane in [Lane::Openworker, Lane::Sdk, Lane::Chat, Lane::Router] {
            for event in [
                Connect,
                Delegation,
                SessionCreate,
                FirstEvent,
                FirstText,
                Done,
                Error,
            ] {
                for k in kinds.iter().map(|k| Some(*k)).chain([None]) {
                    let l = DiagLine {
                        ts: to_iso(0),
                        run_id: "0123456789abcdef",
                        event,
                        lane,
                        host: Some("h.example.com"),
                        error_kind: k,
                        duration_ms: Some(1),
                    };
                    let v = serde_json::to_value(&l).unwrap();
                    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
                    keys.sort();
                    assert_eq!(
                        keys,
                        [
                            "duration_ms",
                            "error_kind",
                            "event",
                            "host",
                            "lane",
                            "run_id",
                            "ts"
                        ]
                    );
                    // The only strings are: ts, run_id, host, and snake_case enum names.
                    for (key, val) in v.as_object().unwrap() {
                        if let Some(t) = val.as_str() {
                            assert!(
                                matches!(key.as_str(), "ts" | "run_id" | "host")
                                    || t.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                                "{key}={t}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn host_of_drops_everything_but_the_hostname() {
        assert_eq!(
            host_of("https://user:pw@jarvice.example.com:8443/p/a?token=SECRET#f").as_deref(),
            Some("jarvice.example.com")
        );
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn classify_never_depends_on_trailing_server_text() {
        assert_eq!(
            ErrorKind::classify("Timed out after 3s waiting for the turn."),
            ErrorKind::Timeout
        );
        assert_eq!(
            ErrorKind::classify("The turn was interrupted."),
            ErrorKind::Interrupted
        );
        assert_eq!(
            ErrorKind::classify("Turn failed (error): SECRET"),
            ErrorKind::Server
        );
        assert_eq!(ErrorKind::classify("whatever SECRET"), ErrorKind::Other);
    }

    #[test]
    fn diagnostics_file_orders_rotates_and_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.jsonl");
        let (tm, now) = fake();
        let mut tm = tm.with_diagnostics(Some(path.clone()), Some("h.example.com".into()));
        tm.time_connect(|| now.set(1010));
        tm.sent();
        now.set(1030);
        tm.text();
        tm.finish(None);
        let text = std::fs::read_to_string(&path).unwrap();
        let ev: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let names: Vec<_> = ev.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(names, ["connect", "first_event", "first_text", "done"]);
        assert_eq!(ev[0]["duration_ms"], 10);
        assert_eq!(ev[3]["duration_ms"], 20);
        assert!(ev
            .iter()
            .all(|e| e["run_id"] == tm.run_id() && e["host"] == "h.example.com"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Over the cap: the old file moves to PATH.1 and a fresh one starts.
        std::fs::write(&path, vec![b'x'; DIAG_CAP_BYTES as usize]).unwrap();
        let (tm2, _) = fake();
        tm2.with_diagnostics(Some(path.clone()), None)
            .finish(Some(ErrorKind::Timeout));
        let rotated = dir.path().join("d.jsonl.1");
        assert_eq!(std::fs::metadata(rotated).unwrap().len(), DIAG_CAP_BYTES);
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(
            fresh.contains(r#""event":"error""#) && fresh.contains(r#""error_kind":"timeout""#)
        );
    }

    #[test]
    fn line_has_only_allowlisted_fields() {
        let (mut tm, _) = fake();
        tm.sent();
        tm.finish(None);
        let v: serde_json::Value = serde_json::from_str(&tm.line()).unwrap();
        let mut keys: Vec<_> = v["timings"].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "connect_ms",
                "delegation_ms",
                "lane",
                "run_id",
                "session_create_ms",
                "started_at",
                "t_request_ms",
                "total_ms",
                "ttfe_ms",
                "ttft_ms"
            ]
        );
        assert_eq!(v["timings"]["lane"], "sdk");
        let id = v["timings"]["run_id"].as_str().unwrap();
        assert!(id.len() == 16 && id.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}
