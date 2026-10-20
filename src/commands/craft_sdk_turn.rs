//! `monocle craft --lane sdk turn TEXT` — one turn on a jarvice agent session over
//! Socket.IO `/agent`. Answer text goes to stdout, progress to stderr, failures to
//! the exit code. The socket token (a credential) lives in memory only: fetched,
//! sent as the connect auth to the tenant's own `/ws/socket.io/`, then dropped.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::commands::redact::Redactor;
use crate::error::{AppError, Result};
use crate::instrument::{Lane, Timings};
use crate::jarvice_agent as sdk;
use crate::jarvice_chats::is_valid_chat_id;
use crate::net::Client;
use crate::socketio::{ws_origin, SocketIo};

pub struct SdkTurn<'a> {
    pub client: &'a Client,
    /// jarvice base from `auth::jarvice_url_for`.
    pub base: &'a str,
    pub bearer: &'a str,
    pub text: &'a str,
    pub session: Option<&'a str>,
    pub title: Option<&'a str>,
    pub timeout: Duration,
    pub json: bool,
    /// Print one timings JSON line to stderr after the turn.
    pub timings: bool,
    /// Diagnostics JSONL file (already resolved from flag/env), if any.
    pub diagnostics_file: Option<std::path::PathBuf>,
}

/// Server error strings that are safe and useful to show; anything else is generic.
const KNOWN_ERRORS: [&str; 10] = [
    "Not authenticated",
    "session_id required",
    "Session not found",
    "Access denied",
    "Failed to connect to Runtime",
    "session_id and content required",
    "Session mismatch",
    "Failed to send message",
    "Failed to interrupt",
    "Failed to reconnect session",
];

fn short(v: &Value) -> String {
    let t = v
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| v.to_string());
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn timed_out(t: &SdkTurn) -> AppError {
    AppError::new(format!(
        "Timed out after {}s waiting for the turn.",
        t.timeout.as_secs()
    ))
}

/// One HTTP call under the overall deadline: it gets what is left (never more
/// than its normal cap); a failure at/after the deadline is reported as the timeout.
fn http<T>(t: &SdkTurn, deadline: Instant, call: impl FnOnce() -> Result<T>) -> Result<T> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(timed_out(t));
    }
    t.client.set_request_cap(Some(left));
    let r = call();
    let left = deadline.saturating_duration_since(Instant::now());
    // The request's own timer can fire a hair before our clock does.
    match r {
        Err(_) if left < Duration::from_millis(200) => Err(timed_out(t)),
        r => r,
    }
}

/// `--timeout` is ONE deadline from here on: session creation, the token fetch,
/// the socket connect, waiting for `active` and the turn all share it. Every
/// byte printed goes through the [`Redactor`] (bearer + socket token).
pub fn run(t: &SdkTurn) -> Result<()> {
    // Vet the target before any request: nothing leaves the machine for a bad host.
    let origin = ws_origin(t.base)?;
    let id = match t.session {
        Some(s) if is_valid_chat_id(s) => Some(s.to_string()),
        Some(_) => return Err(AppError::new("Invalid session ID.")),
        None => None,
    };
    let deadline = Instant::now() + t.timeout;
    let mut r = Redactor::new(&[t.bearer.strip_prefix("Bearer ").unwrap_or(t.bearer)]);
    let mut tm = Timings::new(Lane::Sdk).with_diagnostics(
        t.diagnostics_file.clone(),
        crate::instrument::host_of(t.base),
    );
    let res = run_inner(t, &origin, id, &mut r, deadline, &mut tm);
    t.client.set_request_cap(None);
    tm.finish(res.as_ref().err().map(|e| e.to_string()).as_deref());
    if t.timings {
        eprintln!("{}", tm.line());
    }
    // Server-provided text can sit inside any error: scrub at the boundary.
    res.map_err(|e| AppError::new(r.scrub(&e.to_string())))
}

fn run_inner(
    t: &SdkTurn,
    origin: &str,
    id: Option<String>,
    r: &mut Redactor,
    deadline: Instant,
    tm: &mut Timings,
) -> Result<()> {
    let id = match id {
        Some(id) => id,
        None => tm.time_session_create(|| {
            http(t, deadline, || {
                sdk::create_session(t.client, t.base, t.bearer, t.title.unwrap_or("CLI turn"))
            })
        })?,
    };
    r.errln(&format!("Session: {id}"));
    let token = http(t, deadline, || {
        sdk::socket_token(t.client, t.base, t.bearer)
    })?;
    r.add_secret(&token); // in memory only; redacted from the moment it exists
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(timed_out(t));
    }
    let mut sock = tm
        .time_connect(|| SocketIo::connect(origin, &token, left.min(Duration::from_secs(15))))
        .map_err(|e| {
            if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(200) {
                timed_out(t)
            } else {
                e
            }
        })?;
    drop(token);
    let res = drive(t, r, &id, &mut sock, deadline, tm);
    // Every exit (result, error, timeout, interrupt) flushes the held-back tail.
    flush(t, r, &id);
    if matches!(&res, Err(e) if e.to_string().starts_with("Timed out")) {
        let _ = sock.emit("interrupt", &json!({"session_id": id}));
    }
    sock.close();
    res
}

/// Emit the held-back stream tail: raw text, or a synthetic `text_delta` line.
fn flush(t: &SdkTurn, r: &Redactor, id: &str) -> bool {
    let tail = r.take_tail();
    if tail.is_empty() {
        return false;
    }
    if t.json {
        let ev = json!({"event": "text_delta", "data": {"session_id": id, "content": tail}});
        r.out(&format!("{ev}\n"));
    } else {
        r.out(&tail);
    }
    true
}

fn drive(
    t: &SdkTurn,
    r: &Redactor,
    id: &str,
    sock: &mut SocketIo,
    deadline: Instant,
    tm: &mut Timings,
) -> Result<()> {
    sock.emit("start_session", &json!({"session_id": id}))?;
    let (mut sent, mut deltas, mut wrote) = (false, false, false);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out(t));
        }
        let Some((name, mut data)) = sock.next_event(left.min(Duration::from_secs(5)))? else {
            continue;
        };
        if data["session_id"].as_str().is_some_and(|s| s != id) {
            continue;
        }
        if sent {
            tm.event();
        }
        if t.json {
            match name.as_str() {
                // Re-chunk the delta text from the same scrubbed stream.
                "text_delta" => {
                    if let Some(c) = data["content"].as_str() {
                        data["content"] = json!(r.delta(c));
                    }
                }
                "text" | "error" | "result" | "interrupted" | "reconnect_failed" => {
                    flush(t, r, id);
                    r.lead_json(&mut data);
                }
                _ => {}
            }
            r.out(&format!("{}\n", json!({"event": name, "data": data})));
        }
        let content = data["content"].as_str().unwrap_or("");
        match name.as_str() {
            "session_status" => {
                let status = data["status"].as_str().unwrap_or("");
                if status == "active" {
                    if !sent {
                        tm.sent();
                        sock.emit("message", &json!({"session_id": id, "content": t.text}))?;
                        sent = true;
                    }
                } else {
                    r.errln(&format!("[session_status] {}", short(&data["status"])));
                }
            }
            "text_delta" => {
                deltas = true;
                if !content.is_empty() {
                    tm.text();
                }
                if !t.json {
                    // (--json: `content` was already replaced by the scrubbed chunk)
                    r.out(&r.delta(content));
                    wrote = true;
                }
            }
            "text" if !deltas => {
                if !content.is_empty() {
                    tm.text();
                }
                if !t.json {
                    r.out(&r.lead(content));
                    wrote = true;
                }
            }
            "tool_use" => r.errln(&format!(
                "[tool_use] {} {}",
                short(&data["tool_name"]),
                short(&data["tool_input"])
            )),
            "tool_result" => r.errln("[tool_result]"),
            "error" => {
                let m = data["message"].as_str().unwrap_or("");
                let m = KNOWN_ERRORS
                    .iter()
                    .find(|k| **k == m)
                    .copied()
                    .unwrap_or("see server logs");
                return Err(AppError::new(format!("Turn failed: {m}.")));
            }
            "reconnect_failed" => return Err(AppError::new("Session reconnect failed.")),
            "interrupted" => return Err(AppError::new("The turn was interrupted.")),
            "result" => {
                flush(t, r, id);
                if wrote {
                    r.out("\n");
                }
                return if data["success"].as_bool() == Some(false) {
                    Err(AppError::new("The turn finished unsuccessfully."))
                } else {
                    Ok(())
                };
            }
            _ => {}
        }
    }
}
