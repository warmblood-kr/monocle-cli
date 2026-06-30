//! The agent-core loop: prompt → LLM (with tools) → execute tool calls → feed
//! results back → repeat until the model answers with no tool calls.
//!
//! Side-effecting tools are gated by an `Approver` (SDD G4 — the permission seam;
//! a real scoped-approval / sandboxing policy plugs in here later). An `Observer`
//! surfaces the loop's steps (the CLI prints them; tests stay silent).

use serde_json::{json, Value};

use crate::agent::providers::{ChatRequest, LlmProvider, Message};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::error::{AppError, Result};

/// Decides whether a *side-effecting* tool call may run. Read-only tools are not
/// gated. (Default policies: [`AllowAll`].)
pub trait Approver {
    fn approve(&mut self, tool_name: &str, args: &Value) -> bool;
}

/// Approve everything — for tests and explicit local "yolo" runs.
pub struct AllowAll;
impl Approver for AllowAll {
    fn approve(&mut self, _tool_name: &str, _args: &Value) -> bool {
        true
    }
}

/// Observes loop steps (for printing / logging). Default = no-op ([`Silent`]).
pub trait Observer {
    fn on_text(&mut self, _text: &str) {}
    fn on_tool_call(&mut self, _name: &str, _args: &Value) {}
    fn on_tool_result(&mut self, _name: &str, _outcome: &ToolOutcome) {}
}

pub struct Silent;
impl Observer for Silent {}

pub struct AgentConfig {
    pub model: String,
    pub max_steps: usize,
    pub max_tokens: Option<i64>,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_steps: 20,
            max_tokens: None,
        }
    }
}

pub struct Agent<'a, P: LlmProvider> {
    provider: &'a P,
    tools: &'a ToolRegistry,
    ctx: ToolContext,
    config: AgentConfig,
}

impl<'a, P: LlmProvider> Agent<'a, P> {
    pub fn new(
        provider: &'a P,
        tools: &'a ToolRegistry,
        ctx: ToolContext,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            ctx,
            config,
        }
    }

    /// Run the loop to completion, returning the model's final text answer.
    /// `messages` is the seed conversation (e.g. a system prompt + the user task).
    pub fn run(
        &self,
        mut messages: Vec<Message>,
        approver: &mut dyn Approver,
        observer: &mut dyn Observer,
    ) -> Result<String> {
        let defs = self.tools.defs();

        for _step in 0..self.config.max_steps {
            let resp = self.provider.chat(&ChatRequest {
                model: self.config.model.clone(),
                messages: messages.clone(),
                max_tokens: self.config.max_tokens,
                tools: defs.clone(),
            })?;

            // No tool calls → this is the final answer (returned, not observed, so
            // the caller can route it to stdout exactly once).
            if resp.tool_calls.is_empty() {
                return Ok(resp.content);
            }

            if !resp.content.is_empty() {
                observer.on_text(&resp.content);
            }
            // Replay the assistant's tool-call turn before the matching results.
            messages.push(Message::assistant_with_tool_calls(
                resp.content.clone(),
                resp.tool_calls.clone(),
            ));

            for call in &resp.tool_calls {
                let args: Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
                observer.on_tool_call(&call.function.name, &args);

                let side_effecting = self
                    .tools
                    .get(&call.function.name)
                    .map(|t| t.is_side_effecting())
                    .unwrap_or(true);

                let outcome = if side_effecting && !approver.approve(&call.function.name, &args) {
                    ToolOutcome::error(format!("tool call `{}` denied", call.function.name))
                } else {
                    self.tools.run(&self.ctx, &call.function.name, &args)
                };

                observer.on_tool_result(&call.function.name, &outcome);
                messages.push(Message::tool(call.id.clone(), outcome.content.clone()));
            }
        }

        Err(AppError::new(format!(
            "agent did not finish within {} steps",
            self.config.max_steps
        )))
    }
}
