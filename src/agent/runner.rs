//! The agent-core loop: prompt → LLM (with tools) → execute tool calls → feed
//! results back → repeat until the model answers with no tool calls.
//!
//! Side-effecting tools are gated by an `Approver` (SDD G4 — the permission seam;
//! a real scoped-approval / sandboxing policy plugs in here later). An `Observer`
//! surfaces the loop's steps (the CLI prints them; tests stay silent).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::agent::providers::{ChatRequest, LlmProvider, Message};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::error::Result;

/// A cheap, clonable cancellation flag shared between the loop and whoever can
/// stop it (a Ctrl-C handler, an ACP `session/cancel`, a test). Checked at each
/// step boundary — the loop stops gracefully. Pure and trivially testable.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Decides whether a *side-effecting* tool call may run. Read-only tools are not
/// gated. (Default policies: [`AllowAll`].)
pub trait Approver {
    fn approve(&mut self, id: &str, tool_name: &str, args: &Value) -> bool;
}

/// Approve everything — for tests and explicit local "yolo" runs.
pub struct AllowAll;
impl Approver for AllowAll {
    fn approve(&mut self, _id: &str, _tool_name: &str, _args: &Value) -> bool {
        true
    }
}

/// Observes loop steps (for printing / logging). Default = no-op ([`Silent`]).
///
/// The `id` on the tool callbacks is the LLM tool-call id (`call.id`), which ties
/// a call's start (`on_tool_call`) to its completion (`on_tool_result`) — the ACP
/// surface uses it to correlate `ToolCall` / `ToolCallUpdate` lifecycle updates.
pub trait Observer {
    /// A chunk of assistant text as it streams in.
    fn on_text_delta(&mut self, _delta: &str) {}
    fn on_tool_call(&mut self, _id: &str, _name: &str, _args: &Value) {}
    fn on_tool_result(&mut self, _id: &str, _name: &str, _outcome: &ToolOutcome) {}
    /// A meta/control notice (stopped, cancelled) — NOT model answer text, so the
    /// CLI keeps it off the stdout answer channel.
    fn on_notice(&mut self, _msg: &str) {}
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

    /// Run the loop to completion, appending this turn's assistant/tool messages
    /// to `conversation` (so callers can continue a multi-turn session) and
    /// returning the model's final text answer.
    pub fn run(
        &self,
        conversation: &mut Vec<Message>,
        approver: &mut dyn Approver,
        observer: &mut dyn Observer,
        cancel: &Cancel,
    ) -> Result<String> {
        let defs = self.tools.defs();
        let mut last_text = String::new();

        for _step in 0..self.config.max_steps {
            // Cancellation is checked at each step boundary (graceful stop).
            if cancel.is_cancelled() {
                let notice = "[agent cancelled]".to_string();
                observer.on_notice(&notice);
                return Ok(if last_text.is_empty() {
                    notice
                } else {
                    format!("{last_text}\n\n{notice}")
                });
            }
            let req = ChatRequest {
                model: self.config.model.clone(),
                messages: conversation.clone(),
                max_tokens: self.config.max_tokens,
                tools: defs.clone(),
            };
            // Stream assistant text to the observer as it arrives.
            let resp = self
                .provider
                .chat_stream(&req, &mut |delta| observer.on_text_delta(delta))?;

            // No tool calls → this is the final answer (already streamed to the
            // observer; also returned for programmatic / multi-turn callers).
            if resp.tool_calls.is_empty() {
                conversation.push(Message::assistant(resp.content.clone()));
                return Ok(resp.content);
            }

            if !resp.content.is_empty() {
                last_text = resp.content.clone();
            }
            // Replay the assistant's tool-call turn before the matching results.
            conversation.push(Message::assistant_with_tool_calls(
                resp.content.clone(),
                resp.tool_calls.clone(),
            ));

            for call in &resp.tool_calls {
                // Surface malformed arguments back to the model (as a tool error)
                // instead of silently running the tool with `{}` — let it self-correct.
                let args: Value = match serde_json::from_str(&call.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        let outcome = ToolOutcome::error(format!(
                            "invalid arguments for `{}` (not valid JSON): {e}",
                            call.function.name
                        ));
                        observer.on_tool_result(&call.id, &call.function.name, &outcome);
                        conversation.push(Message::tool(call.id.clone(), outcome.llm.clone()));
                        continue;
                    }
                };
                observer.on_tool_call(&call.id, &call.function.name, &args);

                let side_effecting = self
                    .tools
                    .get(&call.function.name)
                    .map(|t| t.is_side_effecting())
                    .unwrap_or(true);

                let outcome =
                    if side_effecting && !approver.approve(&call.id, &call.function.name, &args) {
                        ToolOutcome::error(format!("tool call `{}` denied", call.function.name))
                    } else {
                        self.tools.run(&self.ctx, &call.function.name, &args)
                    };

                observer.on_tool_result(&call.id, &call.function.name, &outcome);
                conversation.push(Message::tool(call.id.clone(), outcome.llm.clone()));
            }
        }

        // Step budget exhausted: don't throw away work — return the best text so
        // far with a clear notice (graceful stop, not a hard error).
        let notice = format!(
            "[agent stopped after {} steps without finishing]",
            self.config.max_steps
        );
        observer.on_notice(&notice);
        Ok(if last_text.is_empty() {
            notice
        } else {
            format!("{last_text}\n\n{notice}")
        })
    }
}
