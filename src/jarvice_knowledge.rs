//! Client for jarvice's knowledge-base (collection) endpoints: `/api/v1/knowledge`
//! (create / list / add-file / delete) and `/api/v1/files` (upload / delete).
//! Same web-UI storage as the workspace "Knowledge" page, so a collection made
//! here is visible there and vice versa.
//!
//! **jarvice-only**, like `jarvice_chats` — tenant-host-routed
//! (`auth::jarvice_url_for`), never chat-proxy's `router_url`.
//!
//! Shapes verified against jarvice `devel` (`routers/knowledge.py`,
//! `routers/files.py`). Two server behaviours this client works around:
//! - `DELETE /knowledge/{id}/delete` drops only the collection row and its
//!   vectors, **not** the uploaded files — [`delete_knowledge_and_files`] removes
//!   those itself.
//! - A file whose text extraction failed still uploads with HTTP 200; the failure
//!   rides along as an `error` envelope on the response body, and only the later
//!   `file/add` rejects it. [`upload_file`] surfaces it as an error.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{AppError, Result};
use crate::jarvice_chats::is_valid_chat_id;
use crate::net::{Client, FilePart, Resp};
use crate::origin::auth_headers;

/// Extensions `upload` accepts, with the MIME type sent for each.
const SUPPORTED: [(&str, &str); 4] = [
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("md", "text/markdown"),
    ("txt", "text/plain"),
];

#[derive(Debug, Default, Deserialize)]
pub struct FileMeta {
    #[serde(default)]
    pub name: String,
}

/// One file as the knowledge endpoints list it (id + display name).
#[derive(Debug, Deserialize)]
pub struct KbFile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub meta: FileMeta,
}

#[derive(Debug, Deserialize)]
pub struct Knowledge {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub files: Option<Vec<KbFile>>,
}

impl Knowledge {
    pub fn files(&self) -> &[KbFile] {
        self.files.as_deref().unwrap_or(&[])
    }
}

/// Render a failed response without echoing its body: jarvice's file/knowledge
/// errors carry a stable `detail.code` envelope, which is safe and useful; any
/// other body (stack text, proxy HTML) is not.
fn api_error(what: &str, resp: &Resp) -> AppError {
    let code = resp
        .json::<Value>()
        .ok()
        .and_then(|v| v["detail"]["code"].as_str().map(str::to_string));
    match code {
        Some(code) => AppError::new(format!("{what} failed: HTTP {} ({code}).", resp.status)),
        None => AppError::new(format!("{what} failed: HTTP {}.", resp.status)),
    }
}

fn checked_id(id: &str) -> Result<&str> {
    // Ids are spliced into URL paths: same guard as chat thread ids.
    if is_valid_chat_id(id) {
        Ok(id)
    } else {
        Err(AppError::new("Invalid ID returned by the server."))
    }
}

/// `GET /api/v1/knowledge/` (everything readable) or `/list` (only collections
/// the caller may write to — what `upload`/`delete` need). Each carries its files.
pub fn list_knowledge(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    writable: bool,
) -> Result<Vec<Knowledge>> {
    let path = if writable { "list" } else { "" };
    let resp = client.get(
        &format!("{jarvice_url}/api/v1/knowledge/{path}"),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(api_error("Listing collections", &resp));
    }
    resp.json()
}

/// Pick one collection by exact id or exact name. A name shared by several
/// collections is an error (listing their ids) rather than a silent first match —
/// `delete` acts on the result.
pub fn find<'a>(all: &'a [Knowledge], key: &str) -> Result<&'a Knowledge> {
    if let Some(k) = all.iter().find(|k| k.id == key) {
        return Ok(k);
    }
    let named: Vec<&Knowledge> = all.iter().filter(|k| k.name == key).collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(AppError::new(format!(
            "Collection '{key}' was not found (run `monocle knowledge list`)."
        ))),
        many => {
            let ids: Vec<&str> = many.iter().map(|k| k.id.as_str()).collect();
            Err(AppError::new(format!(
                "Collection name '{key}' is ambiguous; use one of these ids: {}",
                ids.join(", ")
            )))
        }
    }
}

/// `POST /api/v1/knowledge/create` — private (no `access_control` sharing set).
pub fn create_knowledge(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    name: &str,
) -> Result<Knowledge> {
    let resp = client.post_json_once(
        &format!("{jarvice_url}/api/v1/knowledge/create"),
        &auth_headers(bearer),
        &json!({ "name": name, "description": "" }),
    )?;
    if !resp.ok() {
        return Err(api_error("Creating the collection", &resp));
    }
    resp.json()
}

/// A local file `upload` will send, with the MIME type its extension implies.
#[derive(Debug, PartialEq)]
pub struct UploadFile {
    pub path: PathBuf,
    pub content_type: &'static str,
}

fn mime_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    SUPPORTED.iter().find(|(e, _)| *e == ext).map(|(_, m)| *m)
}

fn scan_dir(dir: &Path, out: &mut Vec<UploadFile>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        if let Some(content_type) = mime_for(&path) {
            out.push(UploadFile { path, content_type });
        }
    }
    Ok(())
}

/// Expand `upload`'s arguments before any request is made. A directory
/// contributes its supported files (top level only, like `kb-sync.sh`; others are skipped); a file
/// named directly must be a supported type, so a typo fails fast instead of
/// half-uploading. Sorted by path for a stable order.
pub fn collect_files(args: &[String]) -> Result<Vec<UploadFile>> {
    let mut out = Vec::new();
    for arg in args {
        let path = Path::new(arg);
        if path.is_dir() {
            scan_dir(path, &mut out)?;
        } else if path.is_file() {
            let content_type = mime_for(path).ok_or_else(|| {
                AppError::new(format!(
                    "Unsupported file type: {arg} (supported: pdf, docx, md, txt)."
                ))
            })?;
            out.push(UploadFile {
                path: path.to_path_buf(),
                content_type,
            });
        } else {
            return Err(AppError::new(format!("file not found: {arg}")));
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    if out.is_empty() {
        return Err(AppError::new(
            "No supported files (pdf, docx, md, txt) found.",
        ));
    }
    Ok(out)
}

/// `POST /api/v1/files/` (multipart `file`) — returns the new file id. The
/// server extracts text synchronously, so this can be slow for big PDFs.
pub fn upload_file(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    filename: &str,
    file: &UploadFile,
) -> Result<String> {
    let part = FilePart {
        field: "file".into(),
        filename: filename.into(),
        content_type: file.content_type.into(),
        data: std::fs::read(&file.path)?,
    };
    let resp = client.post_multipart_once(
        &format!("{jarvice_url}/api/v1/files/"),
        &auth_headers(bearer),
        vec![part],
        &[],
    )?;
    if !resp.ok() {
        return Err(api_error("Upload", &resp));
    }
    let body: Value = resp.json()?;
    let id = checked_id(body["id"].as_str().unwrap_or(""))?.to_string();
    if let Some(code) = body["error"]["code"].as_str() {
        // Stored but unusable — remove it so it is not left orphaned.
        let _ = delete_file(client, jarvice_url, bearer, &id);
        return Err(AppError::new(format!(
            "Upload failed: server could not process the file ({code})."
        )));
    }
    Ok(id)
}

/// `POST /api/v1/knowledge/{id}/file/add` — embeds the file into the collection.
pub fn add_file(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    knowledge_id: &str,
    file_id: &str,
) -> Result<()> {
    let resp = client.post_json_once(
        &format!(
            "{jarvice_url}/api/v1/knowledge/{}/file/add",
            checked_id(knowledge_id)?
        ),
        &auth_headers(bearer),
        &json!({ "file_id": file_id }),
    )?;
    if !resp.ok() {
        return Err(api_error("Adding the file to the collection", &resp));
    }
    Ok(())
}

/// `DELETE /api/v1/files/{id}`. A file that is already gone (404) counts as deleted.
pub fn delete_file(client: &Client, jarvice_url: &str, bearer: &str, id: &str) -> Result<()> {
    let resp = client.delete(
        &format!("{jarvice_url}/api/v1/files/{}", checked_id(id)?),
        &auth_headers(bearer),
    )?;
    if !resp.ok() && resp.status != 404 {
        return Err(api_error("Deleting a file", &resp));
    }
    Ok(())
}

/// Delete every file of a collection, then the collection. If any file fails
/// to delete, the collection is kept (so a retry can still find the leftovers)
/// and the error says how many failed. Returns the number of files deleted.
pub fn delete_knowledge_and_files(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    kb: &Knowledge,
) -> Result<usize> {
    let mut failed = 0;
    for file in kb.files() {
        if delete_file(client, jarvice_url, bearer, &file.id).is_err() {
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(AppError::new(format!(
            "{failed} file(s) could not be deleted; the collection was kept. Re-run delete to retry."
        )));
    }
    let resp = client.delete(
        &format!(
            "{jarvice_url}/api/v1/knowledge/{}/delete",
            checked_id(&kb.id)?
        ),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(api_error("Deleting the collection", &resp));
    }
    Ok(kb.files().len())
}
