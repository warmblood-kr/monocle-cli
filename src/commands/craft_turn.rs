//! `monocle craft turn TEXT` — drive one Craft turn over the session WebSocket.
//! Answer text goes to stdout, progress/notes to stderr, failures to the exit code.
//!
//! The server may run in "durable edge" mode where the socket is control-only: it
//! answers `turn_accepted` and engine events never stream. Then the answer is
//! polled from `GET /v1/sessions/{id}/messages` instead.

use std::time::{Duration, Instant};

use clap::ValueEnum;
use rand::RngCore;
use serde_json::{json, Value};

use crate::commands::redact::Redactor;
use crate::craft_server as craft;
use crate::error::{AppError, Result};
use crate::instrument::{Lane, Timings};
use crate::jarvice_chats::is_valid_chat_id;
use crate::net::Client;
use crate::util::percent_encode;
use crate::ws::{self, WsConn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Approve {
    Deny,
    Once,
}

/// `monocle craft turn [--lane sdk] [--env E] [--profile P]`: the same shape for both
/// lanes, flags after the subcommand; `--lane` only for sdk, `--env`/`--profile` only
/// when not the defaults (prod / default).
pub fn hint_prefix(sdk: bool, profile: &str, env: &str) -> String {
    let mut s = String::from("monocle craft turn");
    if sdk {
        s.push_str(" --lane sdk");
    }
    if env != "prod" {
        s.push_str(&format!(" --env {env}"));
    }
    if profile != "default" {
        s.push_str(&format!(" --profile {profile}"));
    }
    s
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn hint_shape_is_canonical_for_both_lanes() {
        assert_eq!(hint_prefix(false, "default", "prod"), "monocle craft turn");
        assert_eq!(
            hint_prefix(true, "default", "prod"),
            "monocle craft turn --lane sdk"
        );
        assert_eq!(
            hint_prefix(true, "work", "stg"),
            "monocle craft turn --lane sdk --env stg --profile work"
        );
        assert_eq!(
            continue_hint(&hint_prefix(false, "default", "stg"), "cli-1"),
            r#"continue with: monocle craft turn --env stg --session cli-1 "...""#
        );
    }

    #[test]
    fn interrupt_notice_names_the_session() {
        assert_eq!(
            interrupt_message(Some("cli-1")),
            "Interrupted. The server turn may still finish; session cli-1."
        );
        assert!(interrupt_message(None).starts_with("Interrupted."));
    }
}

/// The runnable hint printed to stderr right after the session id.
pub fn continue_hint(prefix: &str, id: &str) -> String {
    format!("continue with: {prefix} --session {id} \"...\"")
}

#[derive(Clone)]
pub struct TurnOpts {
    /// `-i`: this turn is part of an interactive loop (no continue hint at all).
    pub interactive: bool,
    /// A later turn of the loop on an already announced session: no `Session:` line.
    pub follow_up: bool,
    /// `--continue`: resolve `session` from the last session of this lane/profile/env.
    pub continue_last: bool,
    /// Records the session id for `--continue` once it is known.
    pub last: Option<crate::craft_state::LastSession>,
    /// Command prefix for the "continue with:" hint (see [`hint_prefix`]); `None` = no
    /// hint (only the first turn of a new, non-interactive session prints one).
    pub hint_prefix: Option<String>,
    pub text: String,
    pub session: Option<String>,
    pub workspace: Option<String>,
    pub create_workspace: bool,
    pub agent: String,
    pub timeout: Duration,
    pub approve: Approve,
    pub json: bool,
    pub poll_every: Duration,
    /// Print one timings JSON line to stderr after the turn.
    pub timings: bool,
    /// Whether `delegate` does real work (false = `--no-delegation`); only affects `delegation_ms`.
    pub delegates: bool,
    /// Diagnostics JSONL file (already resolved from flag/env), if any.
    pub diagnostics_file: Option<std::path::PathBuf>,
}

/// Where the turn talks. `ws_base` must come from [`craft::ws_base`].
pub struct Target<'a> {
    pub client: &'a Client,
    pub base: &'a str,
    pub ws_base: &'a str,
    pub bearer: &'a str,
}

static ACTIVE_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Remember the session of the running turn so Ctrl-C can name it.
pub fn note_session(id: &str) {
    if let Ok(mut s) = ACTIVE_SESSION.lock() {
        *s = Some(id.to_string());
    }
}

/// The Ctrl-C notice (stderr). The server turn is not cancelled by exiting.
pub fn interrupt_message(session: Option<&str>) -> String {
    match session {
        Some(id) => format!("Interrupted. The server turn may still finish; session {id}."),
        None => "Interrupted. The server turn may still finish.".to_string(),
    }
}

/// Replace the default SIGINT kill with the notice above; exit status stays 130.
pub fn install_interrupt_notice() {
    let _ = ctrlc::set_handler(|| {
        let id = ACTIVE_SESSION.lock().ok().and_then(|s| s.clone());
        eprintln!("\n{}", interrupt_message(id.as_deref()));
        std::process::exit(130);
    });
}

fn new_session_id() -> String {
    let mut b = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut b);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("cli-{secs}-{:02x}{:02x}", b[0], b[1])
}

fn data_text(data: &Value) -> Option<&str> {
    ["text", "delta", "content"]
        .iter()
        .find_map(|k| data[*k].as_str())
}

fn msg_text(m: &Value) -> &str {
    m["content"].as_str().or(m["text"].as_str()).unwrap_or("")
}

fn short(t: &str) -> String {
    t.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

fn timed_out(opts: &TurnOpts, what: &str) -> AppError {
    AppError::new(format!(
        "Timed out after {}s waiting for {what}.",
        opts.timeout.as_secs()
    ))
}

/// Run one HTTP call under the overall deadline: it gets what is left (never more
/// than its normal cap), and a failure at/after the deadline is reported as the
/// timeout.
fn http<T>(
    t: &Target,
    opts: &TurnOpts,
    deadline: Instant,
    call: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(timed_out(opts, "the turn"));
    }
    t.client.set_request_cap(Some(left));
    let r = call();
    let left = deadline.saturating_duration_since(Instant::now());
    // The request's own timer can fire a hair before our clock does.
    match r {
        Err(_) if left < Duration::from_millis(200) => Err(timed_out(opts, "the turn")),
        r => r,
    }
}

fn send(ws: &mut WsConn, v: Value) -> Result<()> {
    ws.send_text(&v.to_string())
}

/// Workspace to pass on the socket: the explicit one (opened first), else the
/// first recent one that still exists, else none (the server default applies).
fn resolve_workspace(
    t: &Target,
    opts: &TurnOpts,
    r: &Redactor,
    deadline: Instant,
) -> Result<Option<String>> {
    if let Some(path) = &opts.workspace {
        http(t, opts, deadline, || {
            craft::open_workspace(t.client, t.base, t.bearer, path, opts.create_workspace)
        })?;
        return Ok(Some(path.clone()));
    }
    match http(t, opts, deadline, || {
        craft::list_workspaces(t.client, t.base, t.bearer)
    }) {
        Ok(all) => {
            let found = all
                .iter()
                .find(|w| w["exists"].as_bool() == Some(true))
                .and_then(|w| w["path"].as_str().map(str::to_string));
            if found.is_none() {
                // Observed on stg: without a workspace the cowork turn is accepted
                // but never produces a message.
                r.errln(
                    "No existing workspace found; the turn may stall. Pass --workspace PATH [--create-workspace].",
                );
            }
            Ok(found)
        }
        Err(_) if deadline.saturating_duration_since(Instant::now()).is_zero() => {
            Err(timed_out(opts, "the turn"))
        }
        Err(_) => {
            r.errln("Could not list recent workspaces; using the server default.");
            Ok(None)
        }
    }
}

/// Run one turn. `delegate` runs first (before any workspace call or socket).
/// `opts.timeout` is ONE deadline from here on: delegation, workspace calls, the
/// socket connect, frame reads and polling all share it.
pub fn run_turn(
    t: &Target,
    opts: &TurnOpts,
    delegate: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    run_turn_id(t, opts, delegate).0
}

/// [`run_turn`] that also returns the session id once it is known (even on failure).
pub fn run_turn_id(
    t: &Target,
    opts: &TurnOpts,
    delegate: impl FnOnce(&str) -> Result<()>,
) -> (Result<()>, Option<String>) {
    let deadline = Instant::now() + opts.timeout;
    let token = t.bearer.strip_prefix("Bearer ").unwrap_or(t.bearer);
    let r = Redactor::new(&[token]);
    let mut tm = Timings::new(Lane::Openworker).with_diagnostics(
        opts.diagnostics_file.clone(),
        crate::instrument::host_of(t.base),
    );
    let mut sid = None;
    let res = run_inner(t, opts, delegate, &r, deadline, &mut tm, &mut sid);
    t.client.set_request_cap(None);
    tm.finish(res.as_ref().err().map(|e| e.to_string()).as_deref());
    if opts.timings {
        eprintln!("{}", tm.line());
    }
    if let Some(id) = &sid {
        if let Some(last) = &opts.last {
            last.set(id);
        }
        if let Some(prefix) = &opts.hint_prefix {
            r.errln(&continue_hint(prefix, id));
        }
    }
    // Server-provided text can sit inside any error: scrub at the boundary.
    (res.map_err(|e| AppError::new(r.scrub(&e.to_string()))), sid)
}

/// The server refused the message (`input_rejected: queue_full`, craft#225) before
/// accepting it, so sending it again cannot duplicate it.
pub fn is_queue_full(e: &AppError) -> bool {
    e.to_string().ends_with("(input_rejected): queue_full")
}

fn run_inner(
    t: &Target,
    opts: &TurnOpts,
    delegate: impl FnOnce(&str) -> Result<()>,
    r: &Redactor,
    deadline: Instant,
    tm: &mut Timings,
    sid: &mut Option<String>,
) -> Result<()> {
    let id = match &opts.session {
        Some(s) if is_valid_chat_id(s) => s.clone(),
        Some(_) => return Err(AppError::new("Invalid session ID.")),
        None => new_session_id(),
    };
    if !opts.follow_up {
        r.errln(&format!("Session: {id}"));
    }
    *sid = Some(id.clone());
    note_session(&id);
    if opts.delegates {
        tm.time_delegation(|| http(t, opts, deadline, || delegate(&id)))?;
    } else {
        http(t, opts, deadline, || delegate(&id))?;
    }
    let workspace = resolve_workspace(t, opts, r, deadline)?;
    let token = t
        .bearer
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::new("No access token available."))?;
    let mut url = format!(
        "{}/ws/session/{id}?agent={}",
        t.ws_base,
        percent_encode(&opts.agent, false)
    );
    if let Some(w) = &workspace {
        url.push_str(&format!("&workspace={}", percent_encode(w, false)));
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(timed_out(opts, "the turn"));
    }
    let mut conn = tm
        .time_connect(|| {
            ws::connect(
                &url,
                &["openworker", token],
                left.min(Duration::from_secs(15)),
            )
        })
        .map_err(|e| {
            if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(200) {
                timed_out(opts, "the turn")
            } else {
                e
            }
        })?;
    let res = drive(t, opts, r, &id, &mut conn, deadline, tm);
    // Every exit (end, error, timeout, interrupt) flushes the held-back tail.
    r.flush_stream(opts.json);
    conn.close();
    res
}

fn drive(
    t: &Target,
    opts: &TurnOpts,
    r: &Redactor,
    id: &str,
    ws: &mut WsConn,
    deadline: Instant,
    tm: &mut Timings,
) -> Result<()> {
    let timeout = || timed_out(opts, "the turn");
    let mut baseline = 0;
    let mut sent = false;
    let mut streamed = false; // deltas seen since the last assistant_message
    let mut wrote_text = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timeout());
        }
        let Some(raw) = ws.recv_text(left.min(Duration::from_secs(5)))? else {
            continue;
        };
        let Ok(frame) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let kind = frame["type"].as_str().unwrap_or("");
        if sent {
            tm.event();
        }
        // --json: the frame as emitted (delta scrubbed, lead redacted).
        let mut f = frame.clone();
        if opts.json {
            if kind == "assistant_delta" {
                if let Some(k) = ["text", "delta", "content"]
                    .into_iter()
                    .find(|k| f["data"][*k].is_string())
                {
                    let t = f["data"][k].as_str().unwrap_or("").to_string();
                    f["data"][k] = json!(r.delta(&t));
                }
            } else if matches!(
                kind,
                "assistant_message"
                    | "turn_end"
                    | "turn_done"
                    | "error"
                    | "input_rejected"
                    | "interrupted"
            ) {
                r.flush_stream(true);
                r.lead_json(&mut f["data"]);
            }
            r.emit("ws", "frame", &f);
        }
        let data = &frame["data"];
        let shown = if opts.json { &f["data"] } else { data };
        match kind {
            "ready" if !sent => {
                // Messages already in the session, so polling can tell new from old.
                baseline = http(t, opts, deadline, || {
                    craft::history(t.client, t.base, t.bearer, id)
                })
                .map(|h| h["messages"].as_array().map_or(0, Vec::len))
                .unwrap_or(0);
                tm.sent();
                send(ws, json!({"type": "user_message", "text": opts.text}))?;
                sent = true;
            }
            "assistant_delta" => {
                streamed = true;
                if data_text(data).is_some_and(|x| !x.is_empty()) {
                    tm.text();
                }
                if let (false, Some(t)) = (opts.json, data_text(data)) {
                    r.out(&r.delta(t));
                    wrote_text = true;
                }
            }
            "assistant_message" => {
                if data_text(data).is_some_and(|x| !x.is_empty()) {
                    tm.text();
                }
                if !opts.json && r.flush_stream(false) {
                    wrote_text = true;
                }
                if !opts.json && !streamed {
                    if let Some(t) = data_text(data) {
                        r.out(&r.lead(t));
                        wrote_text = true;
                    }
                }
                streamed = false;
            }
            "tool_started" | "tool_finished" | "tool_proposed" => {
                let name = ["name", "tool", "tool_name"]
                    .iter()
                    .find_map(|k| data[*k].as_str())
                    .unwrap_or("");
                r.errln(&format!("[{kind}] {}", short(name)));
            }
            "permission_required" => {
                let (decision, word) = match opts.approve {
                    Approve::Deny => ("deny", "denied"),
                    Approve::Once => ("once", "approved once"),
                };
                r.errln(&format!(
                    "[permission_required] {word} (--approve {decision})"
                ));
                send(ws, json!({"type": "approval", "decision": decision}))?;
            }
            "question_requested" | "plan_proposed" => {
                r.errln(&format!(
                    "[{kind}] cannot be answered from the CLI; interrupting the turn."
                ));
                send(ws, json!({"type": "interrupt"}))?;
            }
            "turn_accepted" => {
                r.flush_stream(opts.json);
                if crate::util::verbose() {
                    r.errln("Turn accepted (queued); polling for the answer.");
                }
                return poll(t, opts, r, id, baseline, deadline, tm);
            }
            "turn_end" | "turn_done" => {
                if !opts.json {
                    r.flush_stream(false);
                }
                if wrote_text {
                    r.out("\n");
                }
                return Ok(());
            }
            "interrupted" => return Err(AppError::new("The turn was interrupted.")),
            "error" | "input_rejected" => {
                if !opts.json {
                    r.flush_stream(false);
                }
                let why = ["error", "message"]
                    .iter()
                    .find_map(|k| shown[*k].as_str())
                    .unwrap_or("no details");
                return Err(AppError::new(format!(
                    "Turn failed ({kind}): {}",
                    short(&if opts.json {
                        why.to_string()
                    } else {
                        r.lead(why)
                    })
                )));
            }
            _ => {}
        }
    }
}

/// Poll the message log until the answer to our message is complete: an
/// assistant message exists after our user message and the log has stopped
/// growing for one more poll (a turn may emit several assistant messages).
fn poll(
    t: &Target,
    opts: &TurnOpts,
    r: &Redactor,
    id: &str,
    baseline: usize,
    deadline: Instant,
    tm: &mut Timings,
) -> Result<()> {
    let mut printed = 0; // json mode: new messages already emitted
    let mut last_len = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(AppError::new(format!(
                "Timed out after {}s waiting for the answer.",
                opts.timeout.as_secs()
            )));
        }
        std::thread::sleep(opts.poll_every.min(left));
        let h = http(t, opts, deadline, || {
            craft::history(t.client, t.base, t.bearer, id)
        })?;
        let all = h["messages"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let new = all.get(baseline..).unwrap_or_default();
        let Some(u) = new.iter().position(|m| m["role"] == "user") else {
            continue;
        };
        let after = &new[u + 1..];
        if opts.json {
            after.iter().skip(printed).for_each(|m| {
                let mut m = m.clone();
                r.lead_json(&mut m);
                r.emit("poll", "message", &m)
            });
            printed = after.len();
        }
        if let Some(n) = after
            .iter()
            .find(|m| m["role"] == "notice" && m["kind"] == "error")
        {
            return Err(AppError::new(format!(
                "Turn failed (notice): {}",
                short(&r.lead(msg_text(n)))
            )));
        }
        let answers: Vec<&Value> = after.iter().filter(|m| m["role"] == "assistant").collect();
        let stable = last_len == Some(after.len());
        last_len = Some(after.len());
        if answers.iter().any(|m| !msg_text(m).is_empty()) {
            tm.text(); // polled: resolution is one poll interval
        }
        if !answers.is_empty() && stable {
            if !opts.json {
                for m in answers {
                    r.out(&format!("{}\n", r.lead(msg_text(m))));
                }
            }
            return Ok(());
        }
    }
}
