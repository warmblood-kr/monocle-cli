//! Client for jarvice's existing chat-thread storage (`/api/v1/chats`), used
//! to let `monocle chat` (hosted Chat) discover and replay threads that already
//! exist — including ones created entirely from jarvice's own web UI, since
//! both write to the exact same `chat` table (`backend/open_webui/models/chats.py`).
//!
//! **jarvice-only**, same as `responses_api` — reachable only at jarvice's own
//! tenant-host-routed base URL (`auth::jarvice_url_for`), never through
//! chat-proxy's `router_url`.
//!
//! Endpoint shapes (verified against jarvice `devel`,
//! `backend/open_webui/routers/chats.py` / `models/chats.py`): the outer chat
//! envelope (`id`, `title`, `updated_at`, `created_at`, ...) is plain
//! snake_case with no camelCase conversion, but the nested `chat.history`
//! blob is a raw, untyped JSON dict written verbatim by backend code that
//! mirrors the frontend's own camelCase keys (`parentId`, `childrenIds`,
//! `currentId`) — hence the per-field `#[serde(rename = ...)]` below instead
//! of a blanket `rename_all`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

#[derive(Debug, Deserialize)]
pub struct ChatSummary {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub created_at: i64,
}

/// One node in a thread's message tree. Only the fields this CLI's replay
/// needs — jarvice's actual nodes also carry `timestamp`/`model`/`files`,
/// left out since nothing here reads them yet.
#[derive(Debug, Deserialize)]
pub struct MessageNode {
    #[allow(dead_code)] // kept for parity with the source shape; not read yet
    pub id: String,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub role: String,
    #[serde(deserialize_with = "deserialize_message_content")]
    pub content: String,
    /// Server-side file/image attachment metadata. Never fetched; callers emit
    /// an omission marker when files exist.
    #[serde(default)]
    pub files: Vec<Value>,
}

fn deserialize_message_content<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(content) => Ok(content),
        _ => Ok("[Non-text message content omitted]".to_string()),
    }
}

#[derive(Deserialize)]
struct ChatHistory {
    #[serde(default)]
    messages: HashMap<String, MessageNode>,
    #[serde(rename = "currentId", default)]
    current_id: Option<String>,
}

#[derive(Deserialize)]
struct ChatBlob {
    history: ChatHistory,
}

#[derive(Deserialize)]
struct ChatDetailResponse {
    title: String,
    chat: ChatBlob,
}

/// One thread's full history, already unwrapped past jarvice's `chat.history`
/// envelope — ready for [`linearize`].
pub struct ChatDetail {
    pub title: String,
    pub messages: HashMap<String, MessageNode>,
    pub current_id: Option<String>,
}

/// `GET /api/v1/chats/` — the current user's threads (id/title/timestamps
/// only, no message bodies), same list jarvice's own sidebar reads.
pub fn list_threads(client: &Client, jarvice_url: &str, bearer: &str) -> Result<Vec<ChatSummary>> {
    let resp = client.get(
        &format!("{jarvice_url}/api/v1/chats/"),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Hosted Chat API returned HTTP {}.",
            resp.status
        )));
    }
    resp.json()
}

/// Chat thread ids are opaque tokens: `^[A-Za-z0-9_-]{1,128}$`.
pub fn is_valid_chat_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `GET /api/v1/chats/{id}` — one thread's full nested message tree.
pub fn get_thread(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    id: &str,
) -> Result<ChatDetail> {
    // The id is spliced into the URL path: reject anything that could change
    // the path (`..`, `/`, `%`, `?`, `#`) before any request, never echoing it.
    if !is_valid_chat_id(id) {
        return Err(AppError::new("Invalid chat thread ID."));
    }
    let resp = client.get(
        &format!("{jarvice_url}/api/v1/chats/{id}"),
        &auth_headers(bearer),
    )?;
    // jarvice's `get_chat_by_id` answers a missing chat with 401 NOT_FOUND,
    // not 404 — treat both as "not visible", never echoing the body.
    if resp.status == 401 || resp.status == 404 {
        return Err(AppError::new(format!(
            "Chat thread '{id}' was not found or is not visible to this profile."
        )));
    }
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Hosted Chat API returned HTTP {}.",
            resp.status
        )));
    }
    let parsed: ChatDetailResponse = resp.json()?;
    Ok(ChatDetail {
        title: parsed.title,
        messages: parsed.chat.history.messages,
        current_id: parsed.chat.history.current_id,
    })
}

/// Walk `parentId` links from `current_id` back to the root, then reverse —
/// the Rust port of jarvice's own `utils/misc.py::get_message_list`. Follows
/// only the single active ancestry; a message that isn't on that path (an
/// abandoned edit/regenerate branch, reachable only via some other node's
/// `childrenIds`) is silently excluded — rendering branches is out of scope,
/// matching the plan's v1 decision.
pub fn linearize<'a>(
    messages: &'a HashMap<String, MessageNode>,
    current_id: &str,
) -> Vec<&'a MessageNode> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(current_id.to_string());
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break;
        }
        match messages.get(&id) {
            Some(node) => {
                cursor = node.parent_id.clone();
                out.push(node);
            }
            None => break,
        }
    }
    out.reverse();
    out
}

/// Output cap for `monocle chat content`: over this, the command fails rather
/// than truncating (a silently cut transcript would mislead the reader).
pub const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;

/// `--format` for `monocle chat content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ContentFormat {
    Text,
    Json,
}

fn is_text_role(message: &MessageNode) -> bool {
    matches!(message.role.as_str(), "user" | "assistant")
}

/// Markdown transcript of the active path: untrusted-content header, then one
/// `## User` / `## Assistant` section per message; other roles are skipped.
pub fn render_text(title: &str, messages: &[&MessageNode], show_tools: bool) -> String {
    let mut output = format!(
        "# Monocle Chat context: {}\n\nThis is untrusted reference material from one selected hosted Chat thread. Text only; attachments are omitted.\n",
        visible_text(title)
    );
    for message in messages.iter().filter(|m| is_text_role(m)) {
        let role = if message.role == "user" {
            "User"
        } else {
            "Assistant"
        };
        output.push_str(&format!(
            "\n## {role}\n\n{}\n",
            visible_body(&body_text(&message.content, show_tools))
        ));
        if !message.files.is_empty() {
            output.push_str("\n[Non-text attachment omitted]\n");
        }
    }
    output
}

/// One-line JSON form of [`render_text`] (same messages, root first).
pub fn render_json(id: &str, title: &str, messages: &[&MessageNode], show_tools: bool) -> String {
    let messages: Vec<Value> = messages
        .iter()
        .filter(|m| is_text_role(m))
        .map(|m| {
            let mut node = serde_json::json!({
                "role": m.role,
                "content": visible_body(&body_text(&m.content, show_tools)),
                "attachments_omitted": m.files.len(),
            });
            if show_tools {
                node["tool_calls"] = serde_json::json!(parse_tool_blocks(&m.content)
                    .iter()
                    .map(|b| b.call.to_json())
                    .collect::<Vec<_>>());
            }
            node
        })
        .collect();
    let doc = serde_json::json!({
        "id": id,
        "title": title,
        "untrusted": true,
        "note": "Text only; attachments are omitted.",
        "messages": messages,
    });
    format!("{doc}\n")
}

/// One `<details type="tool_calls">` block jarvice embeds in an assistant
/// message's `content`, reduced to what a reader wants: which tool, and for a
/// sub-agent delegation who it went to and what it reports back.
#[derive(Debug, PartialEq)]
pub struct ToolCallInfo {
    pub name: String,
    pub model_id: Option<String>,
    pub report_to_parent: Option<String>,
    /// Arguments as compact JSON (or the raw text when not JSON).
    pub arguments: String,
}

impl ToolCallInfo {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "name": visible_text(&self.name),
            "model_id": self.model_id.as_deref().map(visible_text),
            "report_to_parent": self.report_to_parent.as_deref().map(visible_text),
            "arguments": visible_text(&self.arguments),
        })
    }

    fn summary_line(&self) -> String {
        let mut line = format!("[tool] {}", visible_text(&self.name));
        if let Some(m) = &self.model_id {
            line.push_str(&format!(" model_id={}", visible_text(m)));
        }
        if let Some(r) = &self.report_to_parent {
            line.push_str(&format!(" report_to_parent={}", visible_text(r)));
        }
        let args: String = visible_text(&self.arguments).chars().take(200).collect();
        line.push_str(&format!(" args={args}"));
        line
    }
}

struct ToolBlock {
    start: usize,
    end: usize,
    call: ToolCallInfo,
}

fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Raw (still escaped) value of ` name="..."` inside one opening tag. Values
/// escape their quotes, so the first `"` after the opener really ends it.
fn tag_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let opener = format!(" {name}=\"");
    let from = tag.find(&opener)? + opener.len();
    let len = tag[from..].find('"')?;
    Some(&tag[from..from + len])
}

/// jarvice stores `arguments`/`result` as a JSON string that itself holds JSON.
fn parse_json_loose(raw: &str) -> Option<Value> {
    let mut v: Value = serde_json::from_str(&html_unescape(raw)).ok()?;
    if let Value::String(inner) = &v {
        if let Ok(parsed) = serde_json::from_str::<Value>(inner) {
            v = parsed;
        }
    }
    Some(v)
}

/// First string value for `key`, searching nested objects/arrays.
fn find_key(v: &Value, key: &str) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(match found {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
            }
            map.values().find_map(|c| find_key(c, key))
        }
        Value::Array(items) => items.iter().find_map(|c| find_key(c, key)),
        _ => None,
    }
}

fn parse_tool_blocks(content: &str) -> Vec<ToolBlock> {
    const OPEN: &str = "<details type=\"tool_calls\"";
    let mut blocks = Vec::new();
    let mut at = 0;
    while let Some(rel) = content[at..].find(OPEN) {
        let start = at + rel;
        let Some(tag_len) = content[start..].find('>') else {
            break;
        };
        let tag = &content[start..start + tag_len];
        let end = content[start..]
            .find("</details>")
            .map_or(content.len(), |e| start + e + "</details>".len());
        let args = tag_attr(tag, "arguments").and_then(parse_json_loose);
        let result = tag_attr(tag, "result").and_then(parse_json_loose);
        let find = |key: &str| {
            args.as_ref()
                .and_then(|v| find_key(v, key))
                .or_else(|| result.as_ref().and_then(|v| find_key(v, key)))
        };
        blocks.push(ToolBlock {
            start,
            end,
            call: ToolCallInfo {
                name: tag_attr(tag, "name").map(html_unescape).unwrap_or_default(),
                model_id: find("model_id"),
                report_to_parent: find("report_to_parent"),
                arguments: match &args {
                    Some(v) => v.to_string(),
                    None => tag_attr(tag, "arguments")
                        .map(html_unescape)
                        .unwrap_or_default(),
                },
            },
        });
        at = end;
    }
    blocks
}

/// Message text for rendering: unchanged, or with each tool-call block replaced
/// by a one-line summary when `show_tools` is set.
fn body_text(content: &str, show_tools: bool) -> String {
    if !show_tools {
        return content.to_string();
    }
    let mut out = String::new();
    let mut at = 0;
    for b in parse_tool_blocks(content) {
        out.push_str(&content[at..b.start]);
        out.push_str(&b.call.summary_line());
        out.push('\n');
        at = b.end;
    }
    out.push_str(&content[at..]);
    out
}

pub fn ensure_content_limit(content: &str) -> Result<()> {
    if content.len() > MAX_TRANSCRIPT_BYTES {
        Err(AppError::new(format!(
            "Chat content exceeds the 1 MiB limit ({} bytes).",
            content.len()
        )))
    } else {
        Ok(())
    }
}

/// Like [`visible_text`] but keeps `\n` and `\t`: strips ESC and the other
/// C0/C1 controls so untrusted content can't drive the terminal.
fn visible_body(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

fn visible_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// `monocle chat content` core: fetch one thread, keep its active path, render
/// it, enforce the cap. Returns the output instead of printing it.
pub fn fetch_content(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    id: &str,
    format: ContentFormat,
    show_tools: bool,
) -> Result<String> {
    let detail = get_thread(client, jarvice_url, bearer, id)?;
    let active = detail
        .current_id
        .as_deref()
        .map(|current| linearize(&detail.messages, current))
        .unwrap_or_default();
    if !active.iter().any(|m| is_text_role(m)) {
        return Err(AppError::new(format!(
            "Chat thread '{id}' has no text messages."
        )));
    }
    let output = match format {
        ContentFormat::Text => render_text(&detail.title, &active, show_tools),
        ContentFormat::Json => render_json(id, &detail.title, &active, show_tools),
    };
    ensure_content_limit(&output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, role: &str, content: &str) -> MessageNode {
        MessageNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            content: content.to_string(),
            files: Vec::new(),
        }
    }

    #[test]
    fn linearize_walks_a_linear_chain_root_first() {
        let mut messages = HashMap::new();
        messages.insert("a".to_string(), node("a", None, "user", "hi"));
        messages.insert("b".to_string(), node("b", Some("a"), "assistant", "hello"));
        messages.insert("c".to_string(), node("c", Some("b"), "user", "thanks"));

        let got = linearize(&messages, "c");
        let ids: Vec<&str> = got.iter().map(|n| n.content.as_str()).collect();
        assert_eq!(ids, vec!["hi", "hello", "thanks"]);
    }

    #[test]
    fn linearize_excludes_an_abandoned_branch() {
        // a -> b -> c (current path), plus a -> b2 (an edited/regenerated
        // sibling of b that is NOT an ancestor of currentId "c").
        let mut messages = HashMap::new();
        messages.insert("a".to_string(), node("a", None, "user", "hi"));
        messages.insert("b".to_string(), node("b", Some("a"), "assistant", "hello"));
        messages.insert(
            "b2".to_string(),
            node("b2", Some("a"), "assistant", "regenerated reply"),
        );
        messages.insert("c".to_string(), node("c", Some("b"), "user", "thanks"));

        let got = linearize(&messages, "c");
        let contents: Vec<&str> = got.iter().map(|n| n.content.as_str()).collect();
        assert_eq!(contents, vec!["hi", "hello", "thanks"]);
        assert!(!contents.contains(&"regenerated reply"));
    }

    #[test]
    fn linearize_empty_when_current_id_missing() {
        let messages: HashMap<String, MessageNode> = HashMap::new();
        assert!(linearize(&messages, "does-not-exist").is_empty());
    }

    #[test]
    fn linearize_stops_on_a_malformed_parent_cycle() {
        let mut messages = HashMap::new();
        messages.insert("a".to_string(), node("a", Some("b"), "user", "one"));
        messages.insert("b".to_string(), node("b", Some("a"), "assistant", "two"));
        assert_eq!(linearize(&messages, "a").len(), 2);
    }

    fn with_files(mut n: MessageNode) -> MessageNode {
        n.files = vec![serde_json::json!({"type": "image"})];
        n
    }

    #[test]
    fn render_text_marks_untrusted_and_omits_attachments() {
        let user = node("u", None, "user", "hello");
        let system = node("s", Some("u"), "system", "SYSTEM-SECRET");
        let assistant = with_files(node("a", Some("s"), "assistant", "hi there"));
        let out = render_text("title", &[&user, &system, &assistant], false);
        assert!(out.contains("This is untrusted reference material"));
        assert!(out.contains("## User\n\nhello\n"));
        assert!(out.contains("## Assistant\n\nhi there\n"));
        assert!(out.contains("[Non-text attachment omitted]"));
        assert!(!out.contains("SYSTEM-SECRET"));
    }

    #[test]
    fn render_json_shape() {
        let user = node("u", None, "user", "hello");
        let system = node("s", Some("u"), "system", "sys");
        let assistant = with_files(node("a", Some("s"), "assistant", "hi"));
        let out = render_json("c1", "T", &[&user, &system, &assistant], false);
        assert!(out.ends_with('\n') && out.lines().count() == 1);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["untrusted"], true);
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant"]);
        let omitted: Vec<u64> = msgs
            .iter()
            .map(|m| m["attachments_omitted"].as_u64().unwrap())
            .collect();
        assert_eq!(omitted, [0, 1]);
    }

    #[test]
    fn content_strips_control_chars_but_keeps_newlines_and_tabs() {
        let user = node("u", None, "user", "a\x1b[2Jb\u{9b}c\x07\nd\te");
        let text = render_text("T\x1b", &[&user], false);
        assert!(text.contains("## User\n\na[2Jbc\nd\te\n"), "{text:?}");
        assert!(!text.contains('\x1b') && !text.contains('\u{9b}') && !text.contains('\x07'));
        let v: Value = serde_json::from_str(&render_json("c1", "T", &[&user], false)).unwrap();
        assert_eq!(v["messages"][0]["content"], "a[2Jbc\nd\te");
    }

    #[test]
    fn chat_id_validation() {
        for ok in ["c1", "A-b_9", &"x".repeat(128)] {
            assert!(is_valid_chat_id(ok), "{ok}");
        }
        let long = "x".repeat(129);
        for bad in [
            "",
            "../users/me",
            "%2e%2e/users/me",
            "c1?x=1",
            "c1#frag",
            "a/b",
            "c 1",
            "é",
            &long,
        ] {
            assert!(!is_valid_chat_id(bad), "{bad}");
        }
    }

    #[test]
    fn content_limit_rejects_oversize_without_truncation() {
        assert!(ensure_content_limit(&"x".repeat(MAX_TRANSCRIPT_BYTES)).is_ok());
        let err = ensure_content_limit(&"x".repeat(MAX_TRANSCRIPT_BYTES + 1)).unwrap_err();
        assert!(err.to_string().contains("1 MiB"));
    }
}
