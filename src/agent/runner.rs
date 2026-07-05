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

/// Why the loop stopped — the *real* stop reason, reported to callers (the ACP
/// surface maps it to a `StopReason`). Distinguishing these lets a cancel that
/// lands during the final step be reported as cancelled rather than a completed
/// answer, and a step-budget exhaustion as its own condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStop {
    /// The model returned a final answer with no tool calls (normal completion).
    EndTurn,
    /// Cancellation was observed at a step boundary; the loop stopped gracefully.
    Cancelled,
    /// The step budget (`max_steps`) was exhausted before the model finished.
    MaxSteps,
}

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

    /// Convenience constructor for the default config with a custom step budget —
    /// the shape both `monocle agent` and the ACP surface build. Keeps the
    /// `AgentConfig::new` + `max_steps` two-step in one place.
    pub fn with_max_steps(
        provider: &'a P,
        tools: &'a ToolRegistry,
        ctx: ToolContext,
        model: impl Into<String>,
        max_steps: usize,
    ) -> Self {
        let mut config = AgentConfig::new(model);
        config.max_steps = max_steps;
        Self::new(provider, tools, ctx, config)
    }

    /// Run the loop to completion, appending this turn's assistant/tool messages
    /// to `conversation` (so callers can continue a multi-turn session) and
    /// reporting *why* it stopped (see [`RunStop`]). The model's streamed text is
    /// surfaced through the observer and appended to the conversation, not returned.
    pub fn run(
        &self,
        conversation: &mut Vec<Message>,
        approver: &mut dyn Approver,
        observer: &mut dyn Observer,
        cancel: &Cancel,
    ) -> Result<RunStop> {
        let defs = self.tools.defs();

        for _step in 0..self.config.max_steps {
            // Cancellation is checked at each step boundary (graceful stop).
            if cancel.is_cancelled() {
                observer.on_notice("[agent cancelled]");
                return Ok(RunStop::Cancelled);
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

            // A mid-stream drop was salvaged into a truncated response
            // (monocle-cli#59): tool_calls are already dropped, so this falls
            // into the EndTurn path below — just surface a notice. The partial
            // `resp.content` is appended there like any final answer, so a
            // `--session` resume keeps it (no separate append needed).
            if resp.truncated {
                observer.on_notice("[generation was cut short — showing partial output]");
            }

            // No tool calls → this is the final answer (already streamed to the
            // observer; appended to the conversation for multi-turn callers).
            if resp.tool_calls.is_empty() {
                conversation.push(Message::assistant(resp.content.clone()));
                return Ok(RunStop::EndTurn);
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
                        // Emit the ToolCall first (with the raw args as a JSON string)
                        // so an ACP client has a real ToolCall to correlate the
                        // failure to — otherwise the ToolCallUpdate is an orphan and
                        // the failure is invisible.
                        let args_repr = Value::String(call.function.arguments.clone());
                        observer.on_tool_call(&call.id, &call.function.name, &args_repr);
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

                // A cancel that landed during this tool (e.g. a killed shell
                // command) should stop us promptly: skip the remaining tool calls
                // in this step so the top-of-step check reports `Cancelled` rather
                // than running more side effects first.
                if cancel.is_cancelled() {
                    break;
                }
            }
        }

        // Step budget exhausted: stop gracefully (not a hard error) with a clear
        // notice; any streamed text is already in the observer / conversation.
        observer.on_notice(&format!(
            "[agent stopped after {} steps without finishing]",
            self.config.max_steps
        ));
        Ok(RunStop::MaxSteps)
    }
}
