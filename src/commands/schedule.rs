//! `monocle schedule list|show|toggle|delete|runs|mcp-servers|preview` — read and
//! manage the logged-in user's schedules. Data goes to stdout; the profile banner
//! and confirmations go to stderr. `--json` is the server body, unchanged
//! (epochs are raw seconds there; tables render them as UTC).

use chrono::{TimeZone, Utc};
use serde_json::{json, Value};

use crate::commands::chat::{tool_firing_verification, ToolFiringExpectation, ToolFiringVerdict};
use crate::commands::knowledge::{connect, print_table};
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::jarvice_mcp::sanitize;
use crate::jarvice_schedules::{self as sched, RunTools};
use crate::net::Client;
use crate::profiles::Selection;

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Epoch seconds -> `YYYY-MM-DD HH:MM:SSZ` (UTC); `-` when absent.
fn when(v: &Value) -> String {
    v.as_i64()
        .and_then(|s| Utc.timestamp_opt(s, 0).single())
        .map_or("-".into(), |t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
}

/// Text field on one line (server strings are data, not trusted for the terminal).
fn text(v: &Value) -> String {
    v.as_str()
        .map_or("-".into(), |s| s.replace(char::is_control, " "))
}

fn one_schedule(s: &Value) {
    let rule = if s["kind"] == "recurring" {
        text(&s["recurrence"])
    } else {
        format!("once at {}", when(&s["run_at"]))
    };
    println!("{}  {}", text(&s["public_id"]), text(&s["name"]));
    println!(
        "  enabled: {}  kind: {}  rule: {rule}",
        s["enabled"],
        text(&s["kind"])
    );
    println!(
        "  model: {}  timezone: {}  thread: {}",
        text(&s["model_id"]),
        text(&s["timezone"]),
        text(&s["thread_mode"])
    );
    println!(
        "  next fire: {}  last fired: {}",
        when(&s["next_fire_at"]),
        when(&s["last_fired_at"])
    );
}

/// `monocle schedule list [--json]`
pub fn schedule_list_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    let body = sched::list(client, &url, &bearer)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let all = body
        .as_array()
        .ok_or_else(|| AppError::new("Unexpected schedule list from the server."))?;
    if all.is_empty() {
        eprintln!("No schedules found.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            vec![
                text(&s["public_id"]),
                text(&s["name"]),
                text(&s["kind"]),
                s["enabled"].to_string(),
                when(&s["next_fire_at"]),
            ]
        })
        .collect();
    print_table(&["ID", "NAME", "KIND", "ENABLED", "NEXT FIRE (UTC)"], &rows);
    Ok(())
}

/// `monocle schedule show ID [--json]` and (after the change) `toggle ID on|off`.
pub fn schedule_show_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
    enabled: Option<bool>,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    let body = match enabled {
        None => sched::show(client, &url, &bearer, id)?,
        Some(on) => sched::toggle(client, &url, &bearer, id, on)?,
    };
    if json {
        print_json(&body);
    } else {
        one_schedule(&body);
    }
    Ok(())
}

/// `monocle schedule delete ID`
pub fn schedule_delete_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    sched::delete(client, &url, &bearer, id)?;
    eprintln!("Deleted schedule {id}");
    Ok(())
}

/// `monocle schedule mcp-servers ID [--set ..]` — prints the resulting `server_ids`.
pub fn schedule_mcp_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
    servers: Option<&[String]>,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    let body = sched::mcp_servers(client, &url, &bearer, id, servers)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let ids = body["server_ids"].as_array().cloned().unwrap_or_default();
    if ids.is_empty() {
        eprintln!("No MCP servers set for this schedule.");
    }
    for id in ids.iter().filter_map(|v| v.as_str()) {
        println!("{}", id.replace(char::is_control, " "));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// `monocle schedule preview` — next fire times for an unsaved schedule.
pub fn schedule_preview_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    recurrence: Option<&str>,
    dtstart: Option<i64>,
    run_at: Option<i64>,
    timezone: &str,
    count: u32,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    // Minimal `ScheduleForm` (jarvice models/schedules.py:345): `model_id` and
    // `prompt_template` are required by the model but irrelevant to the pure
    // preview computation, so they are placeholders and nothing is stored.
    let form = json!({
        "model_id": "preview", "prompt_template": "preview",
        "kind": if recurrence.is_some() { "recurring" } else { "one_off" },
        "recurrence": recurrence, "dtstart": dtstart, "run_at": run_at, "timezone": timezone,
    });
    let body = sched::preview(client, &url, &bearer, &form, count)?;
    if json {
        print_json(&body);
        return Ok(());
    }
    let list = body
        .as_array()
        .ok_or_else(|| AppError::new("Unexpected preview from the server."))?;
    if list.is_empty() {
        eprintln!("The schedule never fires.");
    }
    for t in list {
        println!("{}", when(t));
    }
    Ok(())
}

/// Verdict for one run: 0 a tool ran / 1 none did / 2 the run cannot say.
/// Names in `calls` are compared sanitized on both sides, the same
/// `server__tool` form as `mcp exec` (`jarvice_mcp::fn_name`); a bare TOOL also
/// matches the `__TOOL` suffix. A call with `ok:false` still counts as fired.
fn run_verdict(run: &Value, tool: Option<&str>) -> ToolFiringVerdict {
    let used: Vec<String> = match sched::run_tools(run) {
        RunTools::Calls(c) => c.iter().map(|n| sanitize(n)).collect(),
        RunTools::Unknown(why) => {
            return ToolFiringVerdict::CannotVerify(format!(
                "? tool-firing verification: CANNOT VERIFY — {why}"
            ))
        }
    };
    let expected = match tool {
        None => ToolFiringExpectation::Any,
        Some(t) => {
            let t = sanitize(t);
            let suffix = format!("__{t}");
            let hit = used.iter().find(|u| **u == t || u.ends_with(&suffix));
            ToolFiringExpectation::Named(hit.cloned().unwrap_or(t))
        }
    };
    tool_firing_verification(&expected, &used, 0, true, None)
}

/// `monocle schedule runs ID [--limit N] [--json] [--verify[=TOOL]] [--run RUN_ID]`
#[allow(clippy::too_many_arguments)]
pub fn schedule_runs_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
    limit: u32,
    json: bool,
    verify: Option<&str>,
    run_id: Option<&str>,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    let body = sched::runs(client, &url, &bearer, id, limit)?;
    let all = body
        .as_array()
        .ok_or_else(|| AppError::new("Unexpected run list from the server."))?;
    if json {
        print_json(&body);
    } else if all.is_empty() {
        eprintln!("No runs yet.");
    } else {
        let rows: Vec<Vec<String>> = all
            .iter()
            .map(|r| {
                vec![
                    text(&r["public_id"]),
                    when(&r["scheduled_for"]),
                    text(&r["status"]),
                    r["attempt"].to_string(),
                    text(&r["error"]),
                ]
            })
            .collect();
        print_table(
            &["RUN", "SCHEDULED (UTC)", "STATUS", "ATTEMPT", "ERROR"],
            &rows,
        );
    }
    let Some(tool) = verify else { return Ok(()) };
    let tool = Some(tool).filter(|t| !t.is_empty());
    // Latest FINISHED run (the list order is not relied on), or the one asked for.
    let run = match run_id {
        Some(r) => all.iter().find(|x| x["public_id"] == r),
        None => all
            .iter()
            .filter(|x| x["finished_at"].is_i64() || x["finished_at"].is_u64())
            .max_by_key(|x| (x["finished_at"].as_i64(), x["scheduled_for"].as_i64())),
    };
    let v = match run {
        Some(r) => run_verdict(r, tool),
        None => ToolFiringVerdict::CannotVerify(
            "? tool-firing verification: CANNOT VERIFY — no matching finished run in the last runs"
                .into(),
        ),
    };
    eprintln!("{}", v.message());
    if v.exit_code() != 0 {
        std::process::exit(v.exit_code());
    }
    Ok(())
}
