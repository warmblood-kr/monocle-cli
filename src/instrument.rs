//! Shared turn instrumentation (`--timings` today; diagnostics file next).
//!
//! * Clocks are injectable ([`Clock`]): real = `Instant` + `SystemTime`, tests = a
//!   fake, so durations assert exactly.
//! * Everything that can be printed is an allowlisted, `Serialize`d struct made of
//!   enums, hostnames-free numbers and timestamps — no free-form strings, so no
//!   token, prompt, response text, header or URL can reach the output.
//! * Each turn gets a random, non-secret run id to correlate a line with server
//!   logs. It is never sent to any server.

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

pub struct Timings {
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
            clock,
            origin,
            sent: None,
            body,
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
        v
    }

    pub fn time_delegation<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (v, d) = self.time(f);
        self.body.delegation_ms = Some(d);
        v
    }

    pub fn time_session_create<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let (v, d) = self.time(f);
        self.body.session_create_ms = Some(d);
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
        if self.body.ttfe_ms.is_none() {
            self.body.ttfe_ms = self.since_sent();
        }
    }

    /// Non-empty assistant text (implies an event).
    pub fn text(&mut self) {
        self.event();
        if self.body.ttft_ms.is_none() {
            self.body.ttft_ms = self.since_sent();
        }
    }

    pub fn finish(&mut self) {
        self.body.total_ms = self.since_sent();
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
        tm.finish();
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
        tm.finish();
        assert_eq!(tm.body.ttfe_ms, None);
        assert_eq!(tm.body.ttft_ms, None);
        assert_eq!(tm.body.total_ms, Some(500));
    }

    #[test]
    fn line_has_only_allowlisted_fields() {
        let (mut tm, _) = fake();
        tm.sent();
        tm.finish();
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
