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

fn emit(source: &str, key: &str, v: &Value) {
    println!("{}", json!({"source": source, key: v}));
}

fn out(t: &str) {
    print!("{t}");
    let _ = std::io::stdout().flush();
}

fn send(ws: &mut WsConn, v: Value) -> Result<()> {
    ws.send_text(&v.to_string())
}

/// Workspace to pass on the socket: the explicit one (opened first), else the
/// first recent one that still exists, else none (the server default applies).
fn resolve_workspace(t: &Target, opts: &TurnOpts) -> Result<Option<String>> {
    if let Some(path) = &opts.workspace {
        craft::open_workspace(t.client, t.base, t.bearer, path, opts.create_workspace)?;
        return Ok(Some(path.clone()));
    }
    match craft::list_workspaces(t.client, t.base, t.bearer) {
        Ok(all) => Ok(all
            .iter()
            .find(|w| w["exists"].as_bool() == Some(true))
            .and_then(|w| w["path"].as_str().map(str::to_string))),
        Err(_) => {
            eprintln!("Could not list recent workspaces; using the server default.");
            Ok(None)
        }
    }
}

/// Run one turn. `delegate` runs first (before any workspace call or socket).
pub fn run_turn(
    t: &Target,
    opts: &TurnOpts,
    delegate: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    let id = match &opts.session {
        Some(s) if is_valid_chat_id(s) => s.clone(),
        Some(_) => return Err(AppError::new("Invalid session ID.")),
        None => new_session_id(),
    };
    eprintln!("Session: {id}");
    delegate(&id)?;
    let workspace = resolve_workspace(t, opts)?;
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
    let deadline = Instant::now() + opts.timeout;
    let step = Duration::from_secs(15).min(opts.timeout);
    let mut conn = ws::connect(&url, &["openworker", token], step)?;
    let r = drive(t, opts, &id, &mut conn, deadline);
    conn.close();
    r
}

fn drive(t: &Target, opts: &TurnOpts, id: &str, ws: &mut WsConn, deadline: Instant) -> Result<()> {
    let timeout = || {
        AppError::new(format!(
            "Timed out after {}s waiting for the turn.",
            opts.timeout.as_secs()
        ))
    };
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
        if opts.json {
            emit("ws", "frame", &frame);
        }
        let kind = frame["type"].as_str().unwrap_or("");
        let data = &frame["data"];
        match kind {
            "ready" if !sent => {
                // Messages already in the session, so polling can tell new from old.
                baseline = craft::history(t.client, t.base, t.bearer, id)
                    .map(|h| h["messages"].as_array().map_or(0, Vec::len))
                    .unwrap_or(0);
                send(ws, json!({"type": "user_message", "text": opts.text}))?;
                sent = true;
            }
            "assistant_delta" => {
                streamed = true;
                if let (false, Some(t)) = (opts.json, data_text(data)) {
                    out(t);
                    wrote_text = true;
                }
            }
            "assistant_message" => {
                if !opts.json && !streamed {
                    if let Some(t) = data_text(data) {
                        out(t);
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
                eprintln!("[{kind}] {}", short(name));
            }
            "permission_required" => {
                let (decision, word) = match opts.approve {
                    Approve::Deny => ("deny", "denied"),
                    Approve::Once => ("once", "approved once"),
                };
                eprintln!("[permission_required] {word} (--approve {decision})");
                send(ws, json!({"type": "approval", "decision": decision}))?;
            }
            "question_requested" | "plan_proposed" => {
                eprintln!("[{kind}] cannot be answered from the CLI; interrupting the turn.");
                send(ws, json!({"type": "interrupt"}))?;
            }
            "turn_accepted" => {
                eprintln!("Turn accepted (queued); polling for the answer.");
                return poll(t, opts, id, baseline, deadline);
            }
            "turn_end" | "turn_done" => {
                if wrote_text {
                    out("\n");
                }
                return Ok(());
            }
            "interrupted" => return Err(AppError::new("The turn was interrupted.")),
            "error" | "input_rejected" => {
                let why = ["error", "message"]
                    .iter()
                    .find_map(|k| data[*k].as_str())
                    .unwrap_or("no details");
                return Err(AppError::new(format!(
                    "Turn failed ({kind}): {}",
                    short(why)
                )));
            }
            _ => {}
        }
    }
}

/// Poll the message log until the answer to our message is complete: an
/// assistant message exists after our user message and the log has stopped
/// growing for one more poll (a turn may emit several assistant messages).
fn poll(t: &Target, opts: &TurnOpts, id: &str, baseline: usize, deadline: Instant) -> Result<()> {
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
        let h = craft::history(t.client, t.base, t.bearer, id)?;
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
            after
                .iter()
                .skip(printed)
                .for_each(|m| emit("poll", "message", m));
            printed = after.len();
        }
        if let Some(n) = after
            .iter()
            .find(|m| m["role"] == "notice" && m["kind"] == "error")
        {
            return Err(AppError::new(format!(
                "Turn failed (notice): {}",
                short(msg_text(n))
            )));
        }
        let answers: Vec<&Value> = after.iter().filter(|m| m["role"] == "assistant").collect();
        let stable = last_len == Some(after.len());
        last_len = Some(after.len());
        if !answers.is_empty() && stable {
            if !opts.json {
                for m in answers {
                    println!("{}", msg_text(m));
                }
            }
            return Ok(());
        }
    }
}
