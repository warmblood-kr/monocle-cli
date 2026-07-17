//! Client for jarvice's custom **"Responses API"** (`POST /api/responses`).
//!
//! This is NOT OpenAI's Responses API — it's a monocle-specific endpoint
//! (jarvice `backend/open_webui/routers/responses.py`) that is, under the
//! hood, still a plain chat completion. What earns it the "Responses API"
//! name is that the **server owns the conversation thread**: a caller sends
//! only the new turn (plus an optional `thread_id` to continue a thread the
//! server already persisted), instead of resending the full message history
//! the way a stateless `/v1/chat/completions` client must (see
//! `commands::chat`'s own accumulating `convo`, which is exactly the
//! bookkeeping this endpoint makes unnecessary).
//!
//! **jarvice-only.** Unlike `/v1/chat/completions` (reachable through
//! chat-proxy's `router_url`), this endpoint is not proxied — it requires
//! jarvice's own tenant-host-routed base URL (`auth::jarvice_url_for`).
//!
//! **Non-streaming only, for now.** jarvice streams token deltas over
//! Socket.IO, not HTTP SSE; this client always sends `"stream": false`, which
//! jarvice's endpoint honors by returning the full assembled reply
//! synchronously in the HTTP JSON body (confirmed against jarvice's
//! `create_response` → `process_chat_response`, the same non-streaming
//! branch the legacy `/api/chat/completions` path already uses) — so a plain
//! blocking-HTTP client needs no Socket.IO involvement at all.

use serde_json::{json, Value};

use crate::agent::providers::{ImageAttachment, ToolCall};
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

/// One turn's reply, plus the thread id to pass as `--resume`/the next turn's
/// `thread_id` to keep the conversation going. jarvice always resolves (and
/// returns) a thread id — creating one on the first turn of a session, or
/// confirming the one passed in — so this is `Option` only defensively, in
/// case a future response ever omits the header.
pub struct ResponsesReply {
    pub content: String,
    pub thread_id: Option<String>,
    /// Tool calls the model requested, surfaced but NOT executed
    /// (monocle-cli#101 / monocle#275): jarvice's `/api/responses` only runs
    /// its MCP tool-execution loop on the streaming branch, and this client
    /// always sends `"stream": false` (see the module docs), so a non-empty
    /// `tool_calls` here means the model's request went unexecuted. Callers
    /// must warn rather than silently drop it — actually executing tools in
    /// this mode is out of scope (tracked separately, monocle#274).
    pub tool_calls: Vec<ToolCall>,
}

/// Build the `input` field: a plain string when there's no attachment (the
/// common case, and the shape jarvice's own docs lead with), or a content-
/// block list (`input_text` + `input_image`) once at least one image is
/// attached. An image-only turn (empty `text`) omits the `input_text` block
/// rather than sending an empty one.
fn build_input(text: &str, images: &[ImageAttachment]) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut parts: Vec<Value> = Vec::new();
    if !text.is_empty() {
        parts.push(json!({"type": "input_text", "text": text}));
    }
    for img in images {
        parts.push(json!({"type": "input_image", "image_url": img.url}));
    }
    json!(parts)
}

pub struct ResponsesClient<'a> {
    client: &'a Client,
    token: String,
    jarvice_url: String,
}

impl<'a> ResponsesClient<'a> {
    pub fn new(
        client: &'a Client,
        token: impl Into<String>,
        jarvice_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            token: token.into(),
            jarvice_url: jarvice_url.into(),
        }
    }

    /// One non-streaming turn. `thread_id` is `None` to start a brand-new
    /// server-side thread (jarvice creates one and returns its id), or
    /// `Some(id)` to continue a thread from an earlier call.
    ///
    /// Note: jarvice's `/api/responses` has no generic system-prompt field
    /// (only a voice-mode `speech_system_context`) — a custom
    /// `--system-prompt` has nothing to attach to here, unlike the plain
    /// `/v1/chat/completions` path. Callers should warn and ignore it rather
    /// than silently dropping it.
    pub fn respond(
        &self,
        model: &str,
        text: &str,
        images: &[ImageAttachment],
        thread_id: Option<&str>,
    ) -> Result<ResponsesReply> {
        let input = build_input(text, images);

        let mut body = json!({
            "model": model,
            "input": input,
            "stream": false,
        });
        if let Some(id) = thread_id {
            body["thread"] = json!({"thread_id": id});
        }

        let bearer = format!("Bearer {}", self.token);
        let resp = self.client.post_json(
            &format!("{}/api/responses", self.jarvice_url),
            &auth_headers(&bearer),
            &body,
        )?;
        if !resp.ok() {
            return Err(AppError::new(format!(
                "Responses API error {}: {}",
                resp.status,
                resp.text()
            )));
        }
        let data: Value = resp.json()?;
        let thread_id = resp.header("x-thread-id").map(str::to_string);
        parse_reply(&data, thread_id)
    }
}

/// Parse a `/api/responses` JSON body into a `ResponsesReply`. Split out from
/// `respond` so the parsing — including the `tool_calls` extraction
/// (monocle-cli#101) — can be exercised directly against fixture JSON in
/// tests, without an HTTP round-trip.
fn parse_reply(data: &Value, thread_id: Option<String>) -> Result<ResponsesReply> {
    if data.get("choices").and_then(|c| c.get(0)).is_none() {
        return Err(AppError::new(format!(
            "unexpected /api/responses reply (no choices): {data}"
        )));
    }
    let message = &data["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default().to_string();
    let tool_calls: Vec<ToolCall> =
        serde_json::from_value(message["tool_calls"].clone()).unwrap_or_default();
    Ok(ResponsesReply {
        content,
        thread_id,
        tool_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_input_serializes_as_plain_string() {
        // The common case, and the shape jarvice's own docs lead with — no
        // content-block wrapping when there's nothing to attach.
        assert_eq!(build_input("hello", &[]), json!("hello"));
    }

    #[test]
    fn image_input_becomes_content_block_list() {
        let images = vec![ImageAttachment {
            url: "data:image/png;base64,AAAA".to_string(),
        }];
        assert_eq!(
            build_input("what is this?", &images),
            json!([
                {"type": "input_text", "text": "what is this?"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
            ])
        );
    }

    #[test]
    fn image_only_input_omits_empty_text_block() {
        let images = vec![ImageAttachment {
            url: "https://example.com/a.png".to_string(),
        }];
        assert_eq!(
            build_input("", &images),
            json!([{"type": "input_image", "image_url": "https://example.com/a.png"}])
        );
    }

    /// Build a minimal `/api/responses`-shaped body, optionally with a
    /// `tool_calls` array on `choices[0].message`.
    fn responses_body(content: &str, tool_calls: Option<Value>) -> Value {
        let mut message = json!({"content": content});
        if let Some(tc) = tool_calls {
            message["tool_calls"] = tc;
        }
        json!({"choices": [{"message": message}]})
    }

    #[test]
    fn tool_calls_empty_when_absent() {
        // The common, no-tool-calls case: `tool_calls` must come back empty,
        // not error, when the field is simply missing from the message.
        let data = responses_body("hello", None);
        let reply = parse_reply(&data, None).unwrap();
        assert_eq!(reply.content, "hello");
        assert!(reply.tool_calls.is_empty());
    }

    #[test]
    fn tool_calls_populated_when_present() {
        // monocle-cli#101: a non-streaming /api/responses reply can legitimately
        // carry an unexecuted tool_calls array — it must round-trip intact into
        // `ResponsesReply.tool_calls` so the caller can warn instead of
        // silently dropping it.
        let data = responses_body(
            "",
            Some(json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"},
            }])),
        );
        let reply = parse_reply(&data, Some("thread_1".to_string())).unwrap();
        assert_eq!(reply.thread_id.as_deref(), Some("thread_1"));
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].id, "call_1");
        assert_eq!(reply.tool_calls[0].function.name, "read_file");
        assert_eq!(
            reply.tool_calls[0].function.arguments,
            "{\"path\":\"a.txt\"}"
        );
    }

    #[test]
    fn multiple_images_preserve_order() {
        let images = vec![
            ImageAttachment {
                url: "data:image/png;base64,AAAA".to_string(),
            },
            ImageAttachment {
                url: "https://example.com/b.png".to_string(),
            },
        ];
        let input = build_input("compare these", &images);
        let parts = input.as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1]["image_url"], json!("data:image/png;base64,AAAA"));
        assert_eq!(parts[2]["image_url"], json!("https://example.com/b.png"));
    }
}
