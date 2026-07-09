//! `monocle mcp ls|connect|exec` — manage and call MCP (Model Context Protocol)
//! servers registered against the tenant's jarvice instance.
//!
//! All three subcommands reuse the CLI's existing login bearer token: jarvice's
//! `GET /api/v1/mcp/servers` (catalog + per-user enable/auth state) and
//! `/api/v1/connectors/*` (OAuth connect + status) accept it as-is (confirmed by
//! the Phase 0 server-side spike — stark mints the same `aud`/`monocle_token_type`
//! for every client). `exec` instead calls monocle-tool-server's own JSON-RPC MCP
//! endpoint (`POST {mcp_base}/{name}/mcp`), which locally shares the same host as
//! `router_url` (compose sets both to the same URL); see `mcp_base_url` below.
//!
//! v1 scope: `exec` only supports non-remote (tool-server-hosted) catalog
//! entries — a `remote` field on the entry means the tool lives behind a
//! third-party's own MCP endpoint with a different (two-hop) call shape, tracked
//! as a fast-follow (monocle-cli#69).

use std::io::Write;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::get_access_token;
use crate::colors as c;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::{Client, Resp};
use crate::origin::auth_headers;
use crate::util::{form_urlencode, pad};

const CONNECT_POLL_INTERVAL_SECS: u64 = 2;
const CONNECT_POLL_TIMEOUT_SECS: u64 = 300;
/// Bound on a single `exec` tool call. `call_tool` uses the streaming request
/// path (to accept either a plain-JSON or one-event SSE envelope), which by
/// default carries no total timeout (see `net.rs::Client::post_json_stream`,
/// tuned for open-ended LLM generations) — inappropriate here, since an MCP
/// tool call is a bounded request/response, not a stream. Matches `net.rs`'s
/// existing non-streaming default (`send()`'s 120s) so a stuck tool fails
/// predictably instead of hanging `monocle mcp exec` forever.
const EXEC_TIMEOUT_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Catalog model — verified against jarvice + monocle-tool-server source
// (Phase 1 spike, `../skills`-adjacent scratch clones of both repos on
// `devel`): jarvice's `routers/mcp.py::list_mcp_servers` (lines 99-107) builds
// each response entry as `{**server, "enabled", "auth_required",
// "auth_satisfied", "tools"}` — i.e. it spreads the raw catalog dict from
// monocle-tool-server's OpenAPI `x-mcp-servers` extension verbatim and only
// *adds* the four computed fields, so every other key (`auth`, `remote`,
// `description`, ...) is exactly whatever `servers/{name}/catalog.json` +
// `scripts/manifest_loader.py` produced. Every field below is still optional/
// defaulted — unknown extra fields are silently ignored by serde — but the
// shapes are now grounded, not guessed.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct McpServerEntry {
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    auth_required: bool,
    #[serde(default)]
    auth_satisfied: bool,
    #[serde(default)]
    tools: Vec<McpToolInfo>,
    /// CONFIRMED (monocle-tool-server `scripts/manifest_loader.py`, the
    /// `load_all_manifests` loop building each catalog `entry`: `entry.update({k:
    /// v for k, v in catalog.items() if k != "envs"})`): `remote` is copied
    /// straight from `catalog.json` and is present *only* for servers that
    /// declare it there. Of the seven real catalog.json files in this repo
    /// (`servers/{github,dart,image,iros,ms365,model_designer,web}`), only
    /// `github` has a `remote` key at all (`{"url": ..., "credential": ...}`);
    /// the other six omit the key entirely — never `"remote": false` or
    /// `"remote": null`. serde's `Option<T>` already maps a missing JSON key to
    /// `None` with no `#[serde(default)]` needed, so the original
    /// `entry.remote.is_some()` check in `mcp_exec_command` was already
    /// correct — this citation replaces what was an unconfirmed guess.
    remote: Option<Value>,
    auth: Option<McpAuthInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpToolInfo {
    name: String,
}

/// CONFIRMED nested (not flat `auth_type: "..."` fields): jarvice
/// `utils/mcp_client.py`'s `OAuth2AuthMeta`/`ApiKeyAuthMeta` pydantic models
/// (lines 83-121) are the typed shape of a catalog `auth` block — always an
/// object with `type` (`"oauth2"` | `"api_key"`) and `provider`, plus
/// oauth2-only `authorization_endpoint`/`token_endpoint`/`scopes`. Real
/// examples: `monocle-tool-server/servers/github/catalog.json` and
/// `servers/ms365/catalog.json` both declare `"auth": {"type": "oauth2",
/// "provider": ..., ...}` as a nested object. `routers/mcp.py`'s `**server`
/// spread (line 101) passes this through to the CLI unchanged, so the
/// original nested-object guess was correct — only `provider` was missing
/// from this model, which is needed to resolve the catalog-name-vs-provider
/// divergence (see `effective_provider` below).
#[derive(Debug, Clone, Deserialize)]
struct McpAuthInfo {
    #[serde(rename = "type")]
    auth_type: Option<String>,
    provider: Option<String>,
}

/// The catalog list call's root can plausibly be a bare JSON array or an object
/// wrapping one (`{"servers": [...]}`) — accept either.
#[derive(Deserialize)]
#[serde(untagged)]
enum ServersResponse {
    List(Vec<McpServerEntry>),
    Wrapped { servers: Vec<McpServerEntry> },
}

fn parse_servers(body: &str) -> Result<Vec<McpServerEntry>> {
    let parsed: ServersResponse = serde_json::from_str(body)
        .map_err(|e| AppError::new(format!("Failed to parse MCP servers response: {e}")))?;
    Ok(match parsed {
        ServersResponse::List(v) => v,
        ServersResponse::Wrapped { servers } => servers,
    })
}

/// GET `MCP_SERVERS` and check the response status — the one place this
/// request/error-handling logic lives. Returns the raw [`Resp`] (not the
/// parsed entries) so callers that also need the exact response body (e.g.
/// `mcp_ls_command`'s `--json` passthrough) don't have to re-fetch; `Resp::text`
/// is non-consuming, so it can be read here for parsing and again by the caller.
fn fetch_servers_response(client: &Client, router_url: &str, bearer: &str) -> Result<Resp> {
    let resp = client.get(
        &format!("{router_url}{}", endpoints::MCP_SERVERS),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "API error {}: {}",
            resp.status,
            resp.text()
        )));
    }
    Ok(resp)
}

fn fetch_servers(client: &Client, router_url: &str, bearer: &str) -> Result<Vec<McpServerEntry>> {
    let resp = fetch_servers_response(client, router_url, bearer)?;
    parse_servers(&resp.text())
}

fn find_entry<'a>(servers: &'a [McpServerEntry], name: &str) -> Option<&'a McpServerEntry> {
    servers.iter().find(|s| s.name == name)
}

// ---------------------------------------------------------------------------
// `monocle mcp ls`
// ---------------------------------------------------------------------------

fn yesno(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn print_servers_table(servers: &[McpServerEntry]) {
    if servers.is_empty() {
        eprintln!("No MCP servers found.");
        return;
    }

    let rows: Vec<(
        String,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        String,
    )> = servers
        .iter()
        .map(|s| {
            let tools = if s.tools.is_empty() {
                "-".to_string()
            } else {
                s.tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            (
                s.name.clone(),
                yesno(s.enabled),
                yesno(s.auth_required),
                yesno(s.auth_satisfied),
                yesno(s.remote.is_some()),
                tools,
            )
        })
        .collect();

    let name_w = rows
        .iter()
        .map(|r| r.0.chars().count())
        .chain([4])
        .max()
        .unwrap();
    let enabled_w = "ENABLED".len();
    let req_w = "AUTH REQ".len();
    let ok_w = "AUTH OK".len();
    let remote_w = "REMOTE".len();

    let mut out = std::io::stdout();
    let _ = writeln!(
        out,
        "{}  {}  {}  {}  {}  TOOLS",
        pad("NAME", name_w),
        pad("ENABLED", enabled_w),
        pad("AUTH REQ", req_w),
        pad("AUTH OK", ok_w),
        pad("REMOTE", remote_w),
    );
    let _ = writeln!(
        out,
        "{}  {}  {}  {}  {}  {}",
        "─".repeat(name_w),
        "─".repeat(enabled_w),
        "─".repeat(req_w),
        "─".repeat(ok_w),
        "─".repeat(remote_w),
        "─".repeat(5),
    );
    for r in &rows {
        let _ = writeln!(
            out,
            "{}  {}  {}  {}  {}  {}",
            pad(&r.0, name_w),
            pad(r.1, enabled_w),
            pad(r.2, req_w),
            pad(r.3, ok_w),
            pad(r.4, remote_w),
            r.5,
        );
    }

    eprintln!("\n{} MCP server(s).", servers.len());
}

pub fn mcp_ls_command(client: &Client, creds: &Credentials, json_out: bool) -> Result<()> {
    let session = get_access_token(client, creds);
    let bearer = format!("Bearer {}", session.token);

    let resp = fetch_servers_response(client, &session.router_url, &bearer)?;

    if json_out {
        println!("{}", resp.text());
        return Ok(());
    }

    let servers = parse_servers(&resp.text())?;
    print_servers_table(&servers);
    Ok(())
}

// ---------------------------------------------------------------------------
// `monocle mcp connect <name>`
// ---------------------------------------------------------------------------

enum ConnectKind {
    /// No `auth` block at all — just enable the server.
    NoAuth,
    /// `auth.type == "api_key"` — prompt for a key on stdin, no browser.
    ApiKey,
    /// Anything else with an `auth` block — OAuth authorization-code flow.
    OAuth,
}

fn connect_kind(entry: &McpServerEntry) -> ConnectKind {
    match &entry.auth {
        None => ConnectKind::NoAuth,
        Some(a) if a.auth_type.as_deref() == Some("api_key") => ConnectKind::ApiKey,
        Some(_) => ConnectKind::OAuth,
    }
}

/// The OAuth/API-key provider identifier for `entry`: `auth.provider` if the
/// catalog declared one explicitly, else the catalog `name` — mirroring
/// jarvice's own fallback (`utils/mcp_client.py::parse_auth`, lines 146-168:
/// "provider may be omitted in the catalog and defaults to the server name...
/// entries where they differ (e.g. ms365 / microsoft) still declare provider
/// explicitly").
///
/// This MUST be used — not `entry.name` — for anything that reads or writes
/// jarvice's connector state, because `UserConnectorsTable` (jarvice
/// `models/users.py`) keys every connector row by `provider`
/// (`get_status_for_user`, lines 653-663: `{r.provider: {...} for r in rows}`),
/// and `POST /api/v1/connectors/{provider}/token` (`routers/connectors.py`
/// line 455: `@router.post("/{provider}/token")`) uses its path param
/// directly with no catalog lookup at all. `servers/ms365/catalog.json` is a
/// real, confirmed case where the catalog `name` ("ms365") and `auth.provider`
/// ("microsoft") diverge — using `name` there would silently write/read the
/// wrong connector row.
fn effective_provider(entry: &McpServerEntry) -> &str {
    entry
        .auth
        .as_ref()
        .and_then(|a| a.provider.as_deref())
        .unwrap_or(entry.name.as_str())
}

/// Whether `entry` is already fully connected: enabled, and — if it declares
/// `auth_required` — its auth is already satisfied. Used to short-circuit
/// `mcp_connect_command` so re-running `connect` on an already-connected server
/// doesn't re-trigger the enable/api-key/OAuth dispatch (a redundant browser
/// pop or key re-prompt).
fn already_connected(entry: &McpServerEntry) -> bool {
    entry.enabled && (!entry.auth_required || entry.auth_satisfied)
}

pub fn mcp_connect_command(client: &Client, creds: &Credentials, name: &str) -> Result<()> {
    let session = get_access_token(client, creds);
    let bearer = format!("Bearer {}", session.token);
    let router_url = session.router_url.clone();

    let servers = fetch_servers(client, &router_url, &bearer)?;
    let entry = find_entry(&servers, name).ok_or_else(|| {
        AppError::new(format!(
            "Unknown MCP server '{name}'. Run `monocle mcp ls` to see available servers."
        ))
    })?;

    if already_connected(entry) {
        eprintln!("'{name}' is already connected.");
        return Ok(());
    }

    let provider = effective_provider(entry);
    match connect_kind(entry) {
        ConnectKind::NoAuth => enable_server(client, &router_url, &bearer, name),
        ConnectKind::ApiKey => connect_api_key(client, &router_url, &bearer, name, provider, entry),
        ConnectKind::OAuth => connect_oauth(client, &router_url, &bearer, name, provider),
    }
}

fn enable_server(client: &Client, router_url: &str, bearer: &str, name: &str) -> Result<()> {
    let path = endpoints::MCP_SERVER_ENABLE.replace("{name}", name);
    let resp = client.post_json(
        &format!("{router_url}{path}"),
        &auth_headers(bearer),
        &json!({}),
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Failed to enable '{name}' (HTTP {}): {}",
            resp.status,
            resp.text()
        )));
    }
    eprintln!("{} '{name}' enabled.", c::green("✓"));
    Ok(())
}

fn connect_api_key(
    client: &Client,
    router_url: &str,
    bearer: &str,
    name: &str,
    provider: &str,
    entry: &McpServerEntry,
) -> Result<()> {
    eprint!("Enter API key for '{name}': ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| AppError::new(format!("Failed to read API key: {e}")))?;
    let key = line.trim();
    if key.is_empty() {
        return Err(AppError::new("No API key provided."));
    }

    // CONFIRMED wire shape (jarvice `routers/connectors.py`): `ManualTokenBody`
    // (lines 449-452) declares `access_token` (not `token`) plus `auth_type` and
    // an optional `enable_server`; `set_manual_token` (lines 455-484) persists
    // via `UserConnectors.upsert(provider=provider, auth_type=body.auth_type,
    // secret=body.access_token, ...)` and, when `enable_server` is set, also
    // enables that MCP server in the same request — mirroring the OAuth
    // callback's auto-enable convenience (`provider_callback`, lines 357-381).
    // The path param is literally named `provider` (line 455: `@router.post(
    // "/{provider}/token")`) and is used with NO catalog lookup at all, so it
    // must be the resolved provider identifier (`effective_provider`), not the
    // catalog server `name` — they diverge for real entries like ms365/microsoft.
    let auth_type = entry
        .auth
        .as_ref()
        .and_then(|a| a.auth_type.as_deref())
        .unwrap_or("api_key");
    let path = endpoints::CONNECTOR_TOKEN.replace("{name}", provider);
    let body = json!({
        "access_token": key,
        "auth_type": auth_type,
        "enable_server": name,
    });
    let resp = client.post_json(&format!("{router_url}{path}"), &auth_headers(bearer), &body)?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Failed to save API key for '{name}' (HTTP {}): {}",
            resp.status,
            resp.text()
        )));
    }
    eprintln!("{} '{name}' connected.", c::green("✓"));
    Ok(())
}

fn connect_oauth(
    client: &Client,
    router_url: &str,
    bearer: &str,
    name: &str,
    provider: &str,
) -> Result<()> {
    // CONFIRMED: unlike the `/token` route, `GET /{server_name}/connect`
    // (jarvice `routers/connectors.py`, lines 188-203) takes the CATALOG name
    // (`_lookup_server` matches `s.get("name") == server_name`, lines 142-151)
    // and resolves `auth.provider` internally — so `name` (not `provider`) is
    // the right identifier here. Only the status poll below needs `provider`.
    let path = endpoints::CONNECTOR_CONNECT.replace("{name}", name);
    let query = form_urlencode(&[("enable_server", name)]);
    let url = format!("{router_url}{path}?{query}");

    let resp = client.get(&url, &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Failed to start OAuth connect for '{name}' (HTTP {}): {}",
            resp.status,
            resp.text()
        )));
    }

    #[derive(Deserialize)]
    struct ConnectResp {
        authorization_url: String,
    }
    let body: ConnectResp = resp.json()?;

    eprintln!("Opening browser for '{name}' authorization...");
    eprintln!(
        "If the browser doesn't open, visit: {}",
        body.authorization_url
    );
    if let Err(e) = open::that(&body.authorization_url) {
        eprintln!(
            "{}",
            c::yellow(&format!(
                "Warning: failed to open browser automatically: {e}"
            ))
        );
    }

    poll_connector_status(client, router_url, bearer, name, provider)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ConnectorStatusResponse {
    List(Vec<ConnectorStatusEntry>),
    Map(std::collections::HashMap<String, Value>),
}

#[derive(Deserialize)]
struct ConnectorStatusEntry {
    #[serde(alias = "provider", alias = "name")]
    provider: String,
    #[serde(default)]
    connected: bool,
}

/// Whether `provider` shows `connected: true` in a `/api/v1/connectors/status`
/// response body.
///
/// CONFIRMED real shape (jarvice `models/users.py::UserConnectorsTable
/// .get_status_for_user`, lines 653-663): always a map **keyed by `provider`**
/// (`{r.provider: {"connected": ..., "scopes": ..., "connected_at": ...} for r
/// in rows}`) — never a list, and never a bare bool value. The `List` variant
/// and bare-`bool` map-value case below are kept only as defensive leniency
/// (harmless if the shape ever changes); the `Map` + object-value branch is
/// the one that matches reality. Critically, the map key is the OAuth
/// **provider** (e.g. `"microsoft"`), not the catalog server name (`"ms365"`)
/// — callers must pass `effective_provider(entry)`, not `entry.name`. An
/// unparseable/absent body is treated as "not yet connected" so the poll loop
/// just keeps waiting until the timeout.
fn provider_connected(body: &str, provider: &str) -> bool {
    match serde_json::from_str::<ConnectorStatusResponse>(body) {
        Ok(ConnectorStatusResponse::List(entries)) => entries
            .iter()
            .find(|e| e.provider == provider)
            .map(|e| e.connected)
            .unwrap_or(false),
        Ok(ConnectorStatusResponse::Map(map)) => match map.get(provider) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(obj)) => obj
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            _ => false,
        },
        Err(_) => false,
    }
}

/// Poll `GET /api/v1/connectors/status` every `CONNECT_POLL_INTERVAL_SECS` until
/// `name` shows connected, mirroring `login.rs::poll_for_token`'s shape (bounded
/// loop, `std::thread::sleep` between attempts) and its implicit Ctrl+C handling
/// — no custom signal handler, the default SIGINT behavior (process exit) is
/// exactly what a stuck poll should do.
fn poll_connector_status(
    client: &Client,
    router_url: &str,
    bearer: &str,
    name: &str,
    provider: &str,
) -> Result<()> {
    eprintln!("Waiting for '{name}' authorization to complete...");
    let deadline = Instant::now() + Duration::from_secs(CONNECT_POLL_TIMEOUT_SECS);

    loop {
        if Instant::now() >= deadline {
            return Err(AppError::new(format!(
                "Timed out waiting for '{name}' to connect after {CONNECT_POLL_TIMEOUT_SECS}s."
            )));
        }
        std::thread::sleep(Duration::from_secs(CONNECT_POLL_INTERVAL_SECS));

        let resp = client.get(
            &format!("{router_url}{}", endpoints::CONNECTOR_STATUS),
            &auth_headers(bearer),
        )?;
        if !resp.ok() {
            if is_transient_status(resp.status) {
                continue; // 5xx / 429 — transient, keep polling until the deadline
            }
            return Err(AppError::new(format!(
                "Failed to check '{name}' connection status (HTTP {}): {}",
                resp.status,
                resp.text()
            )));
        }
        // Looked up by `provider` (e.g. "microsoft"), not the catalog `name`
        // (e.g. "ms365") — see `provider_connected`'s doc comment.
        if provider_connected(&resp.text(), provider) {
            eprintln!("{} '{name}' connected.", c::green("✓"));
            return Ok(());
        }
    }
}

/// Whether a non-2xx status from the connector-status poll is worth retrying,
/// mirroring `login.rs::poll_for_token`'s distinction between "keep waiting"
/// and "fail now": 5xx (server-side hiccup) and 429 (rate-limited) are
/// transient; everything else (401/403/404/...) is a terminal client/auth
/// error — silently retrying one of those for the full
/// `CONNECT_POLL_TIMEOUT_SECS` would just swallow a real failure (e.g. an
/// expired session or an unknown connector name) behind a timeout message.
fn is_transient_status(status: u16) -> bool {
    status >= 500 || status == 429
}

// ---------------------------------------------------------------------------
// `monocle mcp exec <name> <tool> [--arg key=value ...]`
// ---------------------------------------------------------------------------

/// Parse repeated `--arg key=value` flags into a JSON object. Values are kept as
/// JSON strings (no type inference) — the simplest thing that works for a v1
/// tool-argument escape hatch.
fn parse_arg_pairs(pairs: &[String]) -> Result<Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        match pair.split_once('=') {
            Some(("", _)) => {
                return Err(AppError::new(format!(
                    "Invalid --arg '{pair}': key must not be empty"
                )));
            }
            Some((k, _)) if map.contains_key(k) => {
                // Erroring (not warning) is the safer default: a repeated
                // `--arg` key is almost certainly a scripting mistake, not
                // intentional, and there's no existing convention in this
                // codebase for silently-overwritten repeated flags.
                return Err(AppError::new(format!(
                    "Invalid --arg '{pair}': duplicate key '{k}'"
                )));
            }
            Some((k, v)) => {
                map.insert(k.to_string(), Value::String(v.to_string()));
            }
            None => {
                return Err(AppError::new(format!(
                    "Invalid --arg '{pair}': expected key=value"
                )));
            }
        }
    }
    Ok(Value::Object(map))
}

/// A 32-hex-char random request id (no `uuid` crate in the dependency tree;
/// mirrors `oidc.rs::generate_state`'s `rand::thread_rng().fill_bytes` idiom).
fn request_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn build_tools_call_body(id: &str, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments,
        }
    })
}

/// `exec`'s `mcp_base` resolution: `MONOCLE_MCP_BASE_URL` escape hatch first
/// (local dev and any environment can override), else `router_url` — which
/// works for local dev today since compose points both the public and internal
/// MCP hosts at the same URL. Staging/prod parity lands with a stark-side
/// discovery field (follow-up issue); this is the two-way door until then, per
/// the plan: no speculative handling beyond the env var.
fn mcp_base_url(router_url: &str) -> String {
    std::env::var("MONOCLE_MCP_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| router_url.to_string())
}

/// POST the JSON-RPC `tools/call` body to `{mcp_base}/{name}/mcp` and return the
/// parsed envelope. Dual-mode response handling (plain JSON or one `data:` SSE
/// event carrying the JSON-RPC message) mirrors the existing convention in
/// `agent/providers.rs::chat_stream` / `net.rs::EventStream`.
fn call_tool(
    client: &Client,
    mcp_base: &str,
    name: &str,
    bearer: &str,
    body: &Value,
) -> Result<Value> {
    let url = format!("{mcp_base}/{name}/mcp");
    let headers = [
        ("Authorization", bearer),
        ("Accept", "application/json, text/event-stream"),
    ];

    let mut stream = client.post_json_stream_with_timeout(
        &url,
        &headers,
        body,
        Duration::from_secs(EXEC_TIMEOUT_SECS),
    )?;
    if !stream.ok() {
        let status = stream.status;
        let text = stream.read_all();
        return Err(AppError::new(format!("HTTP error {status}: {text}")));
    }

    if !stream.is_event_stream() {
        let text = stream.read_all();
        return serde_json::from_str(&text)
            .map_err(|e| AppError::new(format!("Invalid JSON-RPC response: {e}")));
    }

    loop {
        match stream.next_line()? {
            Some(line) => {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        return Ok(v);
                    }
                }
            }
            None => {
                return Err(AppError::new(
                    "Connection closed before a JSON-RPC response was received.",
                ));
            }
        }
    }
}

/// Print the JSON-RPC envelope verbatim to **stdout** (the eval payoff — the
/// user needs to see exactly what the provider/tool returned, success or
/// JSON-RPC `error`), and exit non-zero only for the `error` case. This is
/// deliberately not a normal `Err` return: an `Err` would print an `Error: ...`
/// prefix to stderr via `main`'s generic path, but a JSON-RPC error is the
/// substantive answer, not a CLI failure.
fn print_result_and_exit(envelope: &Value) -> Result<()> {
    let pretty = serde_json::to_string_pretty(envelope).unwrap_or_else(|_| envelope.to_string());
    println!("{pretty}");
    if envelope.get("error").is_some() {
        std::process::exit(1);
    }
    Ok(())
}

pub fn mcp_exec_command(
    client: &Client,
    creds: &Credentials,
    name: &str,
    tool: &str,
    raw_args: &[String],
) -> Result<()> {
    let session = get_access_token(client, creds);
    let bearer = format!("Bearer {}", session.token);

    let servers = fetch_servers(client, &session.router_url, &bearer)?;
    let entry = find_entry(&servers, name).ok_or_else(|| {
        AppError::new(format!(
            "Unknown MCP server '{name}'. Run `monocle mcp ls` to see available servers."
        ))
    })?;
    if entry.remote.is_some() {
        return Err(AppError::new(format!(
            "'{name}' is a remote MCP server; remote MCP servers not yet supported, see monocle-cli#69."
        )));
    }

    let arguments = parse_arg_pairs(raw_args)?;
    let mcp_base = mcp_base_url(&session.router_url);
    let id = request_id();
    let body = build_tools_call_body(&id, tool, arguments);

    eprintln!("Calling '{tool}' on '{name}'...");
    let envelope = call_tool(client, &mcp_base, name, &bearer, &body)?;
    print_result_and_exit(&envelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Ground-truth fixtures — built from jarvice + monocle-tool-server's
    // actual source (Phase 1 spike: scratch clones of both repos on `devel`),
    // not guessed. See the doc comments on `McpServerEntry`, `McpAuthInfo`,
    // and `effective_provider`/`provider_connected` above for the exact
    // file:line citations each shape below is grounded in.
    // -------------------------------------------------------------------

    /// A `GET /api/v1/mcp/servers` response shaped exactly like jarvice's real
    /// one (`routers/mcp.py::list_mcp_servers`, lines 99-107: `{**server,
    /// "enabled", "auth_required", "auth_satisfied", "tools"}`), covering:
    /// - `github` — OAuth **and** remote (the one real catalog.json with a
    ///   `remote` key: `monocle-tool-server/servers/github/catalog.json`).
    /// - `ms365` — OAuth, non-remote, whose `auth.provider` ("microsoft")
    ///   genuinely diverges from its catalog `name` ("ms365") — the real,
    ///   confirmed case (`servers/ms365/catalog.json`).
    /// - `tavily` — api_key auth. No real catalog.json in this repo currently
    ///   declares `type: "api_key"`, so this entry is synthetic, but its shape
    ///   follows the documented `ApiKeyAuthMeta` contract (jarvice
    ///   `utils/mcp_client.py`, lines 106-119).
    /// - `dart` — no `auth` and no `remote` key at all, matching
    ///   `servers/dart/catalog.json` verbatim.
    const REAL_SERVERS_FIXTURE: &str = r#"{
        "servers": [
            {
                "name": "github",
                "description": "GitHub 공식 리모트 MCP",
                "auth": {
                    "type": "oauth2",
                    "provider": "github",
                    "authorization_endpoint": "https://github.com/login/oauth/authorize",
                    "token_endpoint": "https://github.com/login/oauth/access_token"
                },
                "remote": {
                    "url": "https://api.githubcopilot.com/mcp",
                    "credential": "github"
                },
                "builtin": false,
                "enabled": true,
                "auth_required": true,
                "auth_satisfied": true,
                "tools": [
                    {"name": "search_issues", "description": "Search issues", "enabled": true}
                ]
            },
            {
                "name": "ms365",
                "description": "Microsoft 365 — 파일·메일·캘린더·Teams",
                "auth": {
                    "type": "oauth2",
                    "provider": "microsoft",
                    "authorization_endpoint": "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                    "token_endpoint": "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                    "scopes": ["User.Read", "Mail.Read"]
                },
                "builtin": false,
                "enabled": false,
                "auth_required": true,
                "auth_satisfied": false,
                "tools": []
            },
            {
                "name": "tavily",
                "description": "Synthetic api_key example — no real catalog.json uses this type yet",
                "auth": {
                    "type": "api_key",
                    "provider": "tavily"
                },
                "builtin": false,
                "enabled": false,
                "auth_required": true,
                "auth_satisfied": false,
                "tools": []
            },
            {
                "name": "dart",
                "description": "한국 기업 전자공시(DART)",
                "builtin": false,
                "enabled": true,
                "auth_required": false,
                "auth_satisfied": true,
                "tools": [
                    {"name": "search_company", "description": "Search company", "enabled": true}
                ]
            }
        ]
    }"#;

    /// jarvice's real `/api/v1/connectors/status` response shape
    /// (`models/users.py::UserConnectorsTable.get_status_for_user`, lines
    /// 653-663): a map keyed by **provider**, never by catalog server name.
    const REAL_CONNECTOR_STATUS_FIXTURE: &str = r#"{
        "github": {"connected": true, "scopes": ["repo"], "connected_at": 1751500000},
        "microsoft": {"connected": true, "scopes": ["User.Read", "Mail.Read"], "connected_at": 1751500001}
    }"#;

    #[test]
    fn parse_servers_matches_real_jarvice_shape() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        assert_eq!(servers.len(), 4);
        assert_eq!(servers[0].name, "github");
        assert_eq!(servers[1].name, "ms365");
        assert_eq!(servers[2].name, "tavily");
        assert_eq!(servers[3].name, "dart");
    }

    #[test]
    fn remote_detection_matches_real_catalog_shapes() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        // Only github has a `remote` key at all in the real catalog.
        assert!(
            servers[0].remote.is_some(),
            "github is the one real remote example"
        );
        assert!(
            servers[1].remote.is_none(),
            "ms365's catalog.json has no `remote` key at all (absent, not null/false)"
        );
        assert!(
            servers[3].remote.is_none(),
            "dart's catalog.json has no `remote` key at all (absent, not null/false)"
        );
    }

    #[test]
    fn connect_kind_matches_real_shapes() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        assert!(matches!(connect_kind(&servers[0]), ConnectKind::OAuth)); // github
        assert!(matches!(connect_kind(&servers[1]), ConnectKind::OAuth)); // ms365
        assert!(matches!(connect_kind(&servers[2]), ConnectKind::ApiKey)); // tavily
        assert!(matches!(connect_kind(&servers[3]), ConnectKind::NoAuth)); // dart
    }

    #[test]
    fn effective_provider_uses_auth_provider_when_it_diverges_from_catalog_name() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        // Real, confirmed divergence: monocle-tool-server's
        // servers/ms365/catalog.json declares auth.provider = "microsoft"
        // while the catalog name (the server directory) is "ms365".
        assert_eq!(effective_provider(&servers[1]), "microsoft");
    }

    #[test]
    fn effective_provider_matches_name_when_auth_provider_equals_it() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        assert_eq!(effective_provider(&servers[0]), "github");
    }

    #[test]
    fn effective_provider_falls_back_to_name_when_auth_omits_provider() {
        let entry: McpServerEntry =
            serde_json::from_str(r#"{"name":"x","auth":{"type":"oauth2"}}"#).unwrap();
        assert_eq!(effective_provider(&entry), "x");
    }

    #[test]
    fn effective_provider_falls_back_to_name_when_no_auth_at_all() {
        let servers = parse_servers(REAL_SERVERS_FIXTURE).unwrap();
        assert_eq!(effective_provider(&servers[3]), "dart"); // dart has no auth block
    }

    #[test]
    fn provider_connected_matches_real_status_shape_keyed_by_provider() {
        assert!(provider_connected(REAL_CONNECTOR_STATUS_FIXTURE, "github"));
        assert!(provider_connected(
            REAL_CONNECTOR_STATUS_FIXTURE,
            "microsoft"
        ));
        // The catalog name "ms365" is NOT a key in the real response — only
        // its OAuth provider "microsoft" is. A caller that looked this up by
        // catalog name (the pre-fix bug) would report "not yet connected"
        // forever for ms365, even after the user completes OAuth.
        assert!(!provider_connected(REAL_CONNECTOR_STATUS_FIXTURE, "ms365"));
    }

    #[test]
    fn parse_arg_pairs_builds_json_object() {
        let args = vec!["query=foo".to_string(), "limit=10".to_string()];
        let value = parse_arg_pairs(&args).unwrap();
        assert_eq!(
            value,
            json!({ "query": "foo", "limit": "10" }),
            "values are kept as strings — no type inference in v1"
        );
    }

    #[test]
    fn parse_arg_pairs_rejects_missing_equals() {
        let args = vec!["not-a-pair".to_string()];
        let err = parse_arg_pairs(&args).unwrap_err();
        assert!(
            err.to_string().contains("expected key=value"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn parse_arg_pairs_rejects_empty_key() {
        let args = vec!["=value".to_string()];
        let err = parse_arg_pairs(&args).unwrap_err();
        assert!(
            err.to_string().contains("key must not be empty"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn parse_arg_pairs_rejects_duplicate_key() {
        let args = vec!["query=foo".to_string(), "query=bar".to_string()];
        let err = parse_arg_pairs(&args).unwrap_err();
        assert!(
            err.to_string().contains("duplicate key 'query'"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn parse_arg_pairs_splits_on_first_equals_only() {
        // A value containing '=' (e.g. a base64 token) must survive intact.
        let args = vec!["token=abc=def".to_string()];
        let value = parse_arg_pairs(&args).unwrap();
        assert_eq!(value, json!({ "token": "abc=def" }));
    }

    #[test]
    fn build_tools_call_body_matches_jsonrpc_envelope() {
        // The exact wire shape the Phase 0 spike confirmed against
        // monocle-tool-server's `servers/common.py` — a regression oracle so a
        // future refactor can't silently drop/rename a field.
        let arguments = json!({ "query": "foo" });
        let body = build_tools_call_body("deadbeef", "search_issues", arguments);
        assert_eq!(
            body,
            json!({
                "jsonrpc": "2.0",
                "id": "deadbeef",
                "method": "tools/call",
                "params": {
                    "name": "search_issues",
                    "arguments": { "query": "foo" }
                }
            })
        );
    }

    #[test]
    fn request_id_is_32_lowercase_hex_chars() {
        let id = request_id();
        assert_eq!(id.len(), 32);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn parse_servers_accepts_bare_array() {
        let body = r#"[{"name":"github","enabled":true,"auth_required":true,"auth_satisfied":false,"tools":[{"name":"search_issues"}]}]"#;
        let servers = parse_servers(body).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "github");
        assert!(servers[0].enabled);
        assert!(!servers[0].auth_satisfied);
        assert_eq!(servers[0].tools[0].name, "search_issues");
    }

    #[test]
    fn parse_servers_accepts_wrapped_object() {
        let body = r#"{"servers":[{"name":"filesystem"}]}"#;
        let servers = parse_servers(body).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "filesystem");
        // Defaults kick in for every field the entry omitted.
        assert!(!servers[0].enabled);
        assert!(servers[0].tools.is_empty());
    }

    #[test]
    fn parse_servers_ignores_unknown_fields() {
        // Defensive parsing: an extra/unrecognized field must not break `ls`.
        let body = r#"[{"name":"github","future_field":{"nested":true}}]"#;
        let servers = parse_servers(body).unwrap();
        assert_eq!(servers[0].name, "github");
    }

    #[test]
    fn connect_kind_no_auth_block_means_no_auth() {
        let entry: McpServerEntry = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert!(matches!(connect_kind(&entry), ConnectKind::NoAuth));
    }

    #[test]
    fn connect_kind_api_key_type_means_api_key() {
        let entry: McpServerEntry =
            serde_json::from_str(r#"{"name":"x","auth":{"type":"api_key"}}"#).unwrap();
        assert!(matches!(connect_kind(&entry), ConnectKind::ApiKey));
    }

    #[test]
    fn connect_kind_other_auth_type_means_oauth() {
        let entry: McpServerEntry =
            serde_json::from_str(r#"{"name":"x","auth":{"type":"oauth2"}}"#).unwrap();
        assert!(matches!(connect_kind(&entry), ConnectKind::OAuth));
    }

    #[test]
    fn already_connected_true_when_enabled_and_no_auth_required() {
        let entry: McpServerEntry = serde_json::from_str(r#"{"name":"x","enabled":true}"#).unwrap();
        assert!(already_connected(&entry));
    }

    #[test]
    fn already_connected_false_when_not_enabled() {
        let entry: McpServerEntry = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert!(!already_connected(&entry));
    }

    #[test]
    fn already_connected_false_when_auth_required_but_not_satisfied() {
        let entry: McpServerEntry = serde_json::from_str(
            r#"{"name":"x","enabled":true,"auth_required":true,"auth_satisfied":false}"#,
        )
        .unwrap();
        assert!(!already_connected(&entry));
    }

    #[test]
    fn already_connected_true_when_auth_required_and_satisfied() {
        let entry: McpServerEntry = serde_json::from_str(
            r#"{"name":"x","enabled":true,"auth_required":true,"auth_satisfied":true}"#,
        )
        .unwrap();
        assert!(already_connected(&entry));
    }

    #[test]
    fn is_transient_status_true_for_5xx_and_429() {
        assert!(is_transient_status(500));
        assert!(is_transient_status(503));
        assert!(is_transient_status(429));
    }

    #[test]
    fn is_transient_status_false_for_client_auth_errors() {
        assert!(!is_transient_status(401));
        assert!(!is_transient_status(403));
        assert!(!is_transient_status(404));
        assert!(!is_transient_status(400));
    }

    #[test]
    fn provider_connected_reads_list_shape() {
        let body =
            r#"[{"provider":"github","connected":true},{"provider":"slack","connected":false}]"#;
        assert!(provider_connected(body, "github"));
        assert!(!provider_connected(body, "slack"));
        assert!(!provider_connected(body, "unknown"));
    }

    #[test]
    fn provider_connected_reads_map_of_bools_shape() {
        let body = r#"{"github": true, "slack": false}"#;
        assert!(provider_connected(body, "github"));
        assert!(!provider_connected(body, "slack"));
    }

    #[test]
    fn provider_connected_reads_map_of_objects_shape() {
        let body = r#"{"github": {"connected": true}}"#;
        assert!(provider_connected(body, "github"));
    }

    #[test]
    fn provider_connected_defaults_to_false_on_garbage() {
        assert!(!provider_connected("not json", "github"));
    }

    #[test]
    fn mcp_base_url_env_override_and_fallback() {
        // One test, not two: `MONOCLE_MCP_BASE_URL` is process-wide state, and
        // `cargo test` runs tests in parallel threads within this binary — two
        // separate tests mutating the same env var would race.
        std::env::remove_var("MONOCLE_MCP_BASE_URL");
        assert_eq!(
            mcp_base_url("https://router.example.com"),
            "https://router.example.com",
            "falls back to router_url when unset"
        );

        std::env::set_var("MONOCLE_MCP_BASE_URL", "https://mcp.example.com");
        assert_eq!(
            mcp_base_url("https://router.example.com"),
            "https://mcp.example.com",
            "env var escape hatch takes priority"
        );
        std::env::remove_var("MONOCLE_MCP_BASE_URL");
    }
}
