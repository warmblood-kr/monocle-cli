//! Experimental adapter from the native host to the frozen model-only Agent.
use crate::agent::providers::{ChatRequest, ChatResponse, LlmProvider, Message};
use crate::agent::runner::Cancel;
use crate::agent::session::SessionStore;
use crate::error::{AppError, Result};
use agent_core_rust_sequence as core;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

const UNSUPPORTED_REQUEST: &str = "core model-only: unsupported request";
const UNSUPPORTED_RESPONSE: &str = "core model-only: unsupported response";
const CANCELLED: &str = "core model-only: cancelled";
const SESSION_REQUIRED: &str = "core model-only: a named session is required";

fn unsupported_request(request: &ChatRequest) -> bool {
    request.model.trim().is_empty()
        || request.messages.is_empty()
        || !request.tools.is_empty()
        || !request.images.is_empty()
        || !request.files.is_empty()
        || request.messages.iter().any(|message| {
            message.role == "tool"
                || message.tool_call_id.is_some()
                || !message.tool_calls.is_empty()
        })
        || request
            .messages
            .iter()
            .filter(|m| m.role == "system")
            .any(|m| {
                m.content
                    .as_deref()
                    .is_none_or(|content| content.trim().is_empty())
            })
}

fn core_error(error: core::AgentError) -> AppError {
    AppError::new(match error.phase {
        core::AgentPhase::Cancelled => CANCELLED,
        core::AgentPhase::Validation => UNSUPPORTED_REQUEST,
        core::AgentPhase::Storage => "core model-only: session append failed",
        _ => "core model-only: agent failed",
    })
}

struct CancelAdapter<'a>(&'a Cancel);
impl core::Cancellation for CancelAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct NativeProvider<'a, P: LlmProvider + ?Sized> {
    provider: &'a P,
    original_error: Rc<RefCell<Option<AppError>>>,
}

impl<P: LlmProvider + ?Sized> core::LlmProvider for NativeProvider<'_, P> {
    fn infer(
        &mut self,
        request: core::LlmRequest,
        cancel: &dyn core::Cancellation,
    ) -> std::result::Result<core::LlmResponse, core::AgentError> {
        if cancel.is_cancelled() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Cancelled,
                message: "cancelled".into(),
            });
        }
        let native = ChatRequest {
            model: request.input["model"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            messages: serde_json::from_value(request.input["messages"].clone()).map_err(|_| {
                core::AgentError {
                    phase: core::AgentPhase::Validation,
                    message: "invalid native messages".into(),
                }
            })?,
            max_tokens: serde_json::from_value(request.input["max_tokens"].clone()).ok(),
            ..Default::default()
        };
        let response = match self.provider.chat(&native) {
            Ok(response) => response,
            Err(error) => {
                *self.original_error.borrow_mut() = Some(error);
                return Err(core::AgentError {
                    phase: core::AgentPhase::Provider,
                    message: "provider failed".into(),
                });
            }
        };
        if cancel.is_cancelled() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Cancelled,
                message: "cancelled".into(),
            });
        }
        if response.truncated || !response.tool_calls.is_empty() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Provider,
                message: UNSUPPORTED_RESPONSE.into(),
            });
        }
        let payload = json!({
            "content": response.content,
            "tool_calls": response.tool_calls,
            "model": response.model,
            "finish_reason": response.finish_reason,
            "truncated": response.truncated,
            "usage": response.usage.map(|usage| json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
            })),
        });
        Ok(core::LlmResponse { payload })
    }
}

struct SessionAppend<'a> {
    session: &'a SessionStore,
    pending: Vec<Message>,
    original_error: Rc<RefCell<Option<AppError>>>,
    poisoned: &'a mut bool,
}

impl core::AgentStorage for SessionAppend<'_> {
    fn append(
        &mut self,
        record: &core::TurnRecord,
    ) -> std::result::Result<core::StorageReceipt, core::AgentError> {
        let content = record
            .response
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| core::AgentError {
                phase: core::AgentPhase::Manager,
                message: "missing response content".into(),
            })?;
        let mut messages = self.pending.clone();
        messages.push(Message::assistant(content));
        if let Err(error) = self.session.append(&messages) {
            // SessionStore appends JSONL records without rollback guarantees.
            // An error may follow a partial write, so poison this Repl and do
            // not advance its in-memory cursor or attempt an automatic retry.
            *self.poisoned = true;
            *self.original_error.borrow_mut() = Some(error);
            return Err(core::AgentError {
                phase: core::AgentPhase::Storage,
                message: "append failed".into(),
            });
        }
        Ok(core::StorageReceipt {
            reference: self.session.path().display().to_string(),
        })
    }
}

fn response_from_value(value: Value) -> Result<ChatResponse> {
    let usage = value
        .get("usage")
        .filter(|v| !v.is_null())
        .map(|usage| {
            Ok::<_, AppError>(crate::agent::providers::TokenUsage {
                prompt_tokens: serde_json::from_value(usage["prompt_tokens"].clone())?,
                completion_tokens: serde_json::from_value(usage["completion_tokens"].clone())?,
                total_tokens: serde_json::from_value(usage["total_tokens"].clone())?,
            })
        })
        .transpose()?;
    Ok(ChatResponse {
        content: value["content"].as_str().unwrap_or_default().to_owned(),
        tool_calls: serde_json::from_value(value["tool_calls"].clone())?,
        model: serde_json::from_value(value["model"].clone())?,
        finish_reason: serde_json::from_value(value["finish_reason"].clone())?,
        truncated: value["truncated"].as_bool().unwrap_or(false),
        usage,
    })
}

/// Execute one native request through Agent; persistence occurs inside core's
/// storage stage, and host state advances only after that append succeeds.
pub(crate) fn run_model_only<P: LlmProvider + ?Sized>(
    provider: &P,
    request: &ChatRequest,
    conversation: &mut Vec<Message>,
    persisted: &mut usize,
    session: Option<&SessionStore>,
    poisoned: &mut bool,
    cancel: &Cancel,
) -> Result<ChatResponse> {
    if unsupported_request(request) {
        return Err(AppError::new(UNSUPPORTED_REQUEST));
    }
    let session = session.ok_or_else(|| AppError::new(SESSION_REQUIRED))?;
    if *poisoned {
        return Err(AppError::new(
            "core model-only: session requires reopen or reconciliation",
        ));
    }
    if *persisted > conversation.len() {
        return Err(AppError::new(UNSUPPORTED_REQUEST));
    }

    let original_error = Rc::new(RefCell::new(None));
    let native_provider = NativeProvider {
        provider,
        original_error: original_error.clone(),
    };
    let pending = conversation[*persisted..].to_vec();
    let storage = SessionAppend {
        session,
        pending,
        original_error: original_error.clone(),
        poisoned,
    };
    let input = json!({
        "model": request.model,
        "messages": request.messages,
        "max_tokens": request.max_tokens,
    });
    let mut agent = core::Agent::new(
        native_provider,
        core::DefaultChatTurnManager,
        storage,
        "model-only host turn".into(),
    )
    .map_err(core_error)?;
    let outcome = agent.run_turn(
        core::TurnInput {
            turn_id: "native-cli-turn".into(),
            input,
        },
        &CancelAdapter(cancel),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(original) = original_error.borrow_mut().take() {
                return Err(original);
            }
            if error.phase == core::AgentPhase::Provider && error.message == UNSUPPORTED_RESPONSE {
                return Err(AppError::new(UNSUPPORTED_RESPONSE));
            }
            return Err(core_error(error));
        }
    };
    let response = response_from_value(outcome.response)?;
    conversation.push(Message::assistant(response.content.clone()));
    *persisted = conversation.len();
    Ok(response)
}
