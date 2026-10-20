//! `monocle mcp ls|show|attention|enable|disable|tool` — inspect and toggle the
//! logged-in user's MCP servers (the web "Apps" page). Data goes to stdout;
//! the profile banner and confirmations go to stderr.

use crate::agent::DEFAULT_MODEL;
use crate::commands::chat::{
    dropped_tool_calls_message, resolve_jarvice, tool_firing_verification, ToolFiringExpectation,
    ToolFiringVerdict,
};
use crate::commands::knowledge::{connect, print_table};
use crate::credentials::Credentials;
use crate::error::Result;
use crate::jarvice_mcp::{self as mcp, Server};
use crate::net::Client;
use crate::profiles::Selection;
use crate::responses_api::ResponsesClient;

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
        crate::note!("No MCP servers found.");
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
        crate::note!("No tools listed.");
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

/// `monocle mcp thread get|set CHAT_ID` — `servers = None` reads, `Some` replaces
/// the whole set. Prints the resulting `server_ids`, one per line.
pub fn mcp_thread_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    chat_id: &str,
    servers: Option<&[String]>,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, selection)?;
    let body = mcp::chat_servers_raw(client, &url, &bearer, chat_id, servers)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let ids = body["server_ids"].as_array().cloned().unwrap_or_default();
    if ids.is_empty() {
        eprintln!("No MCP servers set for this chat.");
    }
    for id in ids.iter().filter_map(|v| v.as_str()) {
        println!("{id}");
    }
    Ok(())
}

/// Fixed prompt when the user gives none: forces a real tool call.
fn exec_template(server: &str, tool: Option<&str>) -> String {
    match tool {
        Some(t) => format!("Call the `{t}` tool of the `{server}` MCP server now with a minimal valid argument, then reply with one short sentence stating what it returned. Do not answer from memory."),
        None => format!("Call one tool of the `{server}` MCP server now (pick the simplest read-only one), then reply with one short sentence stating what it returned. Do not answer from memory."),
    }
}

/// Cheap checks against the catalog so a doomed turn is never sent.
/// Returns the server id to use in the `mcp:` tool id. Custom servers list no
/// tools in the catalog, so `detail` (the server detail body) supplies them.
fn exec_preflight(
    all: &[mcp::Server],
    server: &str,
    tool: Option<&str>,
    detail: impl FnOnce(&str) -> Result<serde_json::Value>,
) -> Result<String> {
    let s = mcp::find_server(all, server)?;
    let fail = |m: String| Err(crate::error::AppError::new(m));
    if !s.enabled {
        return fail(format!(
            "MCP server '{server}' is disabled (run `monocle mcp enable {server}`)."
        ));
    }
    if matches!(s.auth_state(), "needed" | "failed") {
        return fail(format!(
            "MCP server '{server}' needs authentication (state: {}); reconnect it in the web Apps page.",
            s.auth_state()
        ));
    }
    if let Some(t) = tool {
        let fetched;
        let tools: &[mcp::Tool] = if s.tools.is_empty() {
            let body = detail(&s.id)?;
            let errored = body.get("tools_error").is_some_and(|e| !e.is_null());
            fetched = serde_json::from_value::<Server>(body).map(|d| d.tools).ok();
            match fetched.as_deref() {
                Some(ts) if !ts.is_empty() && !errored => ts,
                _ => {
                    return fail(format!(
                        "MCP tool list unavailable for '{server}'; cannot verify tool '{t}' exists."
                    ))
                }
            }
        } else {
            &s.tools
        };
        match tools.iter().find(|x| x.name == t) {
            None => {
                return fail(format!(
                    "MCP server '{server}' has no tool '{t}' (run `monocle mcp show {server}`)."
                ))
            }
            Some(x) if !x.enabled => {
                return fail(format!(
                    "Tool '{t}' of '{server}' is disabled (run `monocle mcp tool {server} {t} enable`)."
                ))
            }
            _ => {}
        }
    }
    Ok(s.id.clone())
}

/// `tools_used` names are `SERVER__TOOL`; accept the bare tool name too.
/// Server-only runs must have fired a tool of that server.
fn exec_verdict(
    server: &Server,
    tool: Option<&str>,
    reply: &crate::responses_api::ResponsesReply,
) -> ToolFiringVerdict {
    let dropped = dropped_tool_calls_message(
        &reply.tool_calls,
        reply.unparsed_tool_calls,
        "--responses mode",
    );
    // jarvice sanitizes names it sends natively; tools_used carries the sanitized
    // form, so compare sanitized-to-sanitized (both sides).
    let used: Vec<String> = reply.tools_used.iter().map(|u| mcp::sanitize(u)).collect();
    let prefixes = [
        format!("{}__", mcp::sanitize(&server.id)),
        format!("{}__", mcp::sanitize(&server.name)),
    ];
    let expected = match tool {
        Some(t) => {
            let want = [&server.id, &server.name]
                .iter()
                .map(|n| mcp::fn_name(n, t))
                .find(|q| used.contains(q))
                .unwrap_or_else(|| mcp::sanitize(t));
            ToolFiringExpectation::Named(want)
        }
        None => ToolFiringExpectation::Any,
    };
    let verdict = tool_firing_verification(
        &expected,
        &used,
        reply.unparsed_tools_used,
        reply.tools_used_present,
        dropped.as_deref(),
    );
    if tool.is_none() && matches!(verdict, ToolFiringVerdict::Passed(_)) {
        let ours = used
            .iter()
            .any(|u| prefixes.iter().any(|p| u.starts_with(p.as_str())));
        if !ours && reply.unparsed_tools_used > 0 {
            // Unreadable entries might be this server's; "Any" must not pass on them.
            return ToolFiringVerdict::CannotVerify(format!(
                "? tool-firing verification: CANNOT VERIFY — {} `tools_used` entry/entries came back in an unrecognized shape, so whether a tool of `{}` ran is unknown",
                reply.unparsed_tools_used, server.name
            ));
        }
        if !ours {
            return ToolFiringVerdict::Failed(format!(
                "✗ tool-firing verification FAILED: no tool of `{}` ran; these ran instead: {}",
                server.name,
                reply.tools_used.join(", ")
            ));
        }
    }
    verdict
}

/// `monocle mcp exec SERVER[/TOOL] [PROMPT...] [--verify] [--model M]`
pub fn mcp_exec_command(
    client: &Client,
    creds: &Credentials,
    target: &str,
    prompt: &[String],
    verify: bool,
    model: Option<&str>,
) -> Result<()> {
    let (server, tool) = match target.split_once('/') {
        Some((s, t)) if !s.is_empty() && !t.is_empty() => (s, Some(t)),
        _ => (target, None),
    };
    let (session, url, bearer) = resolve_jarvice(client, creds)?;
    let all = mcp::parse_servers(&mcp::list_servers_raw(client, &url, &bearer)?)?;
    let id = exec_preflight(&all, server, tool, |id| {
        mcp::server_detail_raw(client, &url, &bearer, id)
    })?;
    let tool_id = match tool {
        Some(t) => format!("mcp:{}", mcp::fn_name(&id, t)),
        None => format!("mcp:{id}"),
    };
    let text = if prompt.is_empty() {
        exec_template(server, tool)
    } else {
        prompt.join(" ")
    };
    let model = model.unwrap_or(DEFAULT_MODEL);
    crate::note!("Using model: {model}; tool id: {tool_id}");
    let rc = ResponsesClient::new(client, session.token, url);
    let reply = rc.respond(model, &text, &[], &[], None, &[tool_id])?;
    println!("{}", reply.content);
    if let Some(id) = &reply.thread_id {
        crate::note!("Thread: {id}");
    }
    if verify {
        let srv = mcp::find_server(&all, server)?;
        let v = exec_verdict(srv, tool, &reply);
        eprintln!("{}", v.message());
        if v.exit_code() != 0 {
            std::process::exit(v.exit_code());
        }
    }
    Ok(())
}
