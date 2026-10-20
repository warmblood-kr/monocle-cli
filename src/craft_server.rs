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

const STG_HOST: &str = "craft-stg.monocle-ai.com";
const PRD_HOST: &str = "craft.monocle-ai.com";

fn env_host(environment: &str) -> Option<&'static str> {
    match environment {
        "stg" => Some(STG_HOST),
        "prod" | "prd" => Some(PRD_HOST),
        _ => None,
    }
}

/// `(scheme, host, port)` of an `http(s)://host[:port]/...` URL (no userinfo allowed).
fn url_parts(url: &str) -> Option<(&str, &str, Option<&str>)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let (host, tail) = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']')?,
        None => authority.split_at(authority.find(':').unwrap_or(authority.len())),
    };
    let port = tail.strip_prefix(':');
    Some((scheme, host, port))
}

/// Craft server for the selected environment. `MONOCLE_CRAFT_URL` overrides it,
/// but the bearer must never cross environments: a known craft host has to match
/// the selected env, and any other host must be http(s) loopback (tests/local dev).
pub fn base_url(environment: &str) -> Result<String> {
    let Ok(url) = std::env::var("MONOCLE_CRAFT_URL") else {
        return env_host(environment)
            .map(|h| format!("https://{h}"))
            .ok_or_else(|| {
                AppError::new(format!(
                    "Craft server URL unknown for env '{environment}'; set MONOCLE_CRAFT_URL."
                ))
            });
    };
    let url = url.trim_end_matches('/').to_string();
    let refuse = |why: &str| AppError::new(format!("Refusing MONOCLE_CRAFT_URL: {why}."));
    let (scheme, host, port) = url_parts(&url).ok_or_else(|| refuse("not a valid http(s) URL"))?;
    match host {
        STG_HOST | PRD_HOST => {
            if env_host(environment) != Some(host) {
                return Err(refuse(&format!(
                    "host {host} does not belong to env '{environment}'"
                )));
            }
            if scheme != "https" || !matches!(port, None | Some("443")) {
                return Err(refuse(
                    "known Craft hosts require https on the default port",
                ));
            }
        }
        "localhost" | "127.0.0.1" | "::1" if scheme == "http" || scheme == "https" => {}
        _ => {
            return Err(refuse(
                "only the env's Craft host or a loopback address is allowed",
            ))
        }
    }
    Ok(url)
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

/// `DELETE /v1/sessions/{id}`. `{"ok":false}` is a failure, and on HTTP 500 the
/// list is re-checked: a session that is gone anyway counts as deleted
/// (`"recovered": true` in the returned value), one still present is an error.
pub fn delete(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let url = format!("{base}/v1/sessions/{}", checked_id(id)?);
    let resp = client.delete(&url, &auth(bearer))?;
    if resp.status == 500 {
        let present = list_sessions(client, base, bearer, None)?
            .iter()
            .any(|v| v["session_id"].as_str() == Some(id));
        return if present {
            Err(AppError::new(
                "Deleting session failed: HTTP 500; the session is still present.",
            ))
        } else {
            Ok(serde_json::json!({"ok": true, "recovered": true}))
        };
    }
    let v: Value = check("Deleting session", resp)?
        .json()
        .unwrap_or(Value::Null);
    if v["ok"].as_bool() == Some(false) {
        return Err(AppError::new(
            "Deleting session failed: the server reported ok=false.",
        ));
    }
    Ok(v)
}

/// `GET /v1/workspaces/recent` → `[{path,name,exists}]`.
pub fn list_workspaces(client: &Client, base: &str, bearer: &str) -> Result<Vec<Value>> {
    let url = format!("{base}/v1/workspaces/recent");
    let v: Value = check("Listing workspaces", client.get(&url, &auth(bearer))?)?.json()?;
    Ok(v["workspaces"].as_array().cloned().unwrap_or_default())
}

/// `POST /v1/workspaces/open`; the response shape is unverified, so it is returned
/// as-is (`Null` if the body is not JSON). One attempt: opening may create a folder.
pub fn open_workspace(
    client: &Client,
    base: &str,
    bearer: &str,
    path: &str,
    create: bool,
) -> Result<Value> {
    let url = format!("{base}/v1/workspaces/open");
    let body = serde_json::json!({"path": path, "create": create});
    let resp = client.post_json_once(&url, &auth(bearer), &body)?;
    Ok(check("Opening workspace", resp)?
        .json()
        .unwrap_or(Value::Null))
}

/// `GET /v1/agents` → `[{name,title,needs_workspace,default,tagline}]`.
pub fn list_agents(client: &Client, base: &str, bearer: &str) -> Result<Vec<Value>> {
    let url = format!("{base}/v1/agents");
    let v: Value = check("Listing agents", client.get(&url, &auth(bearer))?)?.json()?;
    Ok(v["agents"].as_array().cloned().unwrap_or_default())
}

/// Craft host for sending the Stark id_token: exactly `https://<env host>` (default
/// port). Unlike [`base_url`], a `MONOCLE_CRAFT_URL` override is only tolerated if it
/// normalizes to that same host — loopback and every other host are refused, and
/// envs without a Craft host (local) are refused outright.
pub fn delegation_base(environment: &str) -> Result<String> {
    let only = || {
        AppError::new(
            "Delegation is only allowed against the Craft host of the selected environment (stg|prod).",
        )
    };
    let host = env_host(environment).ok_or_else(only)?;
    let allowed = format!("https://{host}");
    if std::env::var("MONOCLE_CRAFT_URL").is_ok() {
        let url = base_url(environment)?;
        if url != allowed && url != format!("{allowed}:443") {
            return Err(only());
        }
    }
    Ok(allowed)
}

/// `POST /v1/sessions/{id}/delegation` with the user's Stark `id_token`. Returns
/// `{"ok":true,"expires_at":N}`. The token only ever lives in the request body: it is
/// never formatted into an error, no redirect is followed with it, and a 3xx is an
/// error. Callers must pass a `base` from [`delegation_base`] (tests pass a stub).
pub fn attach_delegation(
    client: &Client,
    base: &str,
    bearer: &str,
    id: &str,
    id_token: &str,
) -> Result<Value> {
    let url = format!("{base}/v1/sessions/{}/delegation", checked_id(id)?);
    let body = serde_json::json!({"id_token": id_token});
    let resp = client.post_json_once_no_redirect(&url, &auth(bearer), &body)?;
    match resp.status {
        300..=399 => {
            return Err(AppError::new(
                "Attaching delegation failed: unexpected redirect (not followed).",
            ))
        }
        400 => {
            return Err(AppError::new(
                "Delegation refused (stale or rejected token); run `monocle login` again.",
            ))
        }
        404 => {
            return Err(AppError::new(format!(
                "Session '{id}' was not found (it may belong to another tenant)."
            )))
        }
        _ => {}
    }
    let v: Value = check("Attaching delegation", resp)?.json()?;
    if v["ok"].as_bool() != Some(true) {
        return Err(AppError::new(
            "Attaching delegation failed: the server did not report ok=true.",
        ));
    }
    Ok(v)
}
