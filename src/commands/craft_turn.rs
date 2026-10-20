//! `monocle craft turn TEXT` — drive one Craft turn over the session WebSocket.
//! Answer text goes to stdout, progress/notes to stderr, failures to the exit code.
//!
//! The server may run in "durable edge" mode where the socket is control-only: it
//! answers `turn_accepted` and engine events never stream. Then the answer is
//! polled from `GET /v1/sessions/{id}/messages` instead.

use std::io::Write;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use rand::RngCore;
use serde_json::{json, Value};

use crate::craft_server as craft;
use crate::error::{AppError, Result};
use crate::jarvice_chats::is_valid_chat_id;
use crate::net::Client;
use crate::util::percent_encode;
use crate::ws::{self, WsConn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Approve {
    Deny,
    Once,
}

pub struct TurnOpts {
    pub text: String,
    pub session: Option<String>,
    pub workspace: Option<String>,
    pub create_workspace: bool,
    pub agent: String,
    pub timeout: Duration,
    pub approve: Approve,
    pub json: bool,
    pub poll_every: Duration,
}

/// Where the turn talks. `ws_base` must come from [`craft::ws_base`].
pub struct Target<'a> {
    pub client: &'a Client,
    pub base: &'a str,
    pub ws_base: &'a str,
    pub bearer: &'a str,
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

/// The ONE output boundary: everything the command writes to stdout/stderr goes
/// through here, so a server echoing a secret (the access token) never reaches a
/// terminal or a pipe. Empty secrets are ignored.
///
/// Threat model: ACCIDENTAL reflection only (a server/proxy echoing headers or
/// tokens in errors/messages, logs, CI capture). A malicious Craft server already
/// holds the token; adversarial splitting is out of scope. So: a secret split
/// across deltas is caught by the hold-back; a split across a delta and a later
/// non-delta text is caught by `partial_redacted` (the held-back tail ending in a
/// secret prefix is redacted at flush, and the next text loses a leading run that
/// is a secret suffix of >= 8 chars). Accepted false positive: a tail that merely
/// ends in the secret's first char is redacted too.
pub struct Redactor {
    forms: Vec<String>,
    /// Hold-back tail of the assistant delta stream (see [`Redactor::delta`]).
    hold: std::cell::RefCell<String>,
    /// A flush redacted a possible secret prefix; the next text may start with its rest.
    partial_redacted: std::cell::Cell<bool>,
}

impl Redactor {
    pub fn new(secrets: &[&str]) -> Self {
        let mut all = Vec::new();
        for s in secrets.iter().filter(|s| !s.is_empty()) {
            all.push(s.to_string());
            // The form it takes inside a serialized JSON string.
            let esc = serde_json::to_string(s).unwrap_or_default();
            let esc = esc.trim_matches('"');
            if esc != *s {
                all.push(esc.to_string());
            }
        }
        Self {
            forms: all,
            hold: Default::default(),
            partial_redacted: Default::default(),
        }
    }

    pub fn scrub(&self, t: &str) -> String {
        self.forms
            .iter()
            .fold(t.to_string(), |t, s| t.replace(s, "[REDACTED]"))
    }

    /// Feed one assistant delta through the single scrubbed stream. A secret split
    /// across deltas can't leak: the last `longest form - 1` chars stay unemitted
    /// until more text (or [`Redactor::flush_stream`]) arrives. Returns what is safe to emit.
    fn delta(&self, t: &str) -> String {
        let mut b = self.hold.borrow_mut();
        b.push_str(t);
        let s = self.scrub(&b);
        let keep = self
            .forms
            .iter()
            .map(|f| f.chars().count())
            .max()
            .unwrap_or(0)
            .saturating_sub(1);
        let cut = s.chars().count().saturating_sub(keep);
        let at = s.char_indices().nth(cut).map_or(s.len(), |(i, _)| i);
        *b = s[at..].to_string();
        if at > 0 {
            self.partial_redacted.set(false); // unrelated text went out
        }
        s[..at].to_string()
    }

    /// Emit the held-back tail (scrubbed). Text mode: raw; `--json`: a synthetic delta frame.
    fn flush_stream(&self, json: bool) -> bool {
        let mut tail = self.scrub(&std::mem::take(&mut *self.hold.borrow_mut()));
        // Longest suffix of the tail that is a prefix of a secret form (char boundary).
        let cut = tail
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| self.forms.iter().any(|f| f.starts_with(&tail[i..])));
        if let Some(i) = cut {
            tail.truncate(i);
            tail.push_str("[REDACTED]");
            self.partial_redacted.set(true);
        }
        if tail.is_empty() {
            return false;
        }
        if json {
            let f = json!({"type": "assistant_delta", "data": {"text": tail}});
            self.emit("ws", "frame", &f);
        } else {
            self.out(&tail);
        }
        true
    }

    /// Non-delta text: if a partial secret was redacted, also drop a leading run of
    /// `t` that is a secret suffix (>= 8 chars, longest match). Clears the flag.
    fn lead(&self, t: &str) -> String {
        if !self.partial_redacted.replace(false) {
            return t.to_string();
        }
        let n = self
            .forms
            .iter()
            .flat_map(|f| f.char_indices().map(move |(i, _)| &f[i..]))
            .filter(|s| s.chars().count() >= 8 && t.starts_with(s))
            .map(str::len)
            .max();
        n.map_or(t.to_string(), |n| format!("[REDACTED]{}", &t[n..]))
    }

    /// `--json`: [`Redactor::lead`] on the text-ish string fields of an object.
    fn lead_json(&self, v: &mut Value) {
        for k in ["text", "message", "content", "error"] {
            if let Some(t) = v[k].as_str() {
                v[k] = json!(self.lead(t));
            }
        }
        self.partial_redacted.set(false);
    }

    fn out(&self, t: &str) {
        print!("{}", self.scrub(t));
        let _ = std::io::stdout().flush();
    }

    fn errln(&self, t: &str) {
        eprintln!("{}", self.scrub(t));
    }

    /// `--json` line: serialize, scrub the string, print.
    fn emit(&self, source: &str, key: &str, v: &Value) {
        self.out(&format!("{}\n", json!({"source": source, key: v})));
    }
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
    let deadline = Instant::now() + opts.timeout;
    let token = t.bearer.strip_prefix("Bearer ").unwrap_or(t.bearer);
    let r = Redactor::new(&[token]);
    let res = run_inner(t, opts, delegate, &r, deadline);
    t.client.set_request_cap(None);
    // Server-provided text can sit inside any error: scrub at the boundary.
    res.map_err(|e| AppError::new(r.scrub(&e.to_string())))
}

fn run_inner(
    t: &Target,
    opts: &TurnOpts,
    delegate: impl FnOnce(&str) -> Result<()>,
    r: &Redactor,
    deadline: Instant,
) -> Result<()> {
    let id = match &opts.session {
        Some(s) if is_valid_chat_id(s) => s.clone(),
        Some(_) => return Err(AppError::new("Invalid session ID.")),
        None => new_session_id(),
    };
    r.errln(&format!("Session: {id}"));
    http(t, opts, deadline, || delegate(&id))?;
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
    let mut conn = ws::connect(
        &url,
        &["openworker", token],
        left.min(Duration::from_secs(15)),
    )
    .map_err(|e| {
        if deadline.saturating_duration_since(Instant::now()) < Duration::from_millis(200) {
            timed_out(opts, "the turn")
        } else {
            e
        }
    })?;
    let res = drive(t, opts, r, &id, &mut conn, deadline);
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
                send(ws, json!({"type": "user_message", "text": opts.text}))?;
                sent = true;
            }
            "assistant_delta" => {
                streamed = true;
                if let (false, Some(t)) = (opts.json, data_text(data)) {
                    r.out(&r.delta(t));
                    wrote_text = true;
                }
            }
            "assistant_message" => {
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
                r.errln("Turn accepted (queued); polling for the answer.");
                return poll(t, opts, r, id, baseline, deadline);
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
