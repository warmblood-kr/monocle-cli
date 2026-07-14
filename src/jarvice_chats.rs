//! Client for jarvice's existing chat-thread storage (`/api/v1/chats`), used
//! to let `monocle chat --responses` discover and replay threads that already
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
    pub content: String,
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
    #[allow(dead_code)] // not surfaced yet; kept for future use (e.g. a header)
    title: String,
    chat: ChatBlob,
}

/// One thread's full history, already unwrapped past jarvice's `chat.history`
/// envelope — ready for [`linearize`].
pub struct ChatDetail {
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
            "API error {}: {}",
            resp.status,
            resp.text()
        )));
    }
    resp.json()
}

/// `GET /api/v1/chats/{id}` — one thread's full nested message tree.
pub fn get_thread(
    client: &Client,
    jarvice_url: &str,
    bearer: &str,
    id: &str,
) -> Result<ChatDetail> {
    let resp = client.get(
        &format!("{jarvice_url}/api/v1/chats/{id}"),
        &auth_headers(bearer),
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "API error {}: {}",
            resp.status,
            resp.text()
        )));
    }
    let parsed: ChatDetailResponse = resp.json()?;
    Ok(ChatDetail {
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
    let mut cursor = Some(current_id.to_string());
    while let Some(id) = cursor {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, role: &str, content: &str) -> MessageNode {
        MessageNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            content: content.to_string(),
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
}
