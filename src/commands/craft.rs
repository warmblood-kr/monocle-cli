//! `monocle craft session list|show|history|files|download|delete` — inspect and
//! clean up Craft sessions on either lane: `openworker` (the Craft server) or
//! `sdk` (jarvice agent sessions). Data goes to stdout; the profile banner and
//! confirmations go to stderr, so output pipes cleanly.

use std::io::Write;

use clap::{Subcommand, ValueEnum};
use serde_json::Value;

use crate::auth::get_access_token;
use crate::commands::cells::{time_from_iso, yes_no};
use crate::commands::chat::resolve_jarvice;
use crate::commands::craft_sdk_turn::{self, SdkTurn};
use crate::commands::craft_turn::{is_queue_full, run_turn_id, Target, TurnOpts};
use crate::commands::knowledge::print_table;
use crate::craft_server as craft;
use crate::craft_state::LastSession;
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
        /// Session ID (see `monocle craft session list`)
        id: String,
        /// Print the raw session as JSON
        #[arg(long)]
        json: bool,
    },
    /// Print a session's history (`--json` for the whole server response)
    History {
        /// Session ID (see `monocle craft session list`)
        id: String,
        /// Include system messages (openworker lane)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// List a session's files
    Files {
        /// Session ID (see `monocle craft session list`)
        id: String,
        /// Folder to list (sdk lane; the server default applies if omitted)
        #[arg(long, value_parser = ["uploads", "outputs"])]
        folder: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Download one session file to stdout, or to a file with `-o`
    Download {
        /// Session ID (see `monocle craft session list`)
        id: String,
        /// File path as listed by `files`
        path: String,
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Delete a session
    Delete {
        /// Session ID (see `monocle craft session list`)
        id: String,
        /// Print the server's response as JSON
        #[arg(long)]
        json: bool,
    },
    /// Attach your delegation to a session (openworker lane)
    ///
    /// Lets Craft call the model gateway on your behalf; stg/prod Craft hosts only.
    Delegate {
        /// Session ID (see `monocle craft session list`)
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
        /// Workspace folder path (see `monocle craft workspace list`)
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
    selection.announce(creds);
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

/// Boolean table cell as `yes`/`no`; non-booleans fall back to [`text`].
fn flag(v: &Value) -> String {
    v.as_bool().map_or_else(|| text(v), |b| yes_no(b).into())
}

fn one_line(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// A delegation expiry (epoch seconds) as a UTC date; anything else as plain text.
fn expiry_date(v: &Value) -> String {
    v.as_i64()
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| text(v))
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
        return delegate(client, creds, selection, lane, id, *json, true);
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
                crate::note!("No sessions found.");
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
                            time_from_iso(s(x, "updated_at")),
                            one_line(s(x, "title")),
                        ]
                    }
                })
                .collect();
            let headers = if sdk_lane {
                ["ID", "STATUS", "UPDATED (UTC)", "TITLE"]
            } else {
                ["ID", "MSGS", "UPDATED (UTC)", "TITLE"]
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
                crate::note!("No history.");
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
                crate::note!("No files.");
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
            eprintln!("Deleted session {id}.");
            if r["recovered"].as_bool() == Some(true) && crate::util::verbose() {
                eprintln!("(the server answered HTTP 500 but the session no longer exists)");
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
    announce: bool,
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
    // Per-turn delegation is plumbing (`-v` only); the explicit command confirms itself.
    if announce || crate::util::verbose() {
        eprintln!(
            "Delegation attached (expires {})",
            expiry_date(&r["expires_at"])
        );
    }
    Ok(())
}

/// `--continue`, step 1 (no network, no auth): the stored id for this
/// profile/env/lane. None stored makes no turn and no request of any kind.
fn stored_continue(last: &LastSession, selection: &Selection, lane: Lane) -> Result<String> {
    let lane_name = if lane == Lane::Sdk {
        "sdk"
    } else {
        "openworker"
    };
    last.get().ok_or_else(|| {
        let flag = if lane == Lane::Sdk { " --lane sdk" } else { "" };
        AppError::new(format!(
            "No previous craft session for lane {lane_name} (profile {}, env {}). Start one with: monocle craft{flag} turn \"...\"",
            selection.profile, selection.environment
        ))
    })
}

/// `--continue`, step 2: the stored id must still exist (a gone session drops
/// its entry).
fn check_continue(
    last: &LastSession,
    id: String,
    exists: impl FnOnce(&str) -> Result<bool>,
) -> Result<String> {
    if !exists(&id)? {
        last.clear();
        return Err(AppError::new(format!(
            "Session {id} no longer exists. Start a new one without --continue."
        )));
    }
    crate::note!("Continuing session {id}");
    Ok(id)
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
    mut opts: TurnOpts,
) -> Result<()> {
    let last = LastSession::new(
        &selection.profile,
        &selection.environment,
        if lane == Lane::Sdk {
            "sdk"
        } else {
            "openworker"
        },
    );
    opts.last = Some(last.clone());
    if let Some(id) = &opts.session {
        crate::commands::craft_turn::note_session(id);
    }
    crate::commands::craft_turn::install_interrupt_notice();
    // Read the state file before any auth/connect so "none stored" costs no request.
    let stored = if opts.continue_last {
        Some(stored_continue(&last, selection, lane)?)
    } else {
        None
    };
    if lane == Lane::Sdk {
        if opts.workspace.is_some() {
            return Err(AppError::new(
                "--workspace is only supported on the openworker lane.",
            ));
        }
        let (base, bearer) = connect(client, creds, selection, lane)?;
        if let Some(id) = stored {
            opts.session = Some(check_continue(&last, id, |id| {
                sdk::session_exists(client, &base, &bearer, id)
            })?);
            opts.follow_up = true; // "Continuing session" already named it
        }
        return turn_loop(&opts, |o| {
            craft_sdk_turn::run_id(&SdkTurn {
                interactive: o.interactive,
                follow_up: o.follow_up,
                last: Some(&last),
                hint_prefix: o.hint_prefix.as_deref(),
                client,
                base: &base,
                bearer: &bearer,
                text: &o.text,
                session: o.session.as_deref(),
                title: title.as_deref(),
                timeout: o.timeout,
                json: o.json,
                timings: o.timings,
                diagnostics_file: o.diagnostics_file.clone(),
            })
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
    if let Some(id) = stored {
        opts.session = Some(check_continue(&last, id, |id| {
            craft::session_exists(client, &base, &bearer, id)
        })?);
        opts.follow_up = true; // "Continuing session" already named it
    }
    turn_loop(&opts, |o| {
        run_turn_id(&target, o, |id| {
            if no_delegation {
                return Ok(());
            }
            delegate(client, creds, selection, lane, id, false, false)
        })
    })
}

/// The prompt `> ` is for a person: only when stdin AND stderr are terminals.
fn prompt_visible(stdin_tty: bool, stderr_tty: bool) -> bool {
    stdin_tty && stderr_tty
}

const BUSY_RETRIES: usize = 3;
const BUSY_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// `turn -i` loop (plain lines, no raw mode) over any lane; `run` is one existing
/// one-shot turn (connection re-established per turn) returning the session id.
fn turn_loop(
    opts: &TurnOpts,
    run: impl FnMut(&TurnOpts) -> (Result<()>, Option<String>),
) -> Result<()> {
    use std::io::IsTerminal;
    if !opts.interactive {
        let mut run = run;
        return run(opts).0;
    }
    let prompt = prompt_visible(
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    );
    lines_loop(opts, std::io::stdin().lock(), prompt, BUSY_WAIT, run)
}

fn lines_loop(
    opts: &TurnOpts,
    mut input: impl std::io::BufRead,
    prompt: bool,
    wait: std::time::Duration,
    mut run: impl FnMut(&TurnOpts) -> (Result<()>, Option<String>),
) -> Result<()> {
    let mut sid: Option<String> = None;
    let mut pending = Some(opts.text.clone()).filter(|t| !t.is_empty());
    let mut outcome = Ok(());
    let mut first = true;
    loop {
        let text = match pending.take() {
            Some(t) => t,
            None => {
                if prompt {
                    eprint!("> ");
                }
                let mut line = String::new();
                if input.read_line(&mut line).map_err(AppError::from)? == 0 {
                    break;
                }
                let line = line.trim();
                if matches!(line, "/exit" | "/quit") {
                    break;
                }
                if line.is_empty() {
                    continue;
                }
                line.to_string()
            }
        };
        let mut o = opts.clone();
        o.text = text;
        let mut attempt = 0;
        let res = loop {
            if sid.is_some() {
                o.session = sid.clone();
                o.follow_up = true;
            }
            let (res, id) = run(&o);
            sid = id.or(sid);
            match res {
                // Rejected before it was accepted: safe to resend, never a duplicate.
                Err(e) if is_queue_full(&e) && attempt < BUSY_RETRIES => {
                    attempt += 1;
                    eprintln!("server busy, retrying…");
                    std::thread::sleep(wait);
                }
                res => break res,
            }
        };
        if let Err(e) = res {
            if !first {
                eprintln!("Error: {e}"); // a later turn: the conversation goes on
            } else {
                outcome = Err(e);
                break;
            }
        }
        first = false;
    }
    outcome
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
                crate::note!("No recent workspaces.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = all
                .iter()
                .map(|x| {
                    vec![
                        one_line(s(x, "name")),
                        flag(&x["exists"]),
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
        crate::note!("No agents.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|x| {
            vec![
                s(x, "name").into(),
                yes_no(x["default"].as_bool() == Some(true)).into(),
                flag(&x["needs_workspace"]),
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

#[cfg(test)]
mod tests {
    use super::prompt_visible;

    #[test]
    fn prompt_only_when_both_ends_are_terminals() {
        assert!(prompt_visible(true, true));
        assert!(!prompt_visible(false, true));
        assert!(!prompt_visible(true, false));
    }
}
