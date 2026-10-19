//! Client for jarvice's workspace ("custom") model endpoints, prefix
//! `/api/v1/models` — the same rows as the web UI's Workspace > Models page.
//! A workspace model is a named preset on top of a base model (see `monocle
//! router models`): system prompt (`params.system`), tools (`meta.toolIds`),
//! knowledge collections (`meta.knowledge`). `monocle chat --model <id>` uses it.
//!
//! **jarvice-only**, tenant-host-routed like `jarvice_knowledge`.
//!
//! Shapes verified against jarvice `devel` (`routers/models.py`,
//! `models/models.py` `ModelForm`). Server behaviours this client accounts for:
//! - Missing `workspace.models` permission, an unknown id and a taken id are all
//!   **HTTP 401** with a plain-string `detail` (not 403, no `detail.code`), so
//!   existence/duplicates are checked client-side against the list first and a
//!   remaining 401/403 on a write is explained as a permission problem.
//! - `update` takes the **full** `ModelForm`, so [`build_form`] starts from the
//!   stored model and overlays only what the caller changed.
//! - Non-idempotent writes (create/update/toggle/delete) are sent once, never
//!   auto-retried (#174).

use serde_json::{json, Map, Value};

use crate::error::{AppError, Result};
use crate::net::{Client, Resp};
use crate::origin::auth_headers;

/// Render a failed response without echoing its body (see `jarvice_knowledge`).
/// 401/403 on a write get the permission explanation: jarvice uses 401 for it.
fn api_error(what: &str, resp: &Resp) -> AppError {
    if resp.status == 401 || resp.status == 403 {
        return AppError::new(format!(
            "{what} failed: HTTP {}. You probably lack the `workspace.models` permission \
             (or write access to this model); ask a jarvice admin to grant it. \
             jarvice reports this as 401, not 403.",
            resp.status
        ));
    }
    let code = resp
        .json::<Value>()
        .ok()
        .and_then(|v| v["detail"]["code"].as_str().map(str::to_string));
    match code {
        Some(code) => AppError::new(format!("{what} failed: HTTP {} ({code}).", resp.status)),
        None => AppError::new(format!("{what} failed: HTTP {}.", resp.status)),
    }
}

/// Percent-encode for a query value (ids may contain `/`, `:` …).
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn url(base: &str, tail: &str, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("{base}/api/v1/models/{tail}?id={}", enc(id)),
        None => format!("{base}/api/v1/models/{tail}"),
    }
}

/// `GET /api/v1/models/` — every model the caller may read (full records).
pub fn list_models(client: &Client, base: &str, bearer: &str) -> Result<Vec<Value>> {
    let resp = client.get(&url(base, "", None), &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error("Listing models", &resp));
    }
    resp.json()
}

/// Exact-id lookup in a list; error names the list command.
pub fn find<'a>(all: &'a [Value], id: &str) -> Result<&'a Value> {
    all.iter()
        .find(|m| m["id"].as_str() == Some(id))
        .ok_or_else(|| {
            AppError::new(format!(
                "Model '{id}' was not found (run `monocle workspace-model list`)."
            ))
        })
}

/// `GET /api/v1/models/model?id=ID`.
pub fn get_model(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let resp = client.get(&url(base, "model", Some(id)), &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error("Fetching the model", &resp));
    }
    resp.json()
}

/// `POST /api/v1/models/create` with a full `ModelForm`.
pub fn create_model(client: &Client, base: &str, bearer: &str, form: &Value) -> Result<Value> {
    let resp = client.post_json_once(&url(base, "create", None), &auth_headers(bearer), form)?;
    if !resp.ok() {
        return Err(api_error("Creating the model", &resp));
    }
    resp.json()
}

/// `POST /api/v1/models/model/update?id=ID` with a full `ModelForm`.
pub fn update_model(
    client: &Client,
    base: &str,
    bearer: &str,
    id: &str,
    form: &Value,
) -> Result<Value> {
    let resp = client.post_json_once(
        &url(base, "model/update", Some(id)),
        &auth_headers(bearer),
        form,
    )?;
    if !resp.ok() {
        return Err(api_error("Updating the model", &resp));
    }
    resp.json()
}

/// `POST /api/v1/models/model/toggle?id=ID` — returns the model with its new state.
pub fn toggle_model(client: &Client, base: &str, bearer: &str, id: &str) -> Result<Value> {
    let resp = client.post_json_once(
        &url(base, "model/toggle", Some(id)),
        &auth_headers(bearer),
        &json!({}),
    )?;
    if !resp.ok() {
        return Err(api_error("Toggling the model", &resp));
    }
    resp.json()
}

/// `DELETE /api/v1/models/model/delete?id=ID`.
pub fn delete_model(client: &Client, base: &str, bearer: &str, id: &str) -> Result<()> {
    let resp = client.delete_once(&url(base, "model/delete", Some(id)), &auth_headers(bearer))?;
    if !resp.ok() {
        return Err(api_error("Deleting the model", &resp));
    }
    Ok(())
}

/// What the flags change; `None`/empty means "leave as is".
#[derive(Debug, Default)]
pub struct Overrides {
    pub name: Option<String>,
    pub base_model_id: Option<String>,
    pub description: Option<String>,
    pub system: Option<String>,
    /// Replaces `meta.toolIds` when non-empty.
    pub tool_ids: Vec<String>,
    /// Replaces `meta.knowledge` when non-empty: `{id, name, type: "collection"}`.
    pub knowledge: Vec<Value>,
}

fn obj<'a>(form: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    let v = form.entry(key).or_insert_with(|| json!({}));
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().unwrap()
}

/// Build the full `ModelForm` for create/update: start from `stored` (update) or
/// empty (create), overlay the top-level keys of `file` (a JSON `ModelForm`
/// fragment), then the flags. A new model defaults to `name = id`, private
/// (`access_control = {}`, as jarvice's own "private" — omitted would mean public)
/// and active. The `id` of the result is always `id`.
pub fn build_form(
    id: &str,
    stored: Option<&Value>,
    file: Option<&Value>,
    o: &Overrides,
) -> Result<Value> {
    let mut form: Map<String, Value> = match stored {
        // Keep only ModelForm fields (drop user_id, timestamps, …).
        Some(s) => [
            "base_model_id",
            "name",
            "meta",
            "params",
            "access_control",
            "is_active",
        ]
        .iter()
        .filter_map(|k| s.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect(),
        None => {
            let mut m = Map::new();
            m.insert("access_control".into(), json!({}));
            m.insert("is_active".into(), json!(true));
            m
        }
    };
    if let Some(f) = file {
        let f = f
            .as_object()
            .ok_or_else(|| AppError::new("The --file JSON must be an object."))?;
        if let Some(fid) = f.get("id").and_then(Value::as_str) {
            if fid != id {
                return Err(AppError::new(format!(
                    "The --file id '{fid}' differs from the model id '{id}'."
                )));
            }
        }
        for (k, v) in f {
            form.insert(k.clone(), v.clone());
        }
    }
    form.insert("id".into(), json!(id));
    if let Some(b) = &o.base_model_id {
        form.insert("base_model_id".into(), json!(b));
    }
    if let Some(n) = &o.name {
        form.insert("name".into(), json!(n));
    }
    form.entry("name").or_insert_with(|| json!(id));
    let meta = obj(&mut form, "meta");
    if let Some(d) = &o.description {
        meta.insert("description".into(), json!(d));
    }
    if !o.tool_ids.is_empty() {
        meta.insert("toolIds".into(), json!(o.tool_ids));
    }
    if !o.knowledge.is_empty() {
        meta.insert("knowledge".into(), json!(o.knowledge));
    }
    let params = obj(&mut form, "params");
    if let Some(s) = &o.system {
        params.insert("system".into(), json!(s));
    }
    Ok(Value::Object(form))
}
