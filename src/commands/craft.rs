//! `monocle craft session list|show|history|files|download|delete` — inspect and
//! clean up Craft sessions on either lane: `openworker` (the Craft server) or
//! `sdk` (jarvice agent sessions). Data goes to stdout; the profile banner and
//! confirmations go to stderr, so output pipes cleanly.

use std::io::Write;

use clap::{Subcommand, ValueEnum};
use serde_json::Value;

use crate::auth::get_access_token;
use crate::commands::chat::resolve_jarvice;
use crate::commands::craft_sdk_turn::{self, SdkTurn};
use crate::commands::craft_turn::{run_turn, Target, TurnOpts};
use crate::commands::knowledge::print_table;
use crate::craft_server as craft;
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::jarvice_agent as sdk;
use crate::net::Client;
use crate::profiles::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Lane {
    Openworker,
    Sdk,
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List sessions (id, status/updated, title)
    List {
        /// Only sessions in this workspace path (openworker lane only)
        #[arg(long)]
        workspace: Option<String>,
        /// Print the raw sessions as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show one session's details
    Show {
        id: String,
        /// Print the raw session as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print a session's history (`--json` for the whole server response)
    History {
        id: String,
        /// Include system messages (openworker lane)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// List a session's files
    Files {
        id: String,
        /// Folder to list (sdk lane; the server default applies if omitted)
        #[arg(long, value_parser = ["uploads", "outputs"])]
        folder: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Download one session file to stdout, or to a file with `-o`
    Download {
        id: String,
        /// File path as listed by `files`
        path: String,
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Delete a session
    Delete {
        id: String,
        /// Print the server's response as JSON
        #[arg(long)]
        json: bool,
    },
    /// Attach your Stark delegation to a session so Craft can call the model
    /// gateway (openworker lane; stg/prod Craft hosts only)
    Delegate {
        id: String,
        /// Print {"ok","expires_at"} as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum WorkspaceCommand {
    /// List recent workspaces
    List {
        /// Print the raw workspaces as JSON
        #[arg(long)]
        json: bool,
    },
    /// Open a workspace folder (prints the server's response as JSON)
    Open {
        path: String,
        /// Create the folder if it does not exist
        #[arg(long)]
        create: bool,
    },
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// List the agents the Craft server offers
    List {
        /// Print the raw agents as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Lane endpoint + bearer, with the same stderr profile banner `knowledge` prints.
fn connect(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
) -> Result<(String, String)> {
    let (url, bearer) = match lane {
        Lane::Sdk => {
            let (_, url, bearer) = resolve_jarvice(client, creds)?;
            (url, bearer)
        }
        Lane::Openworker => {
            let url = craft::base_url(&selection.environment)?;
            (
                url,
                format!("Bearer {}", get_access_token(client, creds).token),
            )
        }
    };
    eprintln!(
        "Profile: {} ({}) — tenant {}",
        selection.profile,
        selection.environment,
        creds.read().map(|c| c.tenant_domain).unwrap_or_default()
    );
    Ok((url, bearer))
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

/// Scalar as plain text, anything else as compact JSON.
fn text(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn one_line(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn unix_time(v: &Value) -> String {
    v.as_i64()
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| text(v))
}

pub fn craft_session_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
    command: &SessionCommand,
) -> Result<()> {
    // Reject lane-specific flags before touching the network.
    match command {
        SessionCommand::List {
            workspace: Some(_), ..
        } if lane == Lane::Sdk => {
            return Err(AppError::new(
                "--workspace is only supported on the openworker lane.",
            ))
        }
        SessionCommand::Files {
            folder: Some(_), ..
        } if lane == Lane::Openworker => {
            return Err(AppError::new("--folder is only supported on the sdk lane."))
        }
        _ => {}
    }
    if let SessionCommand::Delegate { id, json } = command {
        return delegate(client, creds, selection, lane, id, *json);
    }
    let (url, bearer) = connect(client, creds, selection, lane)?;
    let (c, u, b) = (client, url.as_str(), bearer.as_str());
    let sdk_lane = lane == Lane::Sdk;
    match command {
        SessionCommand::List { workspace, json } => {
            let all = if sdk_lane {
                sdk::list_sessions(c, u, b)?
            } else {
                craft::list_sessions(c, u, b, workspace.as_deref())?
            };
            if *json {
                return print_json(&all);
            }
            if all.is_empty() {
                eprintln!("No sessions found.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = all
                .iter()
                .map(|x| {
                    if sdk_lane {
                        vec![
                            s(x, "id").into(),
                            s(x, "status").into(),
                            unix_time(&x["updated_at"]),
                            one_line(s(x, "title")),
                        ]
                    } else {
                        vec![
                            s(x, "session_id").into(),
                            text(&x["messages"]),
                            s(x, "updated_at").into(),
                            one_line(s(x, "title")),
                        ]
                    }
                })
                .collect();
            let headers = if sdk_lane {
                ["SESSION ID", "STATUS", "UPDATED", "TITLE"]
            } else {
                ["SESSION ID", "MSGS", "UPDATED", "TITLE"]
            };
            print_table(&headers, &rows);
        }
        SessionCommand::Show { id, json } => {
            let one = if sdk_lane {
                sdk::get_session(c, u, b, id)?
            } else {
                craft::get_session(c, u, b, id)?
            };
            if *json {
                return print_json(&one);
            }
            for (k, v) in one.as_object().into_iter().flatten() {
                let v = if k.ends_with("_at") && sdk_lane {
                    unix_time(v)
                } else {
                    text(v)
                };
                println!("{k}: {v}");
            }
        }
        SessionCommand::History { id, all, json } => {
            let h = if sdk_lane {
                sdk::history(c, u, b, id)?
            } else {
                craft::history(c, u, b, id)?
            };
            if *json {
                return print_json(&h);
            }
            let lines: Vec<String> = if sdk_lane {
                // v2 `events`; v1 fallback `entries` (shapes beyond `type`/`content` unpinned).
                let list = h["events"].as_array().or(h["entries"].as_array());
                list.into_iter().flatten().map(sdk_event_line).collect()
            } else {
                h["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|m| *all || s(m, "role") != "system")
                    .map(|m| {
                        let body = if m["content"].is_null() {
                            &m["text"]
                        } else {
                            &m["content"]
                        };
                        format!("{}\t{}", s(m, "role"), one_line(&text(body)))
                    })
                    .collect()
            };
            if lines.is_empty() {
                eprintln!("No history.");
            }
            lines.iter().for_each(|l| println!("{l}"));
        }
        SessionCommand::Files { id, folder, json } => {
            let f = if sdk_lane {
                sdk::files(c, u, b, id, folder.as_deref())?
            } else {
                craft::files(c, u, b, id)?
            };
            if *json {
                return print_json(&f);
            }
            let list = f["files"].as_array().or(f["artifacts"].as_array());
            let list = list.map(Vec::as_slice).unwrap_or_default();
            if list.is_empty() {
                eprintln!("No files.");
            }
            if sdk_lane {
                let rows: Vec<Vec<String>> = list
                    .iter()
                    .map(|x| {
                        let dir = x["is_directory"].as_bool().unwrap_or(false);
                        vec![
                            text(&x["size"]),
                            s(x, "modified").into(),
                            format!("{}{}", s(x, "name"), if dir { "/" } else { "" }),
                        ]
                    })
                    .collect();
                if !rows.is_empty() {
                    print_table(&["SIZE", "MODIFIED", "NAME"], &rows);
                }
            } else {
                for x in list {
                    // Artifact shape is not pinned down: known fields, else the raw JSON.
                    let known: Vec<String> = ["name", "path", "size"]
                        .iter()
                        .filter(|k| !x[**k].is_null())
                        .map(|k| text(&x[*k]))
                        .collect();
                    println!(
                        "{}",
                        if known.is_empty() {
                            x.to_string()
                        } else {
                            known.join("\t")
                        }
                    );
                }
            }
        }
        SessionCommand::Download { id, path, output } => {
            let bytes = if sdk_lane {
                sdk::download(c, u, b, id, path)?
            } else {
                craft::download(c, u, b, id, path)?
            };
            match output {
                Some(file) => {
                    std::fs::write(file, &bytes)?;
                    eprintln!("Saved {} byte(s) to {file}.", bytes.len());
                }
                None => std::io::stdout().write_all(&bytes)?,
            }
        }
        SessionCommand::Delegate { .. } => unreachable!("handled above"),
        SessionCommand::Delete { id, json } => {
            let r = if sdk_lane {
                sdk::delete(c, u, b, id)?
            } else {
                craft::delete(c, u, b, id)?
            };
            if *json {
                print_json(&r)?;
            }
            if r["recovered"].as_bool() == Some(true) {
                eprintln!("Session {id} is gone (the server answered HTTP 500 but the session no longer exists).");
            } else {
                eprintln!("Deleted session {id}.");
            }
        }
    }
    Ok(())
}

fn openworker_only(lane: Lane, what: &str) -> Result<()> {
    if lane == Lane::Sdk {
        return Err(AppError::new(format!(
            "{what} is only supported on the openworker lane."
        )));
    }
    Ok(())
}

/// `session delegate`: the id_token goes only to the env's own Craft host (see
/// `craft::delegation_base`). The host check runs before credentials are read, and
/// the token is read after a fresh access token so it is current. It is never printed.
fn delegate(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
    id: &str,
    json: bool,
) -> Result<()> {
    openworker_only(lane, "`session delegate`")?;
    let base = craft::delegation_base(&selection.environment)?;
    let bearer = format!("Bearer {}", get_access_token(client, creds).token);
    let data = creds.read();
    let id_token = data.as_ref().map(|c| c.id_token.as_str()).unwrap_or("");
    if id_token.is_empty() {
        return Err(AppError::new(
            "No id_token stored; run `monocle login` again.",
        ));
    }
    let r = craft::attach_delegation(client, &base, &bearer, id, id_token)?;
    if json {
        print_json(&serde_json::json!({"ok": true, "expires_at": r["expires_at"]}))?;
    }
    eprintln!(
        "Delegation attached (expires_at {})",
        text(&r["expires_at"])
    );
    Ok(())
}

/// `craft turn`: hosts are vetted (REST base and WS origin) before anything is
/// sent; delegation (unless skipped) goes first, then the socket.
pub fn craft_turn_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
    no_delegation: bool,
    title: Option<String>,
    opts: TurnOpts,
) -> Result<()> {
    if lane == Lane::Sdk {
        if opts.workspace.is_some() {
            return Err(AppError::new(
                "--workspace is only supported on the openworker lane.",
            ));
        }
        let (base, bearer) = connect(client, creds, selection, lane)?;
        return craft_sdk_turn::run(&SdkTurn {
            hint_prefix: &opts.hint_prefix,
            client,
            base: &base,
            bearer: &bearer,
            text: &opts.text,
            session: opts.session.as_deref(),
            title: title.as_deref(),
            timeout: opts.timeout,
            json: opts.json,
            timings: opts.timings,
            diagnostics_file: opts.diagnostics_file.clone(),
        });
    }
    if title.is_some() {
        return Err(AppError::new("--title is only supported on the sdk lane."));
    }
    let ws_base = craft::ws_base(&selection.environment)?;
    let (base, bearer) = connect(client, creds, selection, lane)?;
    let target = Target {
        client,
        base: &base,
        ws_base: &ws_base,
        bearer: &bearer,
    };
    run_turn(&target, &opts, |id| {
        if no_delegation {
            return Ok(());
        }
        delegate(client, creds, selection, lane, id, false)
    })
}

pub fn craft_workspace_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
    command: &WorkspaceCommand,
) -> Result<()> {
    openworker_only(lane, "`craft workspace`")?;
    let (url, bearer) = connect(client, creds, selection, lane)?;
    match command {
        WorkspaceCommand::List { json } => {
            let all = craft::list_workspaces(client, &url, &bearer)?;
            if *json {
                return print_json(&all);
            }
            if all.is_empty() {
                eprintln!("No recent workspaces.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = all
                .iter()
                .map(|x| {
                    vec![
                        one_line(s(x, "name")),
                        text(&x["exists"]),
                        s(x, "path").into(),
                    ]
                })
                .collect();
            print_table(&["NAME", "EXISTS", "PATH"], &rows);
        }
        WorkspaceCommand::Open { path, create } => {
            print_json(&craft::open_workspace(
                client, &url, &bearer, path, *create,
            )?)?;
        }
    }
    Ok(())
}

pub fn craft_agent_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    lane: Lane,
    command: &AgentCommand,
) -> Result<()> {
    openworker_only(lane, "`craft agent`")?;
    let (url, bearer) = connect(client, creds, selection, lane)?;
    let AgentCommand::List { json } = command;
    let all = craft::list_agents(client, &url, &bearer)?;
    if *json {
        return print_json(&all);
    }
    if all.is_empty() {
        eprintln!("No agents.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|x| {
            vec![
                s(x, "name").into(),
                if x["default"].as_bool() == Some(true) {
                    "yes"
                } else {
                    ""
                }
                .into(),
                text(&x["needs_workspace"]),
                one_line(s(x, "title")),
                one_line(s(x, "tagline")),
            ]
        })
        .collect();
    print_table(&["NAME", "DEFAULT", "NEEDS WS", "TITLE", "TAGLINE"], &rows);
    Ok(())
}

/// One sdk history event as `type<TAB>summary`: user/text show their content;
/// other types (tool_use, tool_result, thinking, result, ...) a tool name or
/// truncated compact JSON — never a full tool result.
fn sdk_event_line(e: &serde_json::Value) -> String {
    let kind = s(e, "type");
    let body = if matches!(kind, "user" | "text") {
        one_line(&text(&e["content"]))
    } else if kind == "tool_result" {
        String::new() // never dump tool output
    } else if let Some(n) = ["tool_name", "name"].iter().find_map(|k| e[*k].as_str()) {
        one_line(n)
    } else {
        one_line(&e.to_string()).chars().take(120).collect()
    };
    format!("{kind}\t{body}")
}
