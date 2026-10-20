//! `monocle mcp ls|show|attention|enable|disable|tool` — inspect and toggle the
//! logged-in user's MCP servers (the web "Apps" page). Data goes to stdout;
//! the profile banner and confirmations go to stderr.

use crate::commands::knowledge::{connect, print_table};
use crate::credentials::Credentials;
use crate::error::Result;
use crate::jarvice_mcp as mcp;
use crate::net::Client;
use crate::profiles::Selection;

fn yn(b: bool) -> String {
    if b { "yes" } else { "no" }.to_string()
}

fn print_json(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// `monocle mcp ls [--json]`
pub fn mcp_ls_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let body = mcp::list_servers_raw(client, &url, &bearer)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let all = mcp::parse_servers(&body)?;
    if all.is_empty() {
        eprintln!("No MCP servers found.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            vec![
                s.name.clone(),
                yn(s.enabled),
                s.auth_state().to_string(),
                s.tools.len().to_string(),
            ]
        })
        .collect();
    print_table(&["NAME", "ENABLED", "AUTH", "TOOLS"], &rows);
    Ok(())
}

/// `monocle mcp show SERVER [--json]`
pub fn mcp_show_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    server: &str,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let all = mcp::parse_servers(&mcp::list_servers_raw(client, &url, &bearer)?)?;
    let id = mcp::find_server(&all, server)?.id.clone();
    let body = mcp::server_detail_raw(client, &url, &bearer, &id)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let s: mcp::Server = serde_json::from_value(body)
        .map_err(|_| crate::error::AppError::new("Unexpected MCP server from the server."))?;
    println!(
        "{}  enabled: {}  auth: {}",
        s.name,
        yn(s.enabled),
        s.auth_state()
    );
    if s.tools.is_empty() {
        eprintln!("No tools listed.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = s
        .tools
        .iter()
        .map(|t| vec![t.name.clone(), yn(t.enabled)])
        .collect();
    print_table(&["TOOL", "ENABLED"], &rows);
    Ok(())
}

/// `monocle mcp attention [--json]` — how many servers need re-authentication.
pub fn mcp_attention_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let body = mcp::attention_raw(client, &url, &bearer)?;
    if json {
        print_json(&body);
    } else {
        println!("{}", body["count"].as_u64().unwrap_or(0));
    }
    Ok(())
}

/// `monocle mcp enable|disable SERVER` and `monocle mcp tool SERVER TOOL enable|disable`.
pub fn mcp_set_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    server: &str,
    tool: Option<&str>,
    enabled: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    mcp::set_enabled(client, &url, &bearer, server, tool, enabled)?;
    let what = tool.map_or(server.to_string(), |t| format!("{server}/{t}"));
    eprintln!("{what}: {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}
