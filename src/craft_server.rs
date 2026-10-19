//! Client for the Craft server's session routes (`/v1/sessions`) — the
//! "openworker" lane of `monocle craft session`. Auth is the plain monocle
//! access token as a bearer; no origin header. Responses stay `serde_json::Value`
//! (artifact and message shapes vary).
//!
//! Errors never echo the body: Craft answers `{"error":"..."}`, and that string
//! is shown only when it looks like a short snake_case code.

use serde_json::Value;

use crate::error::{AppError, Result};
use crate::jarvice_chats::is_valid_chat_id;
use crate::net::{Client, Resp};
use crate::util::percent_encode;

/// Craft server for the selected environment: `MONOCLE_CRAFT_URL` wins.
pub fn base_url(environment: &str) -> Result<String> {
    if let Ok(url) = std::env::var("MONOCLE_CRAFT_URL") {
        return Ok(url.trim_end_matches('/').to_string());
    }
    match environment {
        "stg" => Ok("https://craft-stg.monocle-ai.com".to_string()),
        "prod" | "prd" => Ok("https://craft.monocle-ai.com".to_string()),
        env => Err(AppError::new(format!(
            "Craft server URL unknown for env '{env}'; set MONOCLE_CRAFT_URL."
        ))),
    }
}

fn checked_id(id: &str) -> Result<&str> {
    if is_valid_chat_id(id) {
        Ok(id)
    } else {
        Err(AppError::new("Invalid session ID."))
    }
}

/// `what failed: HTTP <status>[ (code)].` — `code` only if `^[a-z_]{1,40}$`.
fn api_error(what: &str, resp: &Resp) -> AppError {
    let code = resp
        .json::<Value>()
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .filter(|c| {
            (1..=40).contains(&c.len()) && c.bytes().all(|b| b == b'_' || b.is_ascii_lowercase())
        });
    match code {
        Some(code) => AppError::new(format!("{what} failed: HTTP {} ({code}).", resp.status)),
        None => AppError::new(format!("{what} failed: HTTP {}.", resp.status)),
    }
}

fn check(what: &str, resp: Resp) -> Result<Resp> {
    if resp.ok() {
        Ok(resp)
    } else {
        Err(api_error(what, &resp))
    }
}

fn auth(bearer: &str) -> [(&str, &str); 1] {
    [("Authorization", bearer)]
}

/// `GET /v1/sessions[?workspace=PATH]`.
pub fn list_sessions(
    client: &Client,
    base: &str,
    bearer: &str,
    workspace: Option<&str>,
) -> Result<Vec<Value>> {
    let mut url = format!("{base}/v1/sessions");
    if let Some(w) = workspace {
        url.push_str(&format!("?workspace={}", percent_encode(w, false)));
    }
    let v: Value = check("Listing sessions", client.get(&url, &auth(bearer))?)?.json()?;
    Ok(v["sessions"].as_array().cloned().unwrap_or_default())
}

/// The server has no get-one route, so find the session in the list.
pub fn get_session(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    checked_id(id)?;
    list_sessions(client, base, bearer, None)?
        .into_iter()
        .find(|s| s["session_id"].as_str() == Some(id))
        .ok_or_else(|| AppError::new(format!("Session '{id}' was not found.")))
}

pub fn history(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/v1/sessions/{}/messages", checked_id(id)?);
    check("Reading session history", client.get(&url, &auth(bearer))?)?.json()
}

/// `GET .../artifacts`; 409 means the sandbox is parked or gone.
pub fn files(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/v1/sessions/{}/artifacts", checked_id(id)?);
    let resp = client.get(&url, &auth(bearer))?;
    if resp.status == 409 {
        return Err(AppError::new(
            "Session files unavailable (session parked or sandbox gone).",
        ));
    }
    check("Listing session files", resp)?.json()
}

pub fn download(
    client: &Client,
    base: &str,
    bearer: &str,
    id: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let url = format!(
        "{base}/v1/sessions/{}/artifacts/read?path={}&download=1",
        checked_id(id)?,
        percent_encode(path, false)
    );
    let resp = client.get_download(&url, &auth(bearer))?;
    if resp.status == 404 {
        return Err(AppError::new(format!(
            "File '{path}' was not found in session '{id}'."
        )));
    }
    Ok(check("Downloading file", resp)?.bytes().to_vec())
}

pub fn delete(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/v1/sessions/{}", checked_id(id)?);
    let resp = check("Deleting session", client.delete(&url, &auth(bearer))?)?;
    Ok(resp.json().unwrap_or(Value::Null))
}
