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
use crate::origin::origin_header;

/// One chat message (OpenAI-compatible). `content` is optional because an
/// assistant message that only makes tool calls carries no text.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// The seam the agent loop is built on. Any backend (monocle-routed today, a
/// direct provider tomorrow) implements this; the loop never names a vendor.
pub trait LlmProvider {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse>;
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
}

impl LlmProvider for MonocleProvider {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let bearer = format!("Bearer {}", self.token);
        let mut body = json!({
            "model": req.model,
            "messages": req.messages,
            "stream": false,
        });
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools);
        }

        let resp = self.client.post_json(
            &format!("{}{}", self.router_url, endpoints::CHAT_COMPLETIONS),
            &[("Authorization", &bearer), origin_header()],
            &body,
        )?;

        if !resp.ok() {
            return Err(AppError::new(format!(
                "API error {}: {}",
                resp.status,
                resp.text()
            )));
        }

        let data: Value = resp.json()?;
        let message = &data["choices"][0]["message"];
        let content = message["content"].as_str().unwrap_or_default().to_string();
        let tool_calls: Vec<ToolCall> =
            serde_json::from_value(message["tool_calls"].clone()).unwrap_or_default();
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
}
