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
    pub description: String,
    #[serde(default)]
    pub files: Option<Vec<KbFile>>,
    /// Everything else the server sent (`access_control`, `data`, ...), kept so
    /// `update` can echo it back untouched.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Knowledge {
    pub fn files(&self) -> &[KbFile] {
        self.files.as_deref().unwrap_or(&[])
    }
}

/// A server-provided `detail.code` is printed only if it looks like a code,
/// `^[a-z0-9_.-]{1,64}$` (case-insensitive): a credential or free text reflected
/// into that field (JWT-like, long, spaced) is dropped. `instrument::http_status`
/// accepts the same shape.
pub(crate) fn is_safe_code(c: &str) -> bool {
    (1..=64).contains(&c.len())
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `detail.code` of an error body, only if it passes [`is_safe_code`].
pub(crate) fn safe_detail_code(body: &Value) -> Option<String> {
    body["detail"]["code"]
        .as_str()
        .filter(|c| is_safe_code(c))
        .map(str::to_string)
}

/// Render a failed response without echoing its body: jarvice's file/knowledge
/// errors carry a stable `detail.code` envelope, which is safe and useful when
/// it passes [`is_safe_code`]; any other body (stack text, proxy HTML) is not.
pub(crate) fn api_error(what: &str, resp: &Resp) -> AppError {
    AppError::new(format!(
        "{what} failed: {}.",
        status_label(resp.status, &resp.text())
    ))
}

/// `HTTP <status>` plus ` (<code>)` when `body` is JSON with a safe `detail.code`.
/// The body text is only inspected, never echoed.
pub(crate) fn status_label(status: u16, body: &str) -> String {
    match serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| safe_detail_code(&v))
    {
        Some(c) => format!("HTTP {status} ({c})"),
        None => format!("HTTP {status}"),
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
        let code = if is_safe_code(code) {
            format!(" ({code})")
        } else {
            String::new()
        };
        return Err(AppError::new(format!(
            "Upload failed: server could not process the file{code}."
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

/// `POST /api/v1/knowledge/{id}/update` — rename and/or re-describe. The server
/// replaces `access_control` wholesale (null means public), so it is echoed back
/// from the listing; if the listing did not carry it, refuse rather than risk
/// publishing a private collection.
pub fn update_knowledge(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    kb: &Knowledge,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<()> {
    let access = kb.extra.get("access_control").ok_or_else(|| {
        AppError::new("The server did not report this collection's access control; not updating.")
    })?;
    let mut body = json!({
        "name": name.unwrap_or(&kb.name),
        "description": description.unwrap_or(&kb.description),
        "access_control": access,
    });
    if let Some(data) = kb.extra.get("data").filter(|d| d.is_object()) {
        body["data"] = data.clone();
    }
    let resp = client.post_json_once(
        &format!(
            "{jarvice_url}/api/v1/knowledge/{}/update",
            checked_id(&kb.id)?
        ),
        &auth_headers(bearer),
        &body,
    )?;
    if !resp.ok() {
        return Err(api_error("Updating the collection", &resp));
    }
    Ok(())
}

/// Pick one file of a collection by exact id or exact name (ambiguous names list ids).
pub fn find_file<'a>(kb: &'a Knowledge, key: &str) -> Result<&'a KbFile> {
    if let Some(f) = kb.files().iter().find(|f| f.id == key) {
        return Ok(f);
    }
    let named: Vec<&KbFile> = kb.files().iter().filter(|f| f.meta.name == key).collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(AppError::new(format!(
            "File '{key}' is not in collection '{}' (run `monocle knowledge list {}`).",
            kb.name, kb.name
        ))),
        many => {
            let ids: Vec<&str> = many.iter().map(|f| f.id.as_str()).collect();
            Err(AppError::new(format!(
                "File name '{key}' is ambiguous; use one of these ids: {}",
                ids.join(", ")
            )))
        }
    }
}

/// `POST /api/v1/knowledge/{id}/file/remove` — detach one file, keep the
/// collection. The server also deletes the file itself unless something else
/// (another collection, a chat) still references it.
pub fn remove_file(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    knowledge_id: &str,
    file_id: &str,
) -> Result<()> {
    let resp = client.post_json_once(
        &format!(
            "{jarvice_url}/api/v1/knowledge/{}/file/remove",
            checked_id(knowledge_id)?
        ),
        &auth_headers(bearer),
        &json!({ "file_id": checked_id(file_id)? }),
    )?;
    if !resp.ok() {
        return Err(api_error("Removing the file from the collection", &resp));
    }
    Ok(())
}

/// Re-extract a file's text from its stored binary (`POST /files/{id}/reprocess`,
/// `force` needed when it already has text), then re-embed it into the
/// collection (`POST /knowledge/{id}/file/update`).
pub fn reprocess_file(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    knowledge_id: &str,
    file_id: &str,
    force: bool,
) -> Result<()> {
    let q = if force { "?force=true" } else { "" };
    let resp = client.post_json_once(
        &format!(
            "{jarvice_url}/api/v1/files/{}/reprocess{q}",
            checked_id(file_id)?
        ),
        &auth_headers(bearer),
        &json!({}),
    )?;
    if resp.status == 409 {
        return Err(AppError::new(
            "File is already processed; pass --force to re-extract it.",
        ));
    }
    if !resp.ok() {
        return Err(api_error("Reprocessing the file", &resp));
    }
    let resp = client.post_json_once(
        &format!(
            "{jarvice_url}/api/v1/knowledge/{}/file/update",
            checked_id(knowledge_id)?
        ),
        &auth_headers(bearer),
        &json!({ "file_id": file_id }),
    )?;
    if !resp.ok() {
        return Err(api_error("Re-indexing the file in the collection", &resp));
    }
    Ok(())
}

/// `GET /api/v1/files/{id}/data/content` — the text the server extracted.
pub fn file_content(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    file_id: &str,
) -> Result<String> {
    let resp = client.get(
        &format!(
            "{jarvice_url}/api/v1/files/{}/data/content",
            checked_id(file_id)?
        ),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(api_error("Reading the file content", &resp));
    }
    let v: Value = resp.json()?;
    Ok(v["content"].as_str().unwrap_or("").to_string())
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

/// One retrieved source the server cited for an answer.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Cited {
    pub name: String,
    /// How many retrieved passages came from it.
    pub passages: usize,
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct Answer {
    pub answer: String,
    pub sources: Vec<Cited>,
}

/// Fold one decoded SSE/JSON payload into `out`: content deltas append to the
/// answer, a top-level `sources` event (jarvice emits it before the first token)
/// adds the files the retrieved passages came from.
fn absorb(v: &Value, out: &mut Answer) {
    if let Some(text) = v["choices"][0]["delta"]["content"]
        .as_str()
        .or_else(|| v["choices"][0]["message"]["content"].as_str())
    {
        out.answer.push_str(text);
    }
    // For a collection, `source` is the collection itself; the cited files are the
    // per-passage `metadata` entries (verified on stg).
    for s in v["sources"].as_array().into_iter().flatten() {
        // Other source kinds (e.g. URL citations) are not files of the collection.
        if s["source"]["type"] != "collection" {
            continue;
        }
        for m in s["metadata"].as_array().into_iter().flatten() {
            let name = m["name"].as_str().or(m["source"].as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            match out.sources.iter_mut().find(|c| c.name == name) {
                Some(c) => c.passages += 1,
                None => out.sources.push(Cited {
                    name: name.to_string(),
                    passages: 1,
                }),
            }
        }
    }
}

/// Parse a chat-completions SSE body line by line.
pub fn parse_answer(mut next_line: impl FnMut() -> Result<Option<String>>) -> Result<Answer> {
    let mut out = Answer::default();
    while let Some(line) = next_line()? {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if let Ok(v) = serde_json::from_str::<Value>(data) {
            absorb(&v, &mut out);
        }
    }
    Ok(out)
}

/// Ask `model` a question with the collection attached
/// (`POST /api/chat/completions`, `files: [{type: "collection"}]`, streamed).
/// jarvice has no retrieval REST, so this chat turn is the only place a
/// collection's content is observable: the answer plus the files it was grounded
/// in. Headless (no `chat_id`), so nothing is saved to the user's thread list.
/// One attempt only — the turn costs tokens.
pub fn ask(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    model: &str,
    kb: &Knowledge,
    question: &str,
) -> Result<Answer> {
    let body = json!({
        "model": model,
        "stream": true,
        "messages": [{ "role": "user", "content": question }],
        "files": [{ "type": "collection", "id": checked_id(&kb.id)?, "name": kb.name }],
    });
    let mut stream = client.post_json_stream_once(
        &format!("{jarvice_url}/api/chat/completions"),
        &auth_headers(bearer),
        &body,
    )?;
    if !stream.ok() {
        let status = stream.status;
        let code = serde_json::from_str::<Value>(&stream.read_all())
            .ok()
            .and_then(|v| safe_detail_code(&v));
        return Err(AppError::new(match code {
            Some(c) => format!("Asking the collection failed: HTTP {status} ({c})."),
            None => format!("Asking the collection failed: HTTP {status}."),
        }));
    }
    if !stream.is_event_stream() {
        let v: Value = serde_json::from_str(&stream.read_all())?;
        let mut out = Answer::default();
        absorb(&v, &mut out);
        return Ok(out);
    }
    // A blocking read cannot time out on its own, so a reader thread feeds lines
    // through a channel and silence longer than STREAM_IDLE becomes an error.
    // ponytail: the reader thread is left blocked on timeout; the CLI exits right after.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || while tx.send(stream.next_line()).is_ok() {});
    parse_with_idle(&rx, STREAM_IDLE)
}

/// Longest silence tolerated mid-answer.
const STREAM_IDLE: std::time::Duration = std::time::Duration::from_secs(120);

fn parse_with_idle(
    rx: &std::sync::mpsc::Receiver<Result<Option<String>>>,
    idle: std::time::Duration,
) -> Result<Answer> {
    parse_answer(|| {
        match rx.recv_timeout(idle) {
        Ok(line) => line,
        Err(_) => Err(AppError::new(format!(
            "The server went silent for {}s; giving up (the question may still have been processed).",
            idle.as_secs_f32().round()
        ))),
    }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsafe_detail_codes_are_dropped() {
        use super::{is_safe_code, safe_detail_code};
        use serde_json::json;
        for ok in ["not_found", "FILE_TOO_LARGE", "a.b-c_9", &"a".repeat(64)] {
            assert!(is_safe_code(ok), "{ok}");
        }
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJpZCI6IjEifQ.c2lnbmF0dXJl";
        for bad in [
            "",
            &"a".repeat(65),
            "has space",
            "semi;colon",
            "x/y",
            jwt,
            "a\nb",
        ] {
            // JWT-like values are 'safe' charset-wise only if short: the long one fails by length.
            if bad == jwt {
                assert_eq!(is_safe_code(bad), jwt.len() <= 64);
                continue;
            }
            assert!(!is_safe_code(bad), "{bad:?}");
        }
        let long_jwt = format!("{jwt}{jwt}");
        assert_eq!(safe_detail_code(&json!({"detail":{"code":long_jwt}})), None);
        assert_eq!(
            safe_detail_code(&json!({"detail":{"code":"two words"}})),
            None
        );
        assert_eq!(
            safe_detail_code(&json!({"detail":{"code":"not_found"}})).as_deref(),
            Some("not_found")
        );
        assert_eq!(safe_detail_code(&json!({"detail":"plain"})), None);
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn silent_stream_times_out_instead_of_hanging() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Some(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}".into(),
        )))
        .unwrap();
        // tx stays alive and silent
        let err = parse_with_idle(&rx, Duration::from_millis(50)).unwrap_err();
        assert!(err.to_string().contains("went silent"), "{err}");
        drop(tx);
    }

    #[test]
    fn stream_that_ends_is_parsed() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(Some(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}".into(),
        )))
        .unwrap();
        tx.send(Ok(None)).unwrap();
        assert_eq!(
            parse_with_idle(&rx, Duration::from_secs(5)).unwrap().answer,
            "hi"
        );
    }
}

#[cfg(test)]
mod status_label_tests {
    use super::status_label;

    #[test]
    fn keeps_only_safe_code() {
        assert_eq!(
            status_label(404, r#"{"detail":{"code":"not_found"}}"#),
            "HTTP 404 (not_found)"
        );
        assert_eq!(
            status_label(500, r#"{"detail":{"code":"Bearer eyJ x"}}"#),
            "HTTP 500"
        );
        assert_eq!(
            status_label(502, "<html>SECRET-TOKEN-MARKER</html>"),
            "HTTP 502"
        );
    }
}
