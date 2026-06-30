//! LLM provider abstraction — the **model-choice-freedom (G1)** seam.
//!
//! The agent loop depends on the `LlmProvider` trait, never on a specific vendor.
//! Swapping the model id (or the provider impl) changes the backing model without
//! touching the loop. `MonocleProvider` routes through monocle's chat-proxy
//! (OpenAI-compatible `/v1/chat/completions`), so monocle-model-router /
//! monocle-auto can select any model and the agent stays vendor-agnostic.
//!
//! Spike scope (monocle-cli#44, step 1): one non-streaming turn. No tool calls,
//! no streaming, no loop yet.

use serde::Serialize;
use serde_json::{json, Value};

use crate::auth::AuthSession;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::origin_header;

/// One chat message. Serializes to the OpenAI-compatible `{role, content}` shape.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

/// A vendor-agnostic chat request. `model` is just an id the router understands —
/// the loop does not know or care which vendor it maps to.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: Option<i64>,
}

/// The assistant's reply for one turn.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
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
        let choice = &data["choices"][0];
        let content = choice["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let model = data["model"].as_str().map(String::from);
        let finish_reason = choice["finish_reason"].as_str().map(String::from);

        Ok(ChatResponse {
            content,
            model,
            finish_reason,
        })
    }
}
