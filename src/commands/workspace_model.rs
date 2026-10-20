//! `monocle workspace-model list|show|create|update|toggle|delete` — manage the
//! logged-in user's jarvice workspace (custom) models. Data goes to stdout;
//! the profile banner and summaries go to stderr, so output pipes cleanly.

use serde_json::Value;

use crate::commands::knowledge::{connect, print_table};
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::jarvice_knowledge as kb;
use crate::jarvice_models::{self as models, Overrides};
use crate::net::Client;
use crate::profiles::Selection;

/// Flags shared by `create` and `update` (already parsed by clap).
#[derive(Debug, Default)]
pub struct WriteArgs {
    pub id: String,
    pub file: Option<String>,
    pub name: Option<String>,
    pub base: Option<String>,
    pub description: Option<String>,
    pub system: Option<String>,
    pub tools: Vec<String>,
    pub knowledge: Vec<String>,
    pub clear_tools: bool,
    pub clear_knowledge: bool,
    /// `private` | `public`; mutually exclusive with the share lists (clap-enforced).
    pub access: Option<String>,
    pub read_groups: Vec<String>,
    pub write_groups: Vec<String>,
    pub read_users: Vec<String>,
    pub write_users: Vec<String>,
}

/// `--access` / share lists -> jarvice `access_control`: null = public,
/// {} = private, {read,write: {group_ids,user_ids}} = shared.
fn access_control(a: &WriteArgs) -> Option<Value> {
    match a.access.as_deref() {
        Some("public") => return Some(Value::Null),
        Some(_) => return Some(serde_json::json!({})),
        None => {}
    }
    let lists = [
        &a.read_groups,
        &a.write_groups,
        &a.read_users,
        &a.write_users,
    ];
    if lists.iter().all(|l| l.is_empty()) {
        return None;
    }
    Some(serde_json::json!({
        "read": {"group_ids": a.read_groups, "user_ids": a.read_users},
        "write": {"group_ids": a.write_groups, "user_ids": a.write_users},
    }))
}

fn s(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn read_file(path: &str) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AppError::new(format!("Cannot read {path}: {e}")))?;
    serde_json::from_str(&text).map_err(|e| AppError::new(format!("{path} is not valid JSON: {e}")))
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Resolve `--knowledge` names/ids to `meta.knowledge` entries, so a typo fails
/// before anything is written.
fn knowledge_entries(
    client: &Client,
    url: &str,
    bearer: &str,
    keys: &[String],
) -> Result<Vec<Value>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let all = kb::list_knowledge(client, url, bearer, false)?;
    keys.iter()
        .map(|k| {
            let c = kb::find(&all, k)?;
            Ok(serde_json::json!({"id": c.id, "name": c.name, "type": "collection"}))
        })
        .collect()
}

fn overrides(client: &Client, url: &str, bearer: &str, a: &WriteArgs) -> Result<Overrides> {
    Ok(Overrides {
        name: a.name.clone(),
        base_model_id: a.base.clone(),
        description: a.description.clone(),
        system: a.system.clone(),
        tool_ids: a.tools.clone(),
        knowledge: knowledge_entries(client, url, bearer, &a.knowledge)?,
        clear_tools: a.clear_tools,
        clear_knowledge: a.clear_knowledge,
        access_control: access_control(a),
    })
}

pub fn list_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    json: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    let all = models::list_models(client, &url, &bearer)?;
    if json {
        let v: Vec<Value> = all
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": s(m, "id"), "name": s(m, "name"), "base_model_id": s(m, "base_model_id"),
                    "active": m["is_active"].as_bool().unwrap_or(true),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    if all.is_empty() {
        crate::note!("No workspace models found.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|m| {
            let active = if m["is_active"].as_bool().unwrap_or(true) {
                "yes"
            } else {
                "no"
            };
            vec![
                s(m, "id"),
                s(m, "name"),
                s(m, "base_model_id"),
                active.into(),
            ]
        })
        .collect();
    print_table(&["ID", "NAME", "BASE", "ACTIVE"], &rows);
    Ok(())
}

/// Prints the model as JSON. Large `data:` URLs (the profile image) are replaced
/// by `<data:MIME, N bytes>` unless `full`, which prints the exact server body.
pub fn show_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
    full: bool,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    models::find(&models::list_models(client, &url, &bearer)?, id)?;
    let mut model = models::get_model(client, &url, &bearer, id)?;
    if !full {
        models::elide_data_urls(&mut model);
    }
    print_json(&model);
    Ok(())
}

/// Prints the new model id to stdout (pipe it into `chat --model`).
pub fn create_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    a: &WriteArgs,
) -> Result<()> {
    let file = a.file.as_deref().map(read_file).transpose()?;
    let (url, bearer) = connect(client, creds, sel)?;
    if models::find(&models::list_models(client, &url, &bearer)?, &a.id).is_ok() {
        return Err(AppError::new(format!(
            "Model '{}' already exists; delete it first or pick another id.",
            a.id
        )));
    }
    let o = overrides(client, &url, &bearer, a)?;
    let form = models::build_form(&a.id, None, file.as_ref(), &o)?;
    if form["base_model_id"].as_str().unwrap_or("").is_empty() {
        return Err(AppError::new(
            "A base model is required: pass --base <id> (see `monocle router models`) or set base_model_id in --file.",
        ));
    }
    let created = models::create_model(client, &url, &bearer, &form)?;
    println!("{}", s(&created, "id"));
    Ok(())
}

/// Only what the flags/`--file` name changes; everything else is kept.
pub fn update_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    a: &WriteArgs,
) -> Result<()> {
    let file = a.file.as_deref().map(read_file).transpose()?;
    let (url, bearer) = connect(client, creds, sel)?;
    models::find(&models::list_models(client, &url, &bearer)?, &a.id)?;
    let stored = models::get_model(client, &url, &bearer, &a.id)?;
    let o = overrides(client, &url, &bearer, a)?;
    let form = models::build_form(&a.id, Some(&stored), file.as_ref(), &o)?;
    models::update_model(client, &url, &bearer, &a.id, &form)?;
    eprintln!("Updated model '{}'.", a.id);
    Ok(())
}

pub fn toggle_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    models::find(&models::list_models(client, &url, &bearer)?, id)?;
    let m = models::toggle_model(client, &url, &bearer, id)?;
    let state = if m["is_active"].as_bool().unwrap_or(true) {
        "active"
    } else {
        "inactive"
    };
    eprintln!("Model '{id}' is now {state}.");
    Ok(())
}

pub fn delete_command(
    client: &Client,
    creds: &Credentials,
    sel: &Selection,
    id: &str,
) -> Result<()> {
    let (url, bearer) = connect(client, creds, sel)?;
    models::find(&models::list_models(client, &url, &bearer)?, id)?;
    models::delete_model(client, &url, &bearer, id)?;
    eprintln!("Deleted model '{id}'.");
    Ok(())
}
