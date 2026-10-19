//! Client for jarvice's agent-session endpoints (`/api/v1/agent/sessions`) — the
//! "sdk" lane of `monocle craft session`. jarvice-only, tenant-host-routed like
//! `jarvice_knowledge`. Responses stay `serde_json::Value`: the history entry
//! shape is not pinned down, so nothing is invented here.
//!
//! Errors never echo the body: jarvice's `detail` is a plain (localized) string;
//! only the status and, for object details, the stable `detail.code` are shown.

use serde_json::Value;

use crate::error::{AppError, Result};
use crate::jarvice_chats::is_valid_chat_id;
use crate::jarvice_knowledge::api_error;
use crate::net::{Client, Resp};
use crate::origin::auth_headers;
use crate::util::percent_encode;

/// Session ids are spliced into URL paths: same guard as chat thread ids.
fn checked_id(id: &str) -> Result<&str> {
    if is_valid_chat_id(id) {
        Ok(id)
    } else {
        Err(AppError::new("Invalid session ID."))
    }
}

fn check(what: &str, id: &str, resp: Resp) -> Result<Resp> {
    match resp.status {
        _ if resp.ok() => Ok(resp),
        404 => Err(AppError::new(format!("Session '{id}' was not found."))),
        403 => Err(AppError::new(format!(
            "Session '{id}' belongs to another user."
        ))),
        _ => Err(api_error(what, &resp)),
    }
}

fn get(client: &Client, url: &str, bearer: &str, what: &str, id: &str) -> Result<Resp> {
    check(what, id, client.get(url, &auth_headers(bearer))?)
}

/// `GET /api/v1/agent/sessions?skip=0&limit=50` — a bare array of sessions.
pub fn list_sessions(client: &Client, base: &str, bearer: &str) -> Result<Vec<Value>> {
    let url = format!("{base}/api/v1/agent/sessions?skip=0&limit=50");
    get(client, &url, bearer, "Listing sessions", "")?.json()
}

pub fn get_session(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/api/v1/agent/sessions/{}", checked_id(id)?);
    get(client, &url, bearer, "Showing session", id)?.json()
}

pub fn history(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/api/v1/agent/sessions/{}/history", checked_id(id)?);
    get(client, &url, bearer, "Reading session history", id)?.json()
}

pub fn files(
    client: &Client,
    base: &str,
    bearer: &str,
    id: &str,
    folder: Option<&str>,
) -> Result<Value> {
    let mut url = format!("{base}/api/v1/agent/sessions/{}/files", checked_id(id)?);
    if let Some(f) = folder {
        url.push_str(&format!("?folder={}", percent_encode(f, false)));
    }
    get(client, &url, bearer, "Listing session files", id)?.json()
}

/// Raw bytes of one file; `path` is percent-encoded per segment.
pub fn download(
    client: &Client,
    base: &str,
    bearer: &str,
    id: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let url = format!(
        "{base}/api/v1/agent/sessions/{}/files/download/{}",
        checked_id(id)?,
        percent_encode(path.trim_start_matches('/'), true)
    );
    let resp = client.get_download(&url, &auth_headers(bearer))?;
    Ok(check("Downloading file", id, resp)?.bytes().to_vec())
}

pub fn delete(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/api/v1/agent/sessions/{}", checked_id(id)?);
    let resp = check(
        "Deleting session",
        id,
        client.delete(&url, &auth_headers(bearer))?,
    )?;
    Ok(resp.json().unwrap_or(Value::Null))
}
