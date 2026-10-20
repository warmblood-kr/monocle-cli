//! `monocle schedule list|show|toggle|delete|runs|mcp-servers|preview|create|update` — read and
//! manage the logged-in user's schedules. Data goes to stdout; the profile banner
//! and confirmations go to stderr. `--json` is the server body, unchanged
//! (epochs are raw seconds there; tables render them as UTC).

use chrono::{TimeZone, Utc};
use serde_json::{json, Value};

use crate::commands::cells::{time_from_unix, yes_no};
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

/// Table-cell form of [`when`]: `YYYY-MM-DD HH:MM` (UTC) like every other list.
fn when_cell(v: &Value) -> String {
    v.as_i64().map_or("-".into(), time_from_unix)
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
                s["enabled"]
                    .as_bool()
                    .map_or_else(|| s["enabled"].to_string(), |b| yes_no(b).into()),
                when_cell(&s["next_fire_at"]),
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
    print_epochs(&body, json)
}

fn print_epochs(body: &Value, json: bool) -> Result<()> {
    if json {
        print_json(body);
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
                    when_cell(&r["scheduled_for"]),
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

// ---------------------------------------------------------------- create / update

/// Flags shared by `create` and `update` (on update only the passed ones change).
#[derive(clap::Args, Default, Debug)]
pub struct WriteArgs {
    /// Model id (what `monocle chat --model` takes); required on create
    #[arg(long)]
    pub model: Option<String>,
    /// Prompt text sent on every run; `-` reads it from stdin. Create needs --prompt or --prompt-file
    #[arg(long)]
    pub prompt: Option<String>,
    /// Read the prompt from this file
    #[arg(long, value_name = "PATH", conflicts_with = "prompt")]
    pub prompt_file: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    /// One-off run time: RFC3339 (`2030-01-02T09:00:00+09:00`) or relative (`+30s`, `+10m`, `+2h`, `+1d`)
    #[arg(long, value_name = "WHEN", conflicts_with_all = ["rrule", "dtstart"])]
    pub at: Option<String>,
    /// Recurring: RFC 5545 RRULE, e.g. FREQ=DAILY (create also needs --dtstart)
    #[arg(long)]
    pub rrule: Option<String>,
    /// Recurring anchor (first occurrence / wall-clock time); same formats as --at
    #[arg(long, value_name = "WHEN")]
    pub dtstart: Option<String>,
    /// IANA timezone the rule is evaluated in (server default: UTC)
    #[arg(long, value_name = "TZ")]
    pub timezone: Option<String>,
    /// Run in a fresh chat each time (`new`) or keep appending to one (`continue`)
    #[arg(long, value_parser = ["new", "continue"])]
    pub thread_mode: Option<String>,
    /// Chat id to continue (`none` clears it on update); the server rejects chats you do not own
    #[arg(long, value_name = "CHAT_ID")]
    pub target_chat: Option<String>,
    /// Stop firing after this time (same formats as --at; `none` clears it on update)
    #[arg(long, value_name = "WHEN")]
    pub expires_at: Option<String>,
    /// Switch the schedule off
    #[arg(long, conflicts_with = "enabled")]
    pub disabled: bool,
    /// Switch the schedule on (update; create is on by default)
    #[arg(long)]
    pub enabled: bool,
    /// What to do with slots missed while the runner was down
    #[arg(long, value_parser = ["skip_missed", "run_missed"])]
    pub catchup: Option<String>,
    /// Let the run use web search
    #[arg(long)]
    pub web_search: bool,
    /// Turn web search off (update)
    #[arg(long, conflicts_with = "web_search")]
    pub no_web_search: bool,
}

/// `+10m` / RFC3339 -> epoch seconds. Offset-less times are rejected rather than guessed.
pub fn parse_when(s: &str, now: i64) -> Result<i64> {
    let bad = || {
        AppError::new(format!(
            "Invalid time '{s}': use RFC3339 (2030-01-02T09:00:00+09:00) or relative (+30s, +10m, +2h, +1d)."
        ))
    };
    if let Some(rel) = s.strip_prefix('+') {
        let (n, unit) = rel.split_at(rel.len().saturating_sub(1));
        let mult = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(bad()),
        };
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let n: i64 = n.parse().map_err(|_| bad())?;
        return n
            .checked_mul(mult)
            .and_then(|d| now.checked_add(d))
            .ok_or_else(bad);
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| t.timestamp())
        .map_err(|_| bad())
}

fn read_prompt(a: &WriteArgs) -> Result<Option<String>> {
    let text = match (&a.prompt, &a.prompt_file) {
        (Some(p), _) if p == "-" => std::io::read_to_string(std::io::stdin())?,
        (Some(p), _) => p.clone(),
        (None, Some(f)) => std::fs::read_to_string(f)
            .map_err(|e| AppError::new(format!("Cannot read --prompt-file {f}: {e}")))?,
        (None, None) => return Ok(None),
    };
    if text.trim().is_empty() {
        return Err(AppError::new("The prompt is empty."));
    }
    Ok(Some(text))
}

/// The `ScheduleForm` fields (jarvice `models/schedules.py:345-363`) in wire order.
/// `next_fire_at` is deliberately absent: the server recomputes it when it is null on
/// both insert and update (`models/schedules.py:408-412`, `:462-464`); echoing the
/// stored one on update would pin a stale value. The web client omits it too
/// (`src/lib/apis/schedules/index.ts` `ScheduleForm`).
const FORM_KEYS: [&str; 15] = [
    "name",
    "model_id",
    "prompt_template",
    "kind",
    "recurrence",
    "run_at",
    "dtstart",
    "timezone",
    "thread_mode",
    "target_chat_id",
    "requires_web_search",
    "enabled",
    "expires_at",
    "overlap_policy",
    "catchup_policy",
];

/// Server defaults for a new form (`models/schedules.py:345-363`).
fn default_form() -> Value {
    json!({
        "name": null, "model_id": null, "prompt_template": null,
        "kind": "one_off", "recurrence": null, "run_at": null, "dtstart": null,
        "timezone": "UTC", "thread_mode": "new", "target_chat_id": null,
        "requires_web_search": false, "enabled": true, "expires_at": null,
        "overlap_policy": "skip", "catchup_policy": "skip_missed",
    })
}

/// Overlay only the flags the user passed onto `form`, then reject what the server
/// would accept but never fire.
fn apply(form: &mut Value, a: &WriteArgs, prompt: Option<String>, now: i64) -> Result<()> {
    let clearable = |v: &str| (v != "none").then(|| v.to_string());
    if let Some(m) = &a.model {
        form["model_id"] = json!(m);
    }
    if let Some(p) = prompt {
        form["prompt_template"] = json!(p);
    }
    if let Some(n) = &a.name {
        form["name"] = json!(n);
    }
    if let Some(t) = &a.at {
        form["kind"] = json!("one_off");
        form["run_at"] = json!(parse_when(t, now)?);
        form["recurrence"] = Value::Null;
        form["dtstart"] = Value::Null;
    }
    if let Some(r) = &a.rrule {
        form["kind"] = json!("recurring");
        form["recurrence"] = json!(r);
        form["run_at"] = Value::Null;
    }
    if let Some(d) = &a.dtstart {
        form["dtstart"] = json!(parse_when(d, now)?);
    }
    if let Some(z) = &a.timezone {
        form["timezone"] = json!(z);
    }
    if let Some(m) = &a.thread_mode {
        form["thread_mode"] = json!(m);
    }
    if let Some(c) = &a.target_chat {
        form["target_chat_id"] = json!(clearable(c));
    }
    if let Some(e) = &a.expires_at {
        let v = clearable(e).map(|e| parse_when(&e, now)).transpose()?;
        form["expires_at"] = json!(v);
    }
    if let Some(c) = &a.catchup {
        form["catchup_policy"] = json!(c);
    }
    if a.disabled {
        form["enabled"] = json!(false);
    }
    if a.enabled {
        form["enabled"] = json!(true);
    }
    if a.web_search {
        form["requires_web_search"] = json!(true);
    }
    if a.no_web_search {
        form["requires_web_search"] = json!(false);
    }
    // compute_next_fire (utils/schedule_recurrence.py:129-146) returns None, i.e. the
    // schedule never fires, for a one-off without run_at and a recurring rule without
    // dtstart — yet ScheduleForm accepts both. Everything else (rule validity, past
    // times, whole minutes, COUNT, intervals, chat ownership, thread_mode vs
    // target_chat) is left to the server: it enforces none of the last one.
    if form["kind"] == "recurring" {
        if form["recurrence"].is_null() || form["dtstart"].is_null() {
            return Err(AppError::new(
                "A recurring schedule needs both --rrule and --dtstart.",
            ));
        }
    } else if form["run_at"].is_null() {
        return Err(AppError::new(
            "A one-off schedule needs --at (or use --rrule with --dtstart).",
        ));
    }
    Ok(())
}

/// `monocle schedule create ...` — prints the new id (or the `--json` body) to stdout.
pub fn schedule_create_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    a: &WriteArgs,
    mcp_servers: &[String],
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let prompt = read_prompt(a)?;
    if a.model.is_none() || prompt.is_none() {
        return Err(AppError::new(
            "create needs --model and a prompt (--prompt TEXT, --prompt-file PATH, or --prompt - for stdin).",
        ));
    }
    let mut form = default_form();
    apply(&mut form, a, prompt, Utc::now().timestamp())?;
    let (url, bearer) = connect(client, creds, sel)?;
    if dry_run {
        let body = sched::preview(client, &url, &bearer, &form, 5)?;
        return print_epochs(&body, json);
    }
    let body = sched::create(client, &url, &bearer, &form)?;
    let id = body["public_id"]
        .as_str()
        .filter(|i| crate::jarvice_chats::is_valid_chat_id(i))
        .ok_or_else(|| AppError::new("The server did not return a schedule id."))?
        .to_string();
    eprintln!("Created schedule {id}");
    if !mcp_servers.is_empty() {
        if let Err(e) = sched::mcp_servers(client, &url, &bearer, &id, Some(mcp_servers)) {
            return Err(AppError::new(format!(
                "Schedule {id} was created but setting its MCP servers failed ({e}) \
                 Remove it with `monocle schedule delete {id}`, or retry with `monocle schedule mcp-servers {id} --set ...`."
            )));
        }
    }
    if json {
        print_json(&body);
    } else {
        println!("{id}");
    }
    Ok(())
}

/// `monocle schedule update ID ...` — GET the stored schedule, overlay the passed
/// flags, POST the full form (the endpoint overwrites every field).
pub fn schedule_update_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
    a: &WriteArgs,
    json: bool,
) -> Result<()> {
    let prompt = read_prompt(a)?;
    let untouched = prompt.is_none()
        && a.model.is_none()
        && a.name.is_none()
        && a.at.is_none()
        && a.rrule.is_none()
        && a.dtstart.is_none()
        && a.timezone.is_none()
        && a.thread_mode.is_none()
        && a.target_chat.is_none()
        && a.expires_at.is_none()
        && a.catchup.is_none()
        && !(a.disabled || a.enabled || a.web_search || a.no_web_search);
    if untouched {
        return Err(AppError::new("Nothing to update: pass at least one flag."));
    }
    let (url, bearer) = connect(client, creds, sel)?;
    let current = sched::show(client, &url, &bearer, id)?;
    let mut form = default_form();
    for k in FORM_KEYS {
        form[k] = current[k].clone();
    }
    if !form["model_id"].is_string() || !form["prompt_template"].is_string() {
        return Err(AppError::new("Unexpected schedule from the server."));
    }
    apply(&mut form, a, prompt, Utc::now().timestamp())?;
    let body = sched::update(client, &url, &bearer, id, &form)?;
    eprintln!("Updated schedule {id}");
    if json {
        print_json(&body);
    } else {
        one_schedule(&body);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_when_relative_and_rfc3339() {
        assert_eq!(parse_when("+10m", 1000).unwrap(), 1600);
        assert_eq!(parse_when("+2h", 0).unwrap(), 7200);
        assert_eq!(parse_when("+1d", 0).unwrap(), 86400);
        assert_eq!(parse_when("+30s", 5).unwrap(), 35);
        assert_eq!(
            parse_when("2023-11-14T22:13:20Z", 0).unwrap(),
            1_700_000_000
        );
        assert_eq!(
            parse_when("2023-11-15T07:13:20+09:00", 0).unwrap(),
            1_700_000_000
        );
    }

    #[test]
    fn parse_when_rejects_garbage() {
        for s in [
            "",
            "+",
            "+m",
            "+10",
            "+10x",
            "+-5m",
            "++5m",
            "10m",
            "2023-11-14",
            "2023-11-14T22:13:20",
            "+99999999999999999999m",
        ] {
            assert!(parse_when(s, 0).is_err(), "{s}");
        }
    }
}
