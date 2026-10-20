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

/// `DELETE /api/v1/chats/{id}` — single attempt (never auto-retried). jarvice
/// answers `true`/`false`; `false` is a failure. A missing or foreign id yields
/// HTTP 401 (not 404), and a non-owner's delete may be reported as success while
/// removing nothing, so 401 is surfaced as "not found / not yours".
pub fn delete_thread(client: &Client, jarvice_url: &str, bearer: &str, id: &str) -> Result<()> {
    // Same path-splice guard as `get_thread`; valid ids need no percent-encoding.
    if !is_valid_chat_id(id) {
        return Err(AppError::new("Invalid chat thread ID."));
    }
    let resp = client.delete_once(
        &format!("{jarvice_url}/api/v1/chats/{id}"),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        let mut err = crate::jarvice_knowledge::api_error("Deleting the chat", &resp).to_string();
        if resp.status == 401 {
            err.push_str(" HTTP 401 can mean the chat was not found or is not yours.");
        }
        if resp.status == 500 {
            // Bare 500 is ambiguous (missing/foreign id or a real server fault); never echo the body.
            err =
                "Deleting the chat failed (HTTP 500); the chat may not exist or may not be yours."
                    .to_string();
        }
        return Err(AppError::new(err));
    }
    if resp.json::<bool>().ok() != Some(true) {
        return Err(AppError::new(
            "Deleting the chat failed: the server did not confirm the deletion.",
        ));
    }
    Ok(())
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
pub fn render_text(
    title: &str,
    messages: &[&MessageNode],
    show_tools: bool,
    include_memory: bool,
) -> String {
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
            visible_body(&message_body(message, show_tools, include_memory))
        ));
        if !message.files.is_empty() {
            output.push_str("\n[Non-text attachment omitted]\n");
        }
    }
    output
}

/// One-line JSON form of [`render_text`] (same messages, root first).
pub fn render_json(
    id: &str,
    title: &str,
    messages: &[&MessageNode],
    show_tools: bool,
    include_memory: bool,
) -> String {
    let messages: Vec<Value> = messages
        .iter()
        .filter(|m| is_text_role(m))
        .map(|m| {
            let mut node = serde_json::json!({
                "role": m.role,
                "content": visible_body(&message_body(m, show_tools, include_memory)),
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
///
/// Tool arguments can carry credentials or personal data, so every value here
/// is redacted ([`redact_value`]) and length-capped; `truncated` says a cap
/// cut something, `parse_error` that the block was malformed (the entry is then
/// best-effort — a bad block is reported, never silently dropped).
#[derive(Debug, PartialEq)]
pub struct ToolCallInfo {
    pub name: String,
    pub model_id: Option<String>,
    pub report_to_parent: Option<String>,
    /// Redacted arguments as compact JSON (or redacted raw text when not JSON).
    pub arguments: String,
    pub truncated: bool,
    pub parse_error: Option<String>,
}

/// Per-field caps (bytes). Arguments get more room than the short identifiers.
const MAX_ARGUMENTS_BYTES: usize = 2048;
const MAX_FIELD_BYTES: usize = 500;
const REDACTED: &str = "[REDACTED]";

/// Cut `s` to at most `max` bytes on a char boundary; the flag says it was cut.
fn cap(s: String, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &s[..end]), true)
}

fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "authorization",
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api-key",
        "bearer",
        "credential",
        "cookie",
        "private_key",
    ]
    .iter()
    .any(|needle| k.contains(needle))
}

/// A bare word that looks like a credential: `sk-…`, a JWT (`eyJ…`), or a long
/// opaque token after `Bearer`.
fn looks_secret(word: &str) -> bool {
    let w =
        word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.');
    (w.starts_with("sk-") && w.len() >= 20) || (w.starts_with("eyJ") && w.len() >= 20)
}

/// Split into alternating separator / word parts. A separator is any run of
/// Unicode whitespace plus the literal two-character escapes `\t`, `\n`, `\r`
/// (a JSON string that was escaped twice still reads "Bearer\tsecret").
fn split_parts(text: &str) -> Vec<(bool, &str)> {
    let bytes = text.as_bytes();
    let sep_len_at = |i: usize| -> usize {
        if bytes[i] == b'\\' && matches!(bytes.get(i + 1), Some(b't' | b'n' | b'r')) {
            return 2;
        }
        match text[i..].chars().next() {
            Some(c) if c.is_whitespace() => c.len_utf8(),
            _ => 0,
        }
    };
    let mut parts = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let is_sep = sep_len_at(i) > 0;
        let start = i;
        while i < text.len() && (sep_len_at(i) > 0) == is_sep {
            i += if is_sep {
                sep_len_at(i)
            } else {
                text[i..].chars().next().unwrap().len_utf8()
            };
        }
        parts.push((is_sep, &text[start..i]));
    }
    parts
}

/// Redact credential-looking text inside a free-form string: the word after
/// `Bearer`/`Basic` (or a long one after `token`), `sk-…`/JWT words, and the
/// value of a sensitive key written `key=value`, `key: value` or `key = value`.
/// Words are separated by any whitespace (tabs, newlines, CRLF, runs), and the
/// original separators are kept.
fn redact_str(text: &str) -> String {
    let parts = split_parts(text);
    let next_word = |from: usize| {
        parts[from + 1..]
            .iter()
            .find(|(sep, _)| !sep)
            .map(|(_, w)| *w)
    };
    let mut out = String::with_capacity(text.len());
    let mut redact_next = false;
    let mut key_pending = false; // a bare sensitive key; its `=`/`:` may come next
    for (idx, (is_sep, word)) in parts.iter().enumerate() {
        if *is_sep {
            out.push_str(word);
            continue;
        }
        let lower = word.to_ascii_lowercase();
        let was_key_pending = std::mem::take(&mut key_pending);
        let this_redact_next = std::mem::take(&mut redact_next);
        if this_redact_next {
            out.push_str(REDACTED);
            // "Authorization: Bearer x" — keep going through the scheme word.
            redact_next = matches!(lower.as_str(), "bearer" | "basic");
        } else if was_key_pending && (word.starts_with('=') || word.starts_with(':')) {
            let sep = &word[..1];
            if word.len() == 1 {
                out.push_str(word);
                redact_next = true;
            } else {
                out.push_str(&format!("{sep}{REDACTED}"));
            }
        } else if looks_secret(word) {
            out.push_str(REDACTED);
        } else if let Some((k, v)) = word.split_once(['=', ':']) {
            if is_sensitive_key(k) {
                let sep = &word[k.len()..k.len() + 1];
                if v.is_empty() {
                    out.push_str(word);
                    redact_next = true;
                } else {
                    out.push_str(&format!("{k}{sep}{REDACTED}"));
                }
            } else {
                out.push_str(word);
            }
        } else if matches!(lower.as_str(), "bearer" | "basic")
            || (lower == "token" && next_word(idx).is_some_and(|w| w.len() >= 8))
        {
            out.push_str(word);
            redact_next = true;
        } else {
            out.push_str(word);
            key_pending = is_sensitive_key(word)
                && next_word(idx).is_some_and(|w| w.starts_with('=') || w.starts_with(':'));
        }
    }
    out
}

/// Redact a parsed JSON value: values under sensitive keys are replaced
/// wholesale; every other string goes through [`redact_str`].
fn redact_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, val)| {
                    let red = if is_sensitive_key(k) {
                        Value::String(REDACTED.to_string())
                    } else {
                        redact_value(val)
                    };
                    (k.clone(), red)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::String(s) => Value::String(redact_str(s)),
        other => other.clone(),
    }
}

impl ToolCallInfo {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "name": visible_text(&self.name),
            "model_id": self.model_id.as_deref().map(visible_text),
            "report_to_parent": self.report_to_parent.as_deref().map(visible_text),
            "arguments": visible_text(&self.arguments),
            "truncated": self.truncated,
            "parse_error": self.parse_error,
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
        let (args, cut) = cap(visible_text(&self.arguments), 200);
        line.push_str(&format!(" args={args}"));
        if self.truncated && !cut {
            line.push_str(" (truncated)");
        }
        if let Some(e) = &self.parse_error {
            line.push_str(&format!(" [parse_error: {e}]"));
        }
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

const OPEN_TAG: &str = "<details";
const CLOSE_TAG: &str = "</details>";

/// Byte offset just past the `</details>` that balances the `<details` at
/// `start`, counting nested opens. `None` when the block is never closed.
fn matching_close(content: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = start;
    loop {
        let next_open = content[at..].find(OPEN_TAG).map(|i| at + i);
        let next_close = content[at..].find(CLOSE_TAG).map(|i| at + i);
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                at = o + OPEN_TAG.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                at = c + CLOSE_TAG.len();
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => return None,
        }
    }
}

fn parse_tool_blocks(content: &str) -> Vec<ToolBlock> {
    const OPEN: &str = "<details type=\"tool_calls\"";
    let mut blocks = Vec::new();
    let mut at = 0;
    while let Some(rel) = content[at..].find(OPEN) {
        let start = at + rel;
        let mut errors: Vec<&str> = Vec::new();
        // The whole rest of the message is consumed when the block is cut off:
        // surfacing it as one flagged entry beats leaving half a tag in the text.
        let end = matching_close(content, start).unwrap_or_else(|| {
            errors.push("unterminated block");
            content.len()
        });
        let tag_end = match content[start..end].find('>') {
            Some(i) => start + i,
            None => {
                errors.push("unterminated opening tag");
                end
            }
        };
        let tag = &content[start..tag_end];
        let args_raw = tag_attr(tag, "arguments");
        let args = args_raw.and_then(parse_json_loose);
        let result = tag_attr(tag, "result").and_then(parse_json_loose);
        let find = |key: &str| {
            args.as_ref()
                .and_then(|v| find_key(v, key))
                .or_else(|| result.as_ref().and_then(|v| find_key(v, key)))
        };
        let name = tag_attr(tag, "name").map(html_unescape);
        if name.is_none() {
            errors.push("missing name");
        }
        let truncated = std::cell::Cell::new(false);
        let field = |raw: String, max: usize| {
            let (v, cut) = cap(redact_str(&raw), max);
            truncated.set(truncated.get() || cut);
            v
        };
        let arguments = match (&args, args_raw) {
            (Some(v), _) => {
                let (v, cut) = cap(redact_value(v).to_string(), MAX_ARGUMENTS_BYTES);
                truncated.set(truncated.get() || cut);
                v
            }
            (None, Some(raw)) => field(html_unescape(raw), MAX_ARGUMENTS_BYTES),
            (None, None) => String::new(),
        };
        let call = ToolCallInfo {
            name: field(name.unwrap_or_default(), MAX_FIELD_BYTES),
            model_id: find("model_id").map(|v| field(v, MAX_FIELD_BYTES)),
            report_to_parent: find("report_to_parent").map(|v| field(v, MAX_FIELD_BYTES)),
            arguments,
            truncated: truncated.get(),
            parse_error: (!errors.is_empty()).then(|| errors.join(", ")),
        };
        blocks.push(ToolBlock { start, end, call });
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

/// Blocks jarvice injects into user messages and persists with them: the
/// account's personal memory and snippets of past conversations. jarvice's own
/// UI strips both; neither is something the user typed.
const INJECTED_TAGS: [&str; 2] = ["user_memory", "past_conversations"];

/// If `text` starts with an injected-block tag (`<user_memory>`,
/// `</past_conversations>`, …), return `(is_closing, byte length of the tag)`.
/// Matching is ASCII case-insensitive and allows whitespace inside the angle
/// brackets (`< user_memory >`, `</ USER_MEMORY >`), so a variant spelling
/// cannot slip a block past the filter.
fn injected_tag_at(text: &str) -> Option<(bool, usize)> {
    let skip_ws = |s: &str| s.len() - s.trim_start().len();
    let mut at = text.strip_prefix('<').map(|_| 1)?;
    at += skip_ws(&text[at..]);
    let closing = text[at..].starts_with('/');
    if closing {
        at += 1 + skip_ws(&text[at + 1..]);
    }
    let name = INJECTED_TAGS.iter().find(|t| {
        text.as_bytes()
            .get(at..at + t.len())
            .is_some_and(|b| b.eq_ignore_ascii_case(t.as_bytes()))
    })?;
    at += name.len();
    at += skip_ws(&text[at..]);
    text[at..].starts_with('>').then_some((closing, at + 1))
}

/// Remove the injected `<user_memory>` and `<past_conversations>` blocks
/// (multi-line, anywhere in the text). Blocks nest by depth: an opening tag
/// increments, a closing tag decrements, and everything is dropped until the
/// depth is back to 0, so `<user_memory>a<user_memory>b</user_memory>c</user_memory>`
/// leaks neither `b` nor `c`. A block that is never closed drops everything to
/// the end (fail closed: a partial block is never printed). A stray closing tag
/// is dropped. Whitespace around a removed block is collapsed so no blank gap
/// is left.
pub fn strip_injected_context(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut after_block = false;
    let mut i = 0;
    while i < text.len() {
        let next = text[i..].find('<').map_or(text.len(), |p| i + p);
        if depth == 0 && next > i {
            push_visible(&mut out, &text[i..next], &mut after_block);
        }
        i = next;
        if i >= text.len() {
            break;
        }
        match injected_tag_at(&text[i..]) {
            Some((false, len)) => {
                if depth == 0 {
                    out.truncate(out.trim_end().len());
                }
                depth += 1;
                i += len;
            }
            Some((true, len)) => {
                depth = depth.saturating_sub(1);
                after_block = depth == 0;
                i += len;
            }
            None => {
                if depth == 0 {
                    push_visible(&mut out, "<", &mut after_block);
                }
                i += 1;
            }
        }
    }
    out
}

/// Append `chunk` to `out`; straight after a removed block, drop leading
/// whitespace and separate from earlier text with one blank line.
fn push_visible(out: &mut String, chunk: &str, after_block: &mut bool) {
    let chunk = if *after_block {
        chunk.trim_start()
    } else {
        chunk
    };
    if chunk.is_empty() {
        return;
    }
    if *after_block && !out.is_empty() {
        out.push_str("\n\n");
    }
    *after_block = false;
    out.push_str(chunk);
}

/// Message body for one rendered message: the stored content, without the
/// injected `<user_memory>` block of a user message unless `include_memory`.
fn message_body(message: &MessageNode, show_tools: bool, include_memory: bool) -> String {
    let body = body_text(&message.content, show_tools);
    if include_memory || message.role != "user" {
        body
    } else {
        strip_injected_context(&body)
    }
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
    include_memory: bool,
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
        ContentFormat::Text => render_text(&detail.title, &active, show_tools, include_memory),
        ContentFormat::Json => render_json(id, &detail.title, &active, show_tools, include_memory),
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
        let out = render_text("title", &[&user, &system, &assistant], false, false);
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
        let out = render_json("c1", "T", &[&user, &system, &assistant], false, false);
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
        let text = render_text("T\x1b", &[&user], false, false);
        assert!(text.contains("## User\n\na[2Jbc\nd\te\n"), "{text:?}");
        assert!(!text.contains('\x1b') && !text.contains('\u{9b}') && !text.contains('\x07'));
        let v: Value =
            serde_json::from_str(&render_json("c1", "T", &[&user], false, false)).unwrap();
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

    // ── tool-call block parsing (--show-tools) ──────────────────────────────

    /// A block the way jarvice stores it: attribute values are a JSON string
    /// holding JSON, then HTML-escaped.
    fn block(name: &str, args: &Value, result: &Value) -> String {
        let attr = |v: &Value| {
            let inner = serde_json::to_string(&v.to_string()).unwrap();
            inner
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        format!(
            "<details type=\"tool_calls\" done=\"true\" id=\"c1\" name=\"{name}\" arguments=\"{}\" result=\"{}\" files=\"\">\n<summary>Tool Executed</summary>\n</details>",
            attr(args),
            attr(result)
        )
    }

    fn only(content: &str) -> ToolCallInfo {
        let mut blocks = parse_tool_blocks(content);
        assert_eq!(blocks.len(), 1, "{content}");
        blocks.remove(0).call
    }

    #[test]
    fn nested_details_stay_inside_their_block() {
        let inner = "<details><summary>x</summary>inner</details>";
        let content = format!(
            "before\n{}after",
            block("t", &serde_json::json!({}), &serde_json::json!({}))
                .replace("</details>", &format!("{inner}</details>"))
        );
        let blocks = parse_tool_blocks(&content);
        assert_eq!(blocks.len(), 1);
        let text = body_text(&content, true);
        assert_eq!(
            text,
            format!("before\n{}\nafter", blocks[0].call.summary_line())
        );
        assert!(!text.contains("details>"), "{text}");
        assert!(blocks[0].call.parse_error.is_none());
    }

    #[test]
    fn truncated_block_is_reported_not_dropped() {
        let full = block(
            "web_search",
            &serde_json::json!({"q": "a"}),
            &serde_json::json!({}),
        );
        let cut = &full[..full.len() - "</details>".len() - 5];
        let content = format!("intro\n{cut}");
        let call = only(&content);
        assert_eq!(call.name, "web_search");
        assert!(call
            .parse_error
            .as_deref()
            .unwrap()
            .contains("unterminated block"));
        let text = body_text(&content, true);
        assert!(text.contains("[parse_error: unterminated block]"), "{text}");
        assert!(!text.contains("<details"), "{text}");
    }

    #[test]
    fn opening_tag_without_close_bracket_is_reported() {
        let call = only("<details type=\"tool_calls\" name=\"half\"");
        assert!(call
            .parse_error
            .as_deref()
            .unwrap()
            .contains("unterminated opening tag"));
        assert_eq!(call.name, "half");
    }

    #[test]
    fn missing_name_is_reported_and_missing_report_to_parent_is_none() {
        let content = "<details type=\"tool_calls\" done=\"true\">\n</details>";
        let call = only(content);
        assert!(call
            .parse_error
            .as_deref()
            .unwrap()
            .contains("missing name"));
        assert_eq!(call.report_to_parent, None);
        assert_eq!(call.to_json()["report_to_parent"], Value::Null);
    }

    #[test]
    fn long_arguments_are_capped_and_flagged() {
        let big = "é".repeat(5000); // multi-byte: the cut must land on a char boundary
        let call = only(&block(
            "t",
            &serde_json::json!({ "q": big }),
            &serde_json::json!({}),
        ));
        assert!(call.truncated);
        assert!(call.arguments.len() <= MAX_ARGUMENTS_BYTES + "…".len());
        let j = call.to_json();
        assert_eq!(j["truncated"], true);
        let short = only(&block(
            "t",
            &serde_json::json!({"q": "x"}),
            &serde_json::json!({}),
        ));
        assert!(!short.truncated);
        assert_eq!(short.to_json()["truncated"], false);
    }

    #[test]
    fn secrets_are_redacted_in_json_and_text() {
        let args = serde_json::json!({
            "query": "ok",
            "api_key": "KEYVALUE1",
            "headers": {"Authorization": "Bearer HEADERTOKEN"},
            "note": "call with Bearer FREETOKEN please",
            "pasted": "sk-abcdefghijklmnopqrstuvwx",
            "cfg": "password=hunter2 mode=fast",
        });
        let content = block("t", &args, &serde_json::json!({}));
        let call = only(&content);
        for leaked in [
            "KEYVALUE1",
            "HEADERTOKEN",
            "FREETOKEN",
            "sk-abcdefghijkl",
            "hunter2",
        ] {
            assert!(
                !call.arguments.contains(leaked),
                "{leaked} leaked: {}",
                call.arguments
            );
            assert!(
                !body_text(&content, true).contains(leaked),
                "{leaked} leaked in text"
            );
        }
        assert!(
            call.arguments.contains("\"query\":\"ok\""),
            "{}",
            call.arguments
        );
        assert!(call.arguments.contains("mode=fast"), "{}", call.arguments);
    }

    #[test]
    fn special_characters_survive_and_controls_are_stripped() {
        let args = serde_json::json!({ "q": "a & b <c> \"d\" 한글 🙂 \u{1b}[31m" });
        let call = only(&block("name&amp;x", &args, &serde_json::json!({})));
        assert_eq!(call.name, "name&x");
        let line = call.summary_line();
        assert!(line.contains("한글 🙂"), "{line}");
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(!call.to_json()["arguments"]
            .as_str()
            .unwrap()
            .contains('\u{1b}'));
    }

    #[test]
    fn redaction_is_whitespace_class_and_case_insensitive() {
        let cases = [
            "Bearer\tSECRET1",
            "Bearer\nSECRET1",
            "Bearer\r\nSECRET1",
            "Bearer   SECRET1",
            "bEaReR SECRET1",
            "BEARER\u{a0}SECRET1",
            "Bearer\\tSECRET1",
            "Bearer\\nSECRET1",
            "Basic\tSECRET1",
            "Authorization:\tBearer\nSECRET1",
            "password:\tSECRET1",
            "password =\nSECRET1",
            "password\t=\tSECRET1",
            "PassWord=SECRET1",
            "token\tSECRET1xxxx",
        ];
        for c in cases {
            let out = redact_str(c);
            assert!(!out.contains("SECRET1"), "{c:?} leaked: {out:?}");
            assert!(out.contains(REDACTED), "{c:?}: {out:?}");
        }
        // separators are preserved and ordinary prose is untouched
        assert_eq!(redact_str("a\tb\r\nc  d"), "a\tb\r\nc  d");
        assert_eq!(
            redact_str("reset your password page"),
            "reset your password page"
        );
        assert_eq!(
            redact_str("mode=fast  speed: high"),
            "mode=fast  speed: high"
        );
    }

    #[test]
    fn redaction_covers_json_escaped_whitespace_end_to_end() {
        // `Bearer<TAB>secret` as a real tab, and as the two characters `\` `t`.
        let args = serde_json::json!({
            "a": "x Bearer\tSECRET2",
            "b": "x Bearer\nSECRET2",
            "c": "x Bearer\\tSECRET2",
            "d": "password:\r\n SECRET2",
        });
        let content = block("t", &args, &serde_json::json!({}));
        let call = only(&content);
        assert!(!call.arguments.contains("SECRET2"), "{}", call.arguments);
        assert!(!body_text(&content, true).contains("SECRET2"));
    }

    const MEMORY_BLOCK: &str =
        "<user_memory>\n1. [2026-01-01]. SECRET-FACT-ONE\n2. SECRET-FACT-TWO\n</user_memory>";

    #[test]
    fn strip_injected_context_removes_multiline_block_and_keeps_the_rest() {
        let text = format!("{MEMORY_BLOCK}\n\nsay hi in one word");
        assert_eq!(strip_injected_context(&text), "say hi in one word");
        let mid = format!("before\n{MEMORY_BLOCK}\nafter");
        assert_eq!(strip_injected_context(&mid), "before\n\nafter");
        let twice = format!("{MEMORY_BLOCK} a {MEMORY_BLOCK} b");
        assert_eq!(strip_injected_context(&twice), "a\n\nb");
        assert_eq!(strip_injected_context("plain text"), "plain text");
    }

    #[test]
    fn strip_injected_context_removes_past_conversations_too() {
        let text = "<past_conversations>\nSECRET-PAST\n</past_conversations>\n\n<user_memory>\nSECRET-MEM\n</user_memory>\n\nhi";
        assert_eq!(strip_injected_context(text), "hi");
        let swapped = "<user_memory>a</user_memory>\n<past_conversations>b</past_conversations>\nq";
        assert_eq!(strip_injected_context(swapped), "q");
    }

    #[test]
    fn strip_injected_context_drops_everything_after_an_unclosed_tag() {
        let out = strip_injected_context("question\n<user_memory>\nSECRET-FACT-ONE still open");
        assert_eq!(out, "question");
    }

    #[test]
    fn strip_injected_context_matches_tags_case_insensitively() {
        for text in [
            "<USER_MEMORY>SECRET</USER_MEMORY>\n\nhi",
            "<User_Memory>SECRET</user_memory>\n\nhi",
            "<PAST_Conversations>SECRET</past_CONVERSATIONS>\n\nhi",
        ] {
            assert_eq!(strip_injected_context(text), "hi", "{text}");
        }
    }

    #[test]
    fn strip_injected_context_allows_whitespace_inside_the_brackets() {
        for text in [
            "< user_memory >SECRET</ user_memory >\n\nhi",
            "<user_memory\n>SECRET<\t/ USER_MEMORY\n>\n\nhi",
            "<  past_conversations>SECRET</past_conversations  >hi",
        ] {
            assert_eq!(strip_injected_context(text), "hi", "{text}");
        }
    }

    #[test]
    fn strip_injected_context_counts_nesting_depth() {
        let text = "<user_memory>outer <user_memory>inner</user_memory> STILL_SECRET</user_memory>\n\nafter";
        assert_eq!(strip_injected_context(text), "after");
        // Mixed names share one depth counter.
        let mixed =
            "<user_memory>a<past_conversations>b</user_memory>STILL_SECRET</past_conversations>q";
        assert_eq!(strip_injected_context(mixed), "q");
    }

    #[test]
    fn strip_injected_context_fails_closed_on_unterminated_blocks() {
        for text in [
            "hi <user_memory>SECRET",
            "hi <user_memory>a<user_memory>b</user_memory>STILL_SECRET",
            "hi < PAST_CONVERSATIONS >SECRET",
        ] {
            assert_eq!(strip_injected_context(text), "hi", "{text}");
        }
    }

    #[test]
    fn strip_injected_context_handles_separate_blocks_and_keeps_surrounding_text() {
        let text = "intro\n<user_memory>A</user_memory>\nmiddle\n<past_conversations>B</past_conversations>\noutro <b>bold</b> a < b";
        assert_eq!(
            strip_injected_context(text),
            "intro\n\nmiddle\n\noutro <b>bold</b> a < b"
        );
        assert_eq!(
            strip_injected_context("no tags, 한국어 ✓"),
            "no tags, 한국어 ✓"
        );
    }

    #[test]
    fn render_strips_user_memory_from_user_messages_unless_included() {
        let user = node("1", None, "user", &format!("{MEMORY_BLOCK}\n\nhi"));
        let assistant = node("2", Some("1"), "assistant", "hello");
        let msgs = [&user, &assistant];
        for out in [
            render_text("T", &msgs, false, false),
            render_json("c1", "T", &msgs, false, false),
        ] {
            assert!(!out.contains("SECRET-FACT"), "{out}");
            assert!(!out.contains("user_memory"), "{out}");
            assert!(out.contains("hi") && out.contains("hello"));
        }
        let kept = render_text("T", &msgs, false, true);
        assert!(kept.contains("SECRET-FACT-ONE") && kept.contains("</user_memory>"));
        let kept = render_json("c1", "T", &msgs, false, true);
        assert!(kept.contains("SECRET-FACT-TWO"));
    }
}
