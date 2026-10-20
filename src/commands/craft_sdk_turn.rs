//! `monocle craft --lane sdk turn TEXT` — one turn on a jarvice agent session over
//! Socket.IO `/agent`. Answer text goes to stdout, progress to stderr, failures to
//! the exit code. The socket token (a credential) lives in memory only: fetched,
//! sent as the connect auth to the tenant's own `/ws/socket.io/`, then dropped.

use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{AppError, Result};
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

fn out(t: &str) {
    print!("{t}");
    let _ = std::io::stdout().flush();
}

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

pub fn run(t: &SdkTurn) -> Result<()> {
    // Vet the target before any request: nothing leaves the machine for a bad host.
    let origin = ws_origin(t.base)?;
    let id = match t.session {
        Some(s) if is_valid_chat_id(s) => s.to_string(),
        Some(_) => return Err(AppError::new("Invalid session ID.")),
        None => sdk::create_session(t.client, t.base, t.bearer, t.title.unwrap_or("CLI turn"))?,
    };
    eprintln!("Session: {id}");
    let deadline = Instant::now() + t.timeout;
    let step = Duration::from_secs(15).min(t.timeout);
    let mut sock = {
        let token = sdk::socket_token(t.client, t.base, t.bearer)?;
        SocketIo::connect(&origin, &token, step)? // token dropped here
    };
    let r = drive(t, &id, &mut sock, deadline);
    if matches!(&r, Err(e) if e.to_string().starts_with("Timed out")) {
        let _ = sock.emit("interrupt", &json!({"session_id": id}));
    }
    sock.close();
    r
}

fn drive(t: &SdkTurn, id: &str, sock: &mut SocketIo, deadline: Instant) -> Result<()> {
    sock.emit("start_session", &json!({"session_id": id}))?;
    let (mut sent, mut deltas, mut wrote) = (false, false, false);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(AppError::new(format!(
                "Timed out after {}s waiting for the turn.",
                t.timeout.as_secs()
            )));
        }
        let Some((name, data)) = sock.next_event(left.min(Duration::from_secs(5)))? else {
            continue;
        };
        if data["session_id"].as_str().is_some_and(|s| s != id) {
            continue;
        }
        if t.json {
            println!("{}", json!({"event": name, "data": data}));
        }
        let content = data["content"].as_str().unwrap_or("");
        match name.as_str() {
            "session_status" => {
                let status = data["status"].as_str().unwrap_or("");
                if status == "active" {
                    if !sent {
                        sock.emit("message", &json!({"session_id": id, "content": t.text}))?;
                        sent = true;
                    }
                } else {
                    eprintln!("[session_status] {}", short(&data["status"]));
                }
            }
            "text_delta" => {
                deltas = true;
                if !t.json {
                    out(content);
                    wrote = true;
                }
            }
            "text" if !deltas => {
                if !t.json {
                    out(content);
                    wrote = true;
                }
            }
            "tool_use" => eprintln!(
                "[tool_use] {} {}",
                short(&data["tool_name"]),
                short(&data["tool_input"])
            ),
            "tool_result" => eprintln!("[tool_result]"),
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
                if wrote {
                    out("\n");
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
