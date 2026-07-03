//! LLM provider abstraction — the **model-choice-freedom (G1)** seam.
//!
//! The agent loop depends on the `LlmProvider` trait, never on a specific vendor.
//! Swapping the model id (or the provider impl) changes the backing model without
//! touching the loop. `MonocleProvider` routes through monocle's chat-proxy
//! (OpenAI-compatible `/v1/chat/completions`), so monocle-model-router /
//! monocle-auto can select any model and the agent stays vendor-agnostic.
//!
//! The OpenAI Chat Completions wire format (incl. tool-calling shapes) is an
//! implementation detail of this provider; the loop sees only the normalized
//! types here (SDD §4a — wire format is a seam, not exposed upward).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::AuthSession;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

/// One chat message (OpenAI-compatible). `content` is optional because an
/// assistant message that only makes tool calls carries no text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    fn base(role: &str, content: Option<String>) -> Self {
        Self {
            role: role.to_string(),
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::base("user", Some(content.into()))
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::base("system", Some(content.into()))
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::base("assistant", Some(content.into()))
    }
    /// The assistant turn that issued tool calls — replayed into the conversation
    /// before the matching tool results so the API can correlate them.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        let mut m = Self::base("assistant", Some(content.into()));
        m.tool_calls = tool_calls;
        m
    }
    /// A tool result, keyed to the originating tool call.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let mut m = Self::base("tool", Some(content.into()));
        m.tool_call_id = Some(tool_call_id.into());
        m
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_kind() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments (a string, per the OpenAI wire format).
    pub arguments: String,
}

/// A tool definition advertised to the model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDef {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// A vendor-agnostic chat request. `model` is just an id the router understands —
/// the loop does not know or care which vendor it maps to.
#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: Option<i64>,
    pub tools: Vec<ToolDef>,
}

/// The assistant's reply for one turn.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// The model the backend actually served (echoed by the API when present).
    pub model: Option<String>,
    pub finish_reason: Option<String>,
}

/// Ensure every assembled tool call has a non-empty `id`, synthesizing a stable
/// `call_{i}` from position when the wire delivered an empty one. Downstream (ACP)
/// correlates `ToolCall` / permission / `ToolCallUpdate` by this id, so an empty id
/// would silently break that correlation. Non-empty ids are left untouched.
fn ensure_tool_call_ids(tool_calls: &mut [ToolCall]) {
    for (i, tc) in tool_calls.iter_mut().enumerate() {
        if tc.id.is_empty() {
            tc.id = format!("call_{i}");
        }
    }
}

/// Parse an OpenAI-compatible (non-streaming) chat completion body. Errors on a
/// body with no `choices` (e.g. an error-shaped response) rather than silently
/// returning an empty assistant turn that the loop would treat as a final answer.
fn parse_response_value(data: &Value) -> Result<ChatResponse> {
    if data.get("choices").and_then(|c| c.get(0)).is_none() {
        return Err(AppError::new(format!(
            "unexpected chat response (no choices): {data}"
        )));
    }
    let message = &data["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default().to_string();
    let mut tool_calls: Vec<ToolCall> =
        serde_json::from_value(message["tool_calls"].clone()).unwrap_or_default();
    ensure_tool_call_ids(&mut tool_calls);
    let model = data["model"].as_str().map(String::from);
    let finish_reason = data["choices"][0]["finish_reason"]
        .as_str()
        .map(String::from);
    Ok(ChatResponse {
        content,
        tool_calls,
        model,
        finish_reason,
    })
}

/// Accumulates a streamed tool call across SSE deltas (id/name arrive first,
/// arguments come in fragments).
#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

/// Assemble a `ChatResponse` from an OpenAI-compatible SSE stream, calling
/// `next_line` to pull each raw line and emitting content deltas via `on_delta`.
///
/// Read-error handling — graceful partial-output on a mid-stream drop
/// (monocle-cli#59): when `next_line` yields `Err`, if anything usable has
/// already been received (`content` non-empty OR a tool call has been started)
/// the stream is treated as **truncated-but-usable** — `finish_reason` is set to
/// `"stream_error"`, any in-progress tool calls are **dropped** (a tool call cut
/// off mid-stream is unreliable, so the caller gets clean partial text instead of
/// a malformed call), and the loop ends. Only when nothing has been received is
/// the error propagated (a genuine connection failure with nothing to salvage).
fn assemble_sse_stream(
    mut next_line: impl FnMut() -> Result<Option<String>>,
    on_delta: &mut dyn FnMut(&str),
) -> Result<ChatResponse> {
    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut model: Option<String> = None;
    let mut tool_acc: Vec<ToolCallAcc> = Vec::new();

    loop {
        let raw = match next_line() {
            Ok(Some(raw)) => raw,
            Ok(None) => break,
            Err(e) => {
                // Salvage a truncated-but-usable response if anything arrived.
                if !content.is_empty() || !tool_acc.is_empty() {
                    finish_reason = Some("stream_error".to_string());
                    tool_acc.clear();
                    break;
                }
                return Err(e);
            }
        };
        let data = match raw.trim_end().strip_prefix("data:") {
            Some(d) => d.trim_start().to_string(),
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if model.is_none() {
            model = v["model"].as_str().map(String::from);
        }
        let choice = &v["choices"][0];
        if let Some(fr) = choice["finish_reason"].as_str() {
            finish_reason = Some(fr.to_string());
        }
        let delta = &choice["delta"];
        if let Some(c) = delta["content"].as_str() {
            if !c.is_empty() {
                content.push_str(c);
                on_delta(c);
            }
        }
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                while tool_acc.len() <= idx {
                    tool_acc.push(ToolCallAcc::default());
                }
                let acc = &mut tool_acc[idx];
                if let Some(id) = tc["id"].as_str() {
                    if !id.is_empty() {
                        acc.id = id.to_string();
                    }
                }
                if let Some(name) = tc["function"]["name"].as_str() {
                    if !name.is_empty() {
                        acc.name = name.to_string();
                    }
                }
                if let Some(args) = tc["function"]["arguments"].as_str() {
                    acc.arguments.push_str(args);
                }
            }
        }
    }

    let mut tool_calls: Vec<ToolCall> = tool_acc
        .into_iter()
        .filter(|a| !a.name.is_empty())
        .map(|a| ToolCall {
            id: a.id,
            kind: "function".to_string(),
            function: FunctionCall {
                name: a.name,
                arguments: a.arguments,
            },
        })
        .collect();
    ensure_tool_call_ids(&mut tool_calls);

    Ok(ChatResponse {
        content,
        tool_calls,
        model,
        finish_reason,
    })
}

/// The seam the agent loop is built on. Any backend (monocle-routed today, a
/// direct provider tomorrow) implements this; the loop never names a vendor.
pub trait LlmProvider {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;

    /// Streaming variant: emits assistant text deltas via `on_delta` as they
    /// arrive, returning the assembled final response. Default delegates to
    /// `chat` and emits the whole content once (fine for tests / non-streaming).
    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatResponse> {
        let resp = self.chat(req)?;
        if !resp.content.is_empty() {
            on_delta(&resp.content);
        }
        Ok(resp)
    }
}

/// `LlmProvider` backed by monocle routing (chat-proxy, OpenAI-compatible).
pub struct MonocleProvider {
    client: Client,
    token: String,
    router_url: String,
}

impl MonocleProvider {
    pub fn new(token: impl Into<String>, router_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            token: token.into(),
            router_url: router_url.into(),
        }
    }

    /// Build from a resolved auth session (token + router URL).
    pub fn from_session(session: AuthSession) -> Self {
        Self::new(session.token, session.router_url)
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let mut body = json!({
            "model": req.model,
            "messages": req.messages,
            "stream": stream,
        });
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools);
        }
        body
    }
}

impl LlmProvider for MonocleProvider {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let bearer = format!("Bearer {}", self.token);
        let resp = self.client.post_json(
            &format!("{}{}", self.router_url, endpoints::CHAT_COMPLETIONS),
            &auth_headers(&bearer),
            &self.build_body(req, false),
        )?;
        if !resp.ok() {
            return Err(AppError::new(format!(
                "API error {}: {}",
                resp.status,
                resp.text()
            )));
        }
        let data: Value = resp.json()?;
        parse_response_value(&data)
    }

    fn chat_stream(
        &self,
        req: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatResponse> {
        let bearer = format!("Bearer {}", self.token);
        let mut stream = self.client.post_json_stream(
            &format!("{}{}", self.router_url, endpoints::CHAT_COMPLETIONS),
            &auth_headers(&bearer),
            &self.build_body(req, true),
        )?;

        if !stream.ok() {
            let status = stream.status;
            let body = stream.read_all();
            return Err(AppError::new(format!("API error {status}: {body}")));
        }

        // Non-SSE response (or a stub): buffer and parse as one completion.
        if !stream.is_event_stream() {
            let data: Value = serde_json::from_str(&stream.read_all())?;
            let resp = parse_response_value(&data)?;
            if !resp.content.is_empty() {
                on_delta(&resp.content);
            }
            return Ok(resp);
        }

        // SSE: assemble content + tool calls from `data:` deltas. A mid-stream
        // read error is salvaged into a truncated response (see the fn's docs).
        assemble_sse_stream(|| stream.next_line(), on_delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `assemble_sse_stream` from a fixed script of `next_line` outcomes,
    /// capturing every content delta the callback observed.
    fn run_stream(script: Vec<Result<Option<String>>>) -> (Result<ChatResponse>, Vec<String>) {
        let mut it = script.into_iter();
        let mut deltas: Vec<String> = Vec::new();
        let resp = assemble_sse_stream(|| it.next().unwrap_or(Ok(None)), &mut |d| {
            deltas.push(d.to_string())
        });
        (resp, deltas)
    }

    fn content_line(text: &str) -> Result<Option<String>> {
        Ok(Some(format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}"
        )))
    }

    #[test]
    fn mid_stream_drop_preserves_partial() {
        let (resp, deltas) = run_stream(vec![
            content_line("Hello"),
            content_line(" world"),
            Err(AppError::new("error decoding response body")),
        ]);
        let resp = resp.expect("partial content should be salvaged, not error");
        assert_eq!(resp.content, "Hello world");
        assert_eq!(resp.finish_reason.as_deref(), Some("stream_error"));
        assert!(resp.tool_calls.is_empty());
        assert_eq!(deltas, vec!["Hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn immediate_error_propagates() {
        let (resp, _deltas) = run_stream(vec![Err(AppError::new("error decoding response body"))]);
        assert!(
            resp.is_err(),
            "an error before any content should propagate"
        );
    }

    #[test]
    fn normal_completion_assembles_content() {
        let (resp, deltas) = run_stream(vec![
            content_line("Hello"),
            content_line(" world"),
            Ok(Some("data: [DONE]".to_string())),
            Ok(None),
        ]);
        let resp = resp.expect("normal completion should succeed");
        assert_eq!(resp.content, "Hello world");
        assert_ne!(resp.finish_reason.as_deref(), Some("stream_error"));
        assert!(resp.tool_calls.is_empty());
        assert_eq!(deltas, vec!["Hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn truncation_mid_tool_call_drops_the_tool_call() {
        // A partial tool_call delta (name + start of arguments), then a drop.
        let tool_delta = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
            \"id\":\"call_abc\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}";
        let (resp, _deltas) = run_stream(vec![
            Ok(Some(tool_delta.to_string())),
            Err(AppError::new("error decoding response body")),
        ]);
        let resp = resp.expect("a started tool call should salvage as partial, not error");
        assert!(
            resp.tool_calls.is_empty(),
            "an interrupted tool call must be dropped"
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("stream_error"));
    }
}
