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
    /// Images to attach to the last user message as OpenAI vision parts (see
    /// `MonocleProvider::build_body`). Carried on the request rather than
    /// `Message` because attachments are per-turn: both `monocle chat` and
    /// `monocle agent` grow `messages` across turns, but only the CURRENT
    /// turn's images are ever passed here — an earlier turn's images are not
    /// re-embedded into later requests (the assistant's textual reply about
    /// them is what persists in history).
    pub images: Vec<ImageAttachment>,
}

/// One resolved image, ready to drop into an OpenAI `image_url` content part —
/// either a `data:<mime>;base64,...` URI (local file) or a passthrough remote
/// `http(s)://` URL.
#[derive(Debug, Clone)]
pub struct ImageAttachment {
    pub url: String,
}

/// The assistant's reply for one turn.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// The model the backend actually served (echoed by the API when present).
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    /// `true` only when the stream dropped **mid-generation** (before the model
    /// sent a `finish_reason`) and partial text was salvaged — a typed signal so
    /// callers can flag the output as cut short without pattern-matching on a
    /// magic `finish_reason` string. A complete-but-then-dropped stream (the
    /// model already sent `finish_reason`) is NOT truncated.
    pub truncated: bool,
    /// Token counts, when the backend reports them. Always present on a
    /// non-streaming reply (the OpenAI-compatible `usage` object is part of
    /// the standard, non-opt-in response shape); for streaming, only present
    /// because `MonocleProvider::build_body` opts in via
    /// `stream_options.include_usage` — see that fn's doc comment.
    pub usage: Option<TokenUsage>,
}

/// Token counts for one turn (OpenAI-compatible `usage` object:
/// `prompt_tokens` + `completion_tokens` = `total_tokens`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Parse an OpenAI-compatible `usage` object off a response/chunk body, if
/// present. Shared by the non-streaming parser and the streaming assembler's
/// final-chunk handling.
fn parse_usage(data: &Value) -> Option<TokenUsage> {
    // `.unwrap_or(0)` handles a missing/non-numeric field; the `u32::try_from`
    // below handles a present-but-oversized one (e.g. a malformed upstream
    // payload) by saturating to `u32::MAX` instead of silently wrapping to a
    // small, wrong number via an `as u32` truncation.
    let usage = data.get("usage")?;
    let field = |name: &str| -> u32 {
        u32::try_from(usage[name].as_u64().unwrap_or(0)).unwrap_or(u32::MAX)
    };
    Some(TokenUsage {
        prompt_tokens: field("prompt_tokens"),
        completion_tokens: field("completion_tokens"),
        total_tokens: field("total_tokens"),
    })
}

/// Ensure every assembled tool call has a non-empty `id`, synthesizing a stable
/// `call_{i}` from position when the wire delivered an empty one. Downstream (ACP)
/// correlates `ToolCall` / permission / `ToolCallUpdate` by this id, so an empty id
/// would silently break that correlation. Non-empty ids are left untouched.
///
/// `pub(crate)`: also called by `responses_api::parse_tool_calls` (monocle-cli#101),
/// which parses `tool_calls` off jarvice's `/api/responses` reply against this same
/// `ToolCall` type and must not silently diverge on id-normalization from this path.
pub(crate) fn ensure_tool_call_ids(tool_calls: &mut [ToolCall]) {
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
    let usage = parse_usage(data);
    Ok(ChatResponse {
        content,
        tool_calls,
        model,
        finish_reason,
        truncated: false,
        usage,
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
/// (monocle-cli#59): when `next_line` yields `Err`, the outcome depends on how
/// far the stream got:
/// - **Already complete** (`finish_reason.is_some()`): the model sent its final
///   chunk before the connection dropped (e.g. before `[DONE]`). Treat the error
///   as a benign end-of-stream — `break` and keep everything as-is (tool calls
///   preserved, NOT marked truncated). Dropping a valid tool call here would be a
///   bug.
/// - **Mid-generation** (`finish_reason` still `None`, but `content` or a tool
///   call was started): salvage the partial text — mark `truncated = true`, drop
///   any in-progress tool calls (a tool call cut off mid-stream is unreliable),
///   and break.
/// - **Nothing received**: propagate the error (a genuine connection failure with
///   nothing to salvage).
///
/// No-usable-content guard: if the stream ends having produced nothing usable —
/// no content, no tool calls, no `finish_reason`, and not truncated — it likely
/// carried an error-shaped `data:` payload (a 200 with `{"error":...}`) rather
/// than a real completion, so an error is returned rather than a silent empty
/// `Ok`. A legitimately-empty-but-complete reply carries `finish_reason` and so
/// passes.
fn assemble_sse_stream(
    mut next_line: impl FnMut() -> Result<Option<String>>,
    on_delta: &mut dyn FnMut(&str),
) -> Result<ChatResponse> {
    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    let mut model: Option<String> = None;
    let mut tool_acc: Vec<ToolCallAcc> = Vec::new();
    let mut truncated = false;
    // Only present when the request opted in via `stream_options.include_usage`
    // (see `MonocleProvider::build_body`) — OpenAI-compatible servers then send
    // one extra terminal chunk carrying `usage` alongside an empty `choices`.
    let mut usage: Option<TokenUsage> = None;

    loop {
        let raw = match next_line() {
            Ok(Some(raw)) => raw,
            Ok(None) => break,
            Err(e) => {
                if finish_reason.is_some() {
                    // The model already completed (final chunk arrived); the drop
                    // is just a missing `[DONE]`. Keep everything as-is.
                    break;
                }
                // Salvage a truncated-but-usable response if anything arrived
                // mid-generation; otherwise there is nothing to salvage.
                if !content.is_empty() || !tool_acc.is_empty() {
                    truncated = true;
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
        if usage.is_none() {
            usage = parse_usage(&v);
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

    // No-usable-content guard (parity with the non-streaming `parse_response_value`
    // "no choices" check): a stream that yielded no content, no tool calls, no
    // finish_reason, and wasn't truncated never saw a valid choice — most likely
    // an error-shaped 200 (`data:{"error":...}`). Surface it instead of a silent
    // empty answer.
    if content.is_empty() && tool_calls.is_empty() && finish_reason.is_none() && !truncated {
        return Err(AppError::new(
            "streaming response contained no completion (possible upstream error)",
        ));
    }

    Ok(ChatResponse {
        content,
        tool_calls,
        model,
        finish_reason,
        truncated,
        usage,
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

    /// Update this provider's token/router_url from a freshly resolved auth
    /// session, reusing the existing HTTP client (its connection pool and TLS
    /// session) rather than rebuilding one — `Client::new()` is real setup cost
    /// a token refresh doesn't need to pay for every turn.
    pub fn refresh(&mut self, session: AuthSession) {
        self.token = session.token;
        self.router_url = session.router_url;
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let mut messages = json!(req.messages);
        // Vision requests (monocle-cli file-attach plan): rewrite the last
        // `user` message's `content` from a plain string into the OpenAI
        // multi-part shape (`[{type:text}, {type:image_url}, ...]`). With no
        // images the messages serialize exactly as before — this branch is
        // the only place behavior can diverge.
        if !req.images.is_empty() {
            if let Some(arr) = messages.as_array_mut() {
                if let Some(last_user) = arr.iter_mut().rev().find(|m| m["role"] == "user") {
                    let mut parts: Vec<Value> = Vec::new();
                    let text = last_user["content"].as_str().unwrap_or("");
                    if !text.is_empty() {
                        parts.push(json!({"type": "text", "text": text}));
                    }
                    for img in &req.images {
                        parts.push(json!({"type": "image_url", "image_url": {"url": img.url}}));
                    }
                    last_user["content"] = json!(parts);
                }
            }
        }
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": stream,
        });
        if stream {
            // OpenAI-compatible streaming omits `usage` unless the request
            // opts in — without this, a streamed turn's `/diag` would never
            // have token counts (only the non-streaming path would).
            body["stream_options"] = json!({"include_usage": true});
        }
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
        assert!(resp.truncated);
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
        assert!(!resp.truncated);
        assert!(resp.tool_calls.is_empty());
        assert_eq!(deltas, vec!["Hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn empty_model_in_first_chunk_does_not_lock_out_later_real_model() {
        let (resp, _deltas) = run_stream(vec![
            Ok(Some(
                "data: {\"model\":\"\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}"
                    .to_string(),
            )),
            Ok(Some(
                "data: {\"model\":\"some-real-model\",\"choices\":[{\"delta\":{\"content\":\"!\"}}]}"
                    .to_string(),
            )),
            Ok(Some("data: [DONE]".to_string())),
            Ok(None),
        ]);
        let resp = resp.expect("normal completion should succeed");
        assert_eq!(resp.model.as_deref(), Some("some-real-model"));
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
        assert!(resp.truncated);
    }

    #[test]
    fn complete_tool_call_then_drop_is_not_truncated() {
        // The model sends a COMPLETE tool call plus `finish_reason:"tool_calls"`,
        // then the connection drops before `[DONE]`. The response is already
        // complete, so the drop must not truncate it or discard the tool call.
        let complete_tool_delta = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
            \"id\":\"call_abc\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]},\
            \"finish_reason\":\"tool_calls\"}]}";
        let (resp, _deltas) = run_stream(vec![
            Ok(Some(complete_tool_delta.to_string())),
            Err(AppError::new("error decoding response body")),
        ]);
        let resp = resp.expect("a completed response should survive a trailing drop");
        assert!(
            !resp.truncated,
            "a completed response must not be truncated"
        );
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            resp.tool_calls.len(),
            1,
            "a completed tool call must be preserved"
        );
        assert_eq!(resp.tool_calls[0].function.name, "read_file");
    }

    #[test]
    fn error_shaped_stream_with_no_choices_errors() {
        // A 200 that carries only an error-shaped payload (no valid choice /
        // finish_reason) then ends: nothing usable was produced, so surface an
        // error rather than a silent empty `Ok`.
        let (resp, _deltas) = run_stream(vec![
            Ok(Some(
                "data: {\"error\":{\"message\":\"upstream boom\"}}".to_string(),
            )),
            Ok(None),
        ]);
        assert!(
            resp.is_err(),
            "a stream with no usable completion should error"
        );
    }

    #[test]
    fn non_streaming_response_with_usage_is_parsed() {
        let data = serde_json::json!({
            "model": "gpt-4o",
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15},
        });
        let resp = parse_response_value(&data).unwrap();
        assert_eq!(
            resp.usage,
            Some(TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 3,
                total_tokens: 15,
            })
        );
    }

    #[test]
    fn non_streaming_response_without_usage_is_none() {
        let data = serde_json::json!({
            "model": "gpt-4o",
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
        });
        let resp = parse_response_value(&data).unwrap();
        assert_eq!(resp.usage, None);
    }

    #[test]
    fn oversized_usage_field_saturates_instead_of_wrapping() {
        // A malformed/oversized upstream value must saturate to `u32::MAX`,
        // not silently wrap to a small (wrong-looking) number via `as u32`.
        let data = serde_json::json!({
            "model": "gpt-4o",
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": u64::MAX,
                "completion_tokens": (u32::MAX as u64) + 1,
                "total_tokens": 15,
            },
        });
        let resp = parse_response_value(&data).unwrap();
        assert_eq!(
            resp.usage,
            Some(TokenUsage {
                prompt_tokens: u32::MAX,
                completion_tokens: u32::MAX,
                total_tokens: 15,
            })
        );
    }

    #[test]
    fn streaming_final_usage_chunk_is_captured() {
        // Mirrors the terminal chunk an OpenAI-compatible server sends when the
        // request opts in via `stream_options.include_usage`: empty `choices`
        // alongside a `usage` object, after the content-bearing deltas.
        let usage_chunk =
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}";
        let (resp, deltas) = run_stream(vec![
            content_line("Hi"),
            Ok(Some(usage_chunk.to_string())),
            Ok(Some("data: [DONE]".to_string())),
            Ok(None),
        ]);
        let resp = resp.expect("normal completion with a trailing usage chunk should succeed");
        assert_eq!(resp.content, "Hi");
        assert_eq!(
            resp.usage,
            Some(TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
            })
        );
        assert_eq!(deltas, vec!["Hi".to_string()]);
    }

    fn dummy_provider() -> MonocleProvider {
        MonocleProvider::new("token", "https://router.example")
    }

    #[test]
    fn build_body_with_no_images_is_unchanged() {
        // The critical regression oracle: an empty `images` vec must produce
        // exactly the same body as before attachments existed — a plain
        // string `content`, not an array.
        let provider = dummy_provider();
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::system("be terse"), Message::user("hello")],
            max_tokens: Some(100),
            ..Default::default()
        };
        let body = provider.build_body(&req, false);
        assert_eq!(body["messages"][0]["content"], json!("be terse"));
        assert_eq!(body["messages"][1]["content"], json!("hello"));
        assert!(body["messages"][1]["content"].is_string());
        assert_eq!(body["max_tokens"], json!(100));
    }

    #[test]
    fn build_body_streaming_opts_into_usage() {
        // Streaming requests must ask for the terminal `usage` chunk (OpenAI-
        // compatible servers omit it otherwise) so `/diag` can show token
        // counts on a streamed turn, same as a non-streaming one.
        let provider = dummy_provider();
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::user("hello")],
            ..Default::default()
        };
        let streaming_body = provider.build_body(&req, true);
        assert_eq!(
            streaming_body["stream_options"],
            json!({"include_usage": true})
        );

        let non_streaming_body = provider.build_body(&req, false);
        assert!(non_streaming_body.get("stream_options").is_none());
    }

    #[test]
    fn build_body_with_images_rewrites_last_user_message_as_parts() {
        let provider = dummy_provider();
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::system("be terse"), Message::user("count them")],
            images: vec![
                ImageAttachment {
                    url: "data:image/png;base64,AAAA".to_string(),
                },
                ImageAttachment {
                    url: "https://example.com/b.png".to_string(),
                },
            ],
            ..Default::default()
        };
        let body = provider.build_body(&req, false);

        // System message untouched.
        assert_eq!(body["messages"][0]["content"], json!("be terse"));

        // Last user message becomes a parts array: text part then image parts,
        // in the order the images were given.
        let parts = body["messages"][1]["content"]
            .as_array()
            .expect("user content should be rewritten into an array");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], json!({"type": "text", "text": "count them"}));
        assert_eq!(
            parts[1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}})
        );
        assert_eq!(
            parts[2],
            json!({"type": "image_url", "image_url": {"url": "https://example.com/b.png"}})
        );
    }

    #[test]
    fn build_body_with_images_and_empty_text_omits_text_part() {
        let provider = dummy_provider();
        let req = ChatRequest {
            model: "gpt-4o".to_string(),
            messages: vec![Message::user("")],
            images: vec![ImageAttachment {
                url: "https://example.com/a.png".to_string(),
            }],
            ..Default::default()
        };
        let body = provider.build_body(&req, false);
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], json!("image_url"));
    }
}
