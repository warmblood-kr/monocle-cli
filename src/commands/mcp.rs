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
// Catalog model — deliberately permissive: we don't have a byte-exact response
// sample from jarvice (its source isn't in this workspace), only the route and
// field names confirmed by the Phase 0 spike, so every field is optional/
// defaulted and unknown extra fields are silently ignored by serde.
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
    /// Presence (non-null) means this is a remote (third-party-hosted) server —
    /// `exec` refuses these in v1.
    remote: Option<Value>,
    auth: Option<McpAuthInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpToolInfo {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct McpAuthInfo {
    #[serde(rename = "type")]
    auth_type: Option<String>,
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

    match connect_kind(entry) {
        ConnectKind::NoAuth => enable_server(client, &router_url, &bearer, name),
        ConnectKind::ApiKey => connect_api_key(client, &router_url, &bearer, name),
        ConnectKind::OAuth => connect_oauth(client, &router_url, &bearer, name),
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

fn connect_api_key(client: &Client, router_url: &str, bearer: &str, name: &str) -> Result<()> {
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

    // NOTE: the exact request body shape for this endpoint isn't confirmed
    // (jarvice's `connectors.py` source wasn't available to the Phase 0 spike
    // beyond the route + purpose); `{"token": ...}` is a reasonable default that
    // is easy to adjust once the wire contract is verified end-to-end.
    let path = endpoints::CONNECTOR_TOKEN.replace("{name}", name);
    let body = json!({ "token": key });
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

fn connect_oauth(client: &Client, router_url: &str, bearer: &str, name: &str) -> Result<()> {
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

    poll_connector_status(client, router_url, bearer, name)
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

/// Whether `name` shows `connected: true` in a `/api/v1/connectors/status`
/// response body. Tolerant of either a list-of-entries or a `name -> state` map
/// shape (bool or `{"connected": bool}`), since we don't have a byte-exact
/// sample; an unparseable/absent body is treated as "not yet connected" so the
/// poll loop just keeps waiting until the timeout.
fn provider_connected(body: &str, name: &str) -> bool {
    match serde_json::from_str::<ConnectorStatusResponse>(body) {
        Ok(ConnectorStatusResponse::List(entries)) => entries
            .iter()
            .find(|e| e.provider == name)
            .map(|e| e.connected)
            .unwrap_or(false),
        Ok(ConnectorStatusResponse::Map(map)) => match map.get(name) {
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
        if provider_connected(&resp.text(), name) {
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
