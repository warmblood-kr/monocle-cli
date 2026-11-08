//! Experimental model-only host adapter; native response metadata stays in the host.
use crate::agent::core_interface::{run_turn, HostPort};
use crate::agent::providers::{ChatRequest, ChatResponse, LlmProvider};
use crate::agent::runner::Cancel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::cell::RefCell;

const UNSUPPORTED_REQUEST: &str = "core model-only: unsupported request";
const UNSUPPORTED_RESPONSE: &str = "core model-only: unsupported response";
const CANCELLED: &str = "core model-only: cancelled";

fn unsupported_request(request: &ChatRequest) -> bool {
    !request.tools.is_empty()
        || !request.images.is_empty()
        || !request.files.is_empty()
        || request.messages.iter().any(|message| {
            message.role == "tool"
                || message.tool_call_id.is_some()
                || !message.tool_calls.is_empty()
        })
}

fn failure_message(reason: &'static str) -> &'static str {
    match reason {
        "cancelled" => CANCELLED,
        "unsupported request" => UNSUPPORTED_REQUEST,
        _ => UNSUPPORTED_RESPONSE,
    }
}

/// Host constructs request; adapter owns no auth, tools, observer or storage.
/// Must call the shared core with initial_response=None and limit=1, then
/// perform provider.chat only from its Model callback. Return native response
/// metadata intact after terminal core success; never replay through Native.
pub(crate) fn run_model_only<P: LlmProvider + ?Sized>(
    provider: &P,
    request: &ChatRequest,
    cancel: &Cancel,
) -> Result<ChatResponse> {
    if unsupported_request(request) {
        return Err(AppError::new(UNSUPPORTED_REQUEST));
    }

    let captured: RefCell<Option<Result<ChatResponse>>> = RefCell::new(None);
    let outcome = {
        let mut port = HostPort::new(
            |_payload: Value| -> std::result::Result<Value, &'static str> {
                if cancel.is_cancelled() {
                    return Err("cancelled");
                }
                match provider.chat(request) {
                    Ok(response) => {
                        if cancel.is_cancelled() {
                            return Err("cancelled");
                        }
                        if response.truncated || !response.tool_calls.is_empty() {
                            return Err("unsupported response");
                        }
                        *captured.borrow_mut() = Some(Ok(response));
                        Ok(json!({ "calls": [] }))
                    }
                    Err(error) => {
                        *captured.borrow_mut() = Some(Err(error));
                        Err("provider error")
                    }
                }
            },
            |_payload: Value| -> std::result::Result<Value, &'static str> {
                Err("unsupported tools")
            },
        );
        run_turn(json!({}), None, 1, &mut port)
    };

    match outcome {
        Ok(_) => match captured.borrow_mut().take() {
            Some(Ok(response)) if !cancel.is_cancelled() => Ok(response),
            Some(Ok(_)) => Err(AppError::new(CANCELLED)),
            _ => Err(AppError::new(UNSUPPORTED_RESPONSE)),
        },
        Err(failure) => {
            if let Some(Err(error)) = captured.borrow_mut().take() {
                return Err(error);
            }
            Err(AppError::new(failure_message(failure.reason)))
        }
    }
}

#[cfg(test)]
#[path = "../tests/core_model_only.rs"]
mod contract_tests;
