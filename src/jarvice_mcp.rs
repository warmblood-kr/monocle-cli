//! Client for jarvice's per-user MCP catalog endpoints under `/api/v1/mcp/servers`
//! (list / detail / attention / enable / disable / per-tool toggle) — the same
//! state the web "Apps" page edits. **jarvice-only**: tenant-host-routed
//! (`auth::jarvice_url_for`), never chat-proxy's `router_url`.
//!
//! Shapes verified against jarvice `devel` (`routers/mcp.py`):
//! - `GET /servers` -> `{"servers":[{id,name,enabled,auth_required,auth_satisfied,
//!   auth_failed,tools:[{name,description,enabled}]}]}` (custom servers list
//!   `tools: []`; the detail call does the live `tools/list`).
//! - `GET /servers/{id}` — `id` is the catalog name for marketplace servers but a
//!   uuid for tenant custom servers, so [`find_server`] maps name -> id first.
//! - `GET /servers/attention` -> `{"count": N}` (servers whose auth failed).
//! - `POST /servers/{name}/enable|disable` (no body) and
//!   `POST /servers/{name}/tools/{tool}` with `{"enabled": bool}` — the latter only
//!   flips a switch (404 when the server is not enabled); it never runs the tool.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{AppError, Result};
use crate::jarvice_knowledge::api_error;
use crate::net::Client;
use crate::origin::auth_headers;
use crate::util::percent_encode;

#[derive(Debug, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
pub struct Server {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default)]
    pub auth_satisfied: bool,
    #[serde(default)]
    pub auth_failed: bool,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

impl Server {
    pub fn auth_state(&self) -> &'static str {
        match (self.auth_required, self.auth_failed, self.auth_satisfied) {
            (false, _, _) => "none",
            (_, true, _) => "failed",
            (_, _, true) => "connected",
            _ => "needed",
        }
    }
}

fn get_value(client: &Client, url: &str, bearer: &str, what: &str) -> Result<Value> {
    let resp = client.get(url, &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error(what, &resp));
    }
    resp.json()
}

/// Raw `GET /servers` body (for `--json`).
pub fn list_servers_raw(client: &Client, jarvice_url: &str, bearer: &str) -> Result<Value> {
    get_value(
        client,
        &format!("{jarvice_url}/api/v1/mcp/servers"),
        bearer,
        "Listing MCP servers",
    )
}

/// Servers out of a list body: a bare array or `{"servers":[...]}`.
pub fn parse_servers(body: &Value) -> Result<Vec<Server>> {
    let list = body.get("servers").unwrap_or(body);
    serde_json::from_value(list.clone())
        .map_err(|_| AppError::new("Unexpected MCP server list from the server."))
}

/// Pick a server by exact name or id (the id is what the detail route needs).
pub fn find_server<'a>(all: &'a [Server], key: &str) -> Result<&'a Server> {
    all.iter()
        .find(|s| s.name == key || s.id == key)
        .ok_or_else(|| {
            AppError::new(format!(
                "MCP server '{key}' was not found (run `monocle mcp ls`)."
            ))
        })
}

/// Raw `GET /servers/{id}` body (for `--json`).
pub fn server_detail_raw(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    id: &str,
) -> Result<Value> {
    get_value(
        client,
        &format!(
            "{jarvice_url}/api/v1/mcp/servers/{}",
            percent_encode(id, false)
        ),
        bearer,
        "Fetching the MCP server",
    )
}

/// Raw `GET /servers/attention` body: `{"count": N}`.
pub fn attention_raw(client: &Client, jarvice_url: &str, bearer: &str) -> Result<Value> {
    get_value(
        client,
        &format!("{jarvice_url}/api/v1/mcp/servers/attention"),
        bearer,
        "Checking MCP servers",
    )
}

/// `POST /servers/{name}/enable|disable`, or with `tool` the per-tool switch.
pub fn set_enabled(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    server: &str,
    tool: Option<&str>,
    enabled: bool,
) -> Result<()> {
    let base = format!(
        "{jarvice_url}/api/v1/mcp/servers/{}",
        percent_encode(server, false)
    );
    let (url, body, what) = match tool {
        Some(t) => (
            format!("{base}/tools/{}", percent_encode(t, false)),
            json!({ "enabled": enabled }),
            "Updating the MCP tool",
        ),
        None => (
            format!("{base}/{}", if enabled { "enable" } else { "disable" }),
            json!({}),
            "Updating the MCP server",
        ),
    };
    let resp = client.post_json_once(&url, &auth_headers(bearer), &body)?;
    if !resp.ok() {
        return Err(api_error(what, &resp));
    }
    Ok(())
}
