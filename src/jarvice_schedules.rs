//! Client for jarvice's schedule endpoints under `/api/v1/schedules`
//! (`routers/schedules.py`, all `get_verified_user`; the path id is the schedule's
//! `public_id`). **jarvice-only**, like [`crate::jarvice_mcp`].
//!
//! Shapes verified against jarvice `devel`:
//! - `GET /` -> `ScheduleModel[]`; `GET /{id}` -> `ScheduleModel`.
//! - `POST /{id}/toggle` `{"enabled":bool}` -> `ScheduleModel`.
//! - `DELETE /{id}/delete` -> bare `bool` (false = nothing deleted).
//! - `GET /{id}/runs?limit=50` -> `ScheduleRunModel[]`, newest `scheduled_for` first
//!   (`models/schedules.py:671-682`).
//! - `GET|PUT /{id}/mcp-servers` -> `{"server_ids":[..],"ever_set":bool}`; PUT replaces the set.
//! - `POST /create` and `POST /{id}/update` with a `ScheduleForm` -> `ScheduleModel`
//!   (`routers/schedules.py:101-118`, `:157-170`; 403 for a `target_chat_id` the caller does
//!   not own, 400 when the insert/update fails).
//! - `POST /preview?count=5` with a `ScheduleForm` -> `int[]` of next-fire epochs.
//! - Every epoch is **seconds** (`int(time.time())`, `routers/schedules.py:127-136`).

use serde_json::{json, Value};

use crate::error::{AppError, Result};
use crate::jarvice_chats::is_valid_chat_id;
use crate::jarvice_knowledge::api_error;
use crate::net::Client;
use crate::origin::auth_headers;
use crate::util::percent_encode;

const BASE: &str = "/api/v1/schedules";

fn id_url(jarvice_url: &str, id: &str, suffix: &str) -> Result<String> {
    // public ids are UUIDv7 strings; same charset gate as chat ids.
    if !is_valid_chat_id(id) {
        return Err(AppError::new("Invalid schedule ID."));
    }
    Ok(format!(
        "{jarvice_url}{BASE}/{}{suffix}",
        percent_encode(id, false)
    ))
}

fn get_value(client: &Client, url: &str, bearer: &str, what: &str) -> Result<Value> {
    let resp = client.get(url, &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error(what, &resp));
    }
    resp.json()
}

pub fn list(client: &Client, url: &str, bearer: &str) -> Result<Value> {
    get_value(
        client,
        &format!("{url}{BASE}/"),
        bearer,
        "Listing schedules",
    )
}

pub fn show(client: &Client, url: &str, bearer: &str, id: &str) -> Result<Value> {
    get_value(
        client,
        &id_url(url, id, "")?,
        bearer,
        "Fetching the schedule",
    )
}

pub fn toggle(client: &Client, url: &str, bearer: &str, id: &str, enabled: bool) -> Result<Value> {
    let resp = client.post_json_once(
        &id_url(url, id, "/toggle")?,
        &auth_headers(bearer),
        &json!({ "enabled": enabled }),
    )?;
    if !resp.ok() {
        return Err(api_error("Updating the schedule", &resp));
    }
    resp.json()
}

/// Once-only; the server answers a bare bool and anything but `true` is a failure.
pub fn delete(client: &Client, url: &str, bearer: &str, id: &str) -> Result<()> {
    let resp = client.delete_once(&id_url(url, id, "/delete")?, &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error("Deleting the schedule", &resp));
    }
    match resp.json::<Value>() {
        Ok(Value::Bool(true)) => Ok(()),
        _ => Err(AppError::new("The server did not delete the schedule.")),
    }
}

pub fn runs(client: &Client, url: &str, bearer: &str, id: &str, limit: u32) -> Result<Value> {
    let u = id_url(url, id, &format!("/runs?limit={limit}"))?;
    get_value(client, &u, bearer, "Fetching the schedule runs")
}

/// `GET` (`servers = None`) or `PUT` (`Some(ids)`, replaces the WHOLE set).
pub fn mcp_servers(
    client: &Client,
    url: &str,
    bearer: &str,
    id: &str,
    servers: Option<&[String]>,
) -> Result<Value> {
    let u = id_url(url, id, "/mcp-servers")?;
    let Some(ids) = servers else {
        return get_value(client, &u, bearer, "Fetching the schedule's MCP servers");
    };
    let resp = client.put_json_once(&u, &auth_headers(bearer), &json!({ "server_ids": ids }))?;
    if !resp.ok() {
        return Err(api_error("Updating the schedule's MCP servers", &resp));
    }
    resp.json()
}

/// `POST /create` (once-only: a retry could duplicate the schedule).
pub fn create(client: &Client, url: &str, bearer: &str, form: &Value) -> Result<Value> {
    let resp =
        client.post_json_once(&format!("{url}{BASE}/create"), &auth_headers(bearer), form)?;
    if !resp.ok() {
        return Err(api_error("Creating the schedule", &resp));
    }
    resp.json()
}

/// `POST /{id}/update`; `form` is the FULL `ScheduleForm` (every field is overwritten).
pub fn update(client: &Client, url: &str, bearer: &str, id: &str, form: &Value) -> Result<Value> {
    let resp = client.post_json_once(&id_url(url, id, "/update")?, &auth_headers(bearer), form)?;
    if !resp.ok() {
        return Err(api_error("Updating the schedule", &resp));
    }
    resp.json()
}

/// `POST /preview?count=N`. `form` is a `ScheduleForm`; the server computes the
/// next fire epochs without storing anything.
pub fn preview(
    client: &Client,
    url: &str,
    bearer: &str,
    form: &Value,
    count: u32,
) -> Result<Value> {
    let resp = client.post_json_once(
        &format!("{url}{BASE}/preview?count={count}"),
        &auth_headers(bearer),
        form,
    )?;
    if !resp.ok() {
        return Err(api_error("Previewing the schedule", &resp));
    }
    resp.json()
}

/// What `runs --verify` can say about one run, from `tool_results_summary`.
#[derive(Debug, PartialEq)]
pub enum RunTools {
    /// `calls[].name` of every tool call the run made (may be empty).
    Calls(Vec<String>),
    Unknown(String),
}

/// Read a run's tool calls. The ONLY shape the code proves is
/// `ToolAssertionResult.to_summary()` (jarvice `utils/schedule_assertions.py:80-89`):
/// `{"satisfied","status","required","missing","failed","calls":[{"name","ok","error"}]}`,
/// stored by `schedule_runner.py:1263-1280` (via `finish_run`, `models/schedules.py:828`).
/// `calls` lists EVERY tool call of the turn (`:146-153`), required or not.
/// Other writers: `{"preflight_failures":[..]}` (`schedule_runner.py:1230`, the run
/// never reached the model) and `NULL` (paused/skipped/missed/unfinished runs);
/// neither says anything about tools, so both are `Unknown`, never "not fired".
pub fn run_tools(run: &Value) -> RunTools {
    let unknown = |m: &str| RunTools::Unknown(m.to_string());
    if run["finished_at"].is_null() {
        return unknown("the run has not finished yet");
    }
    let Some(sum) = run["tool_results_summary"].as_object() else {
        return unknown("the run has no tool_results_summary (paused, skipped, missed or never reached the model)");
    };
    let Some(calls) = sum.get("calls").and_then(Value::as_array) else {
        return unknown("tool_results_summary has no `calls` list (e.g. a pre-flight failure)");
    };
    let names: Option<Vec<String>> = calls
        .iter()
        .map(|c| {
            c["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(String::from)
        })
        .collect();
    match names {
        Some(n) => RunTools::Calls(n),
        None => unknown("a `calls` entry has no tool name (unrecognized shape)"),
    }
}
