//! Limited named-session adapter: core owns sequencing; native host owns I/O.
//! Only nonstream inference and existing read_file are supported. Native file
//! paths are not confined to workdir. JSONL acknowledgement is not crash durability.
use super::providers::{ChatRequest, ChatResponse, LlmProvider, Message, ToolDef};
use super::runner::Cancel;
use super::session::SessionStore;
use super::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::error::{AppError, Result};
use agent_core_rust_sequence as core;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

const PAYLOAD: usize = 64 * 1024;
const TRANSCRIPT: usize = 1024 * 1024;

pub(crate) struct ReadEvent {
    pub id: String,
    pub arguments: Value,
    pub outcome: ToolOutcome,
}
pub(crate) struct SelectedTurn {
    pub response: ChatResponse,
    pub steps: Vec<ChatResponse>,
    pub generated: Vec<Message>,
    pub events: Vec<ReadEvent>,
}
#[derive(Default)]
struct NativeState {
    responses: Vec<ChatResponse>,
    generated: Vec<Message>,
    events: Vec<ReadEvent>,
    response_bytes: usize,
}
fn invalid(message: impl Into<String>) -> core::AgentError {
    core::AgentError {
        phase: core::AgentPhase::Validation,
        message: message.into(),
    }
}
fn bounded(value: &Value, limit: usize) -> std::result::Result<(), core::AgentError> {
    if serde_json::to_vec(value)
        .map_err(|error| invalid(error.to_string()))?
        .len()
        > limit
    {
        return Err(invalid("selected core native payload exceeds byte bound"));
    }
    Ok(())
}

pub(crate) fn validate_history(messages: &[Message], max_steps: usize) -> Result<()> {
    if max_steps == 0
        || messages.first().is_none_or(|message| {
            message.role != "system"
                || message
                    .content
                    .as_deref()
                    .is_none_or(|text| text.trim().is_empty())
        })
    {
        return Err(AppError::new(
            "selected core requires system history and positive max-steps",
        ));
    }
    let mut pending = HashSet::new();
    for message in messages {
        match message.role.as_str() {
            "assistant" if pending.is_empty() => {
                for call in &message.tool_calls {
                    if call.id.trim().is_empty() || !pending.insert(call.id.clone()) {
                        return Err(AppError::new(
                            "selected core unsupported historical tool correlation",
                        ));
                    }
                }
            }
            "tool"
                if message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|id| pending.remove(id)) => {}
            "system" | "user" if pending.is_empty() => {}
            _ => return Err(AppError::new("selected core unsupported native history")),
        }
    }
    if !pending.is_empty() {
        return Err(AppError::new("selected core incomplete tool history"));
    }
    Ok(())
}

struct NativeManager;
impl core::ToolWorkflowManager for NativeManager {
    fn prepare_workflow(
        &mut self,
        input: core::TurnInput,
        prompt: &str,
    ) -> std::result::Result<core::WorkflowRequest, core::AgentError> {
        Ok(core::WorkflowRequest {
            turn_id: input.turn_id,
            system_prompt: prompt.into(),
            input: input.input,
            tools: vec![],
            transcript: vec![],
        })
    }
    fn apply_tool_results(
        &mut self,
        request: &core::WorkflowRequest,
        step: &core::ModelStep,
        results: &[core::ToolResult],
    ) -> std::result::Result<core::WorkflowRequest, core::AgentError> {
        let mut next = request.clone();
        next.transcript.push(core::WorkflowExchange {
            step: step.clone(),
            results: results.to_vec(),
        });
        Ok(next)
    }
}
struct ReadDefinition(core::ToolDefinition);
impl core::ToolRegistry for ReadDefinition {
    fn definitions(&self) -> std::result::Result<Vec<core::ToolDefinition>, core::AgentError> {
        Ok(vec![self.0.clone()])
    }
}
struct CancelView<'a>(&'a Cancel);
impl core::Cancellation for CancelView<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

// Translate trusted core correlation with original native assistant/raw args.
fn native_messages(
    base: &[Message],
    responses: &[ChatResponse],
    request: &core::WorkflowRequest,
) -> std::result::Result<Vec<Message>, core::AgentError> {
    if responses.len() != request.transcript.len() {
        return Err(invalid("native step history mismatch"));
    }
    let mut messages = base.to_vec();
    for (response, exchange) in responses.iter().zip(&request.transcript) {
        let core::ModelStep::ToolRequests(calls) = &exchange.step else {
            return Err(invalid("native continuation after terminal step"));
        };
        if calls.len() != response.tool_calls.len() || calls.len() != exchange.results.len() {
            return Err(invalid("native tool history count mismatch"));
        }
        messages.push(Message::assistant_with_tool_calls(
            response.content.clone(),
            response.tool_calls.clone(),
        ));
        for ((call, native), result) in calls
            .iter()
            .zip(&response.tool_calls)
            .zip(&exchange.results)
        {
            let args: Value = serde_json::from_str(&native.function.arguments)
                .map_err(|_| invalid("native historical arguments are not JSON"))?;
            if call.key.turn_id != request.turn_id
                || call.key.call_id != native.id
                || call.name != native.function.name
                || call.arguments != args
                || result.key != call.key
            {
                return Err(invalid("native tool history correlation mismatch"));
            }
            let core::ToolStatus::Completed(value) = &result.status else {
                return Err(invalid("selected core cannot continue a failed read"));
            };
            let text = value["llm"]
                .as_str()
                .ok_or_else(|| invalid("missing native read result"))?;
            messages.push(Message::tool(native.id.clone(), text));
        }
    }
    Ok(messages)
}

struct ReadExecutor<'a> {
    tools: &'a ToolRegistry,
    context: &'a ToolContext,
    state: Rc<RefCell<NativeState>>,
    original: Rc<RefCell<Option<AppError>>>,
}
impl core::ToolExecutor for ReadExecutor<'_> {
    fn execute(
        &mut self,
        call: &core::ToolCall,
        cancel: &dyn core::Cancellation,
    ) -> core::ToolResult {
        let denied = |message: &str| core::ToolResult {
            key: call.key.clone(),
            status: core::ToolStatus::Denied(message.into()),
        };
        if cancel.is_cancelled() {
            return denied("cancelled before read_file");
        }
        if call.name != "read_file"
            || !call.arguments.is_object()
            || call.arguments["path"]
                .as_str()
                .is_none_or(|path| path.is_empty())
            || self
                .tools
                .get("read_file")
                .is_none_or(|tool| tool.is_side_effecting())
        {
            return denied("selected core admits only native read_file with a path string");
        }
        // Native read-only tools do not invoke Approver. Mutation tools are
        // denied here, not delegated through an unattended approval policy.
        let outcome = self.tools.run(self.context, "read_file", &call.arguments);
        let value = json!({"llm":outcome.llm, "ui":outcome.ui});
        let status = if let Err(error) = bounded(&value, PAYLOAD) {
            core::ToolStatus::FailedNoEffect(error.message)
        } else if outcome.is_error {
            *self.original.borrow_mut() = Some(AppError::new(outcome.llm.clone()));
            core::ToolStatus::FailedNoEffect(outcome.llm.clone())
        } else {
            core::ToolStatus::Completed(value)
        };
        self.state.borrow_mut().events.push(ReadEvent {
            id: call.key.call_id.clone(),
            arguments: call.arguments.clone(),
            outcome,
        });
        core::ToolResult {
            key: call.key.clone(),
            status,
        }
    }
}
struct NativeStorage<'a> {
    session: &'a SessionStore,
    pending: &'a [Message],
    state: Rc<RefCell<NativeState>>,
    original: Rc<RefCell<Option<AppError>>>,
}
impl core::AgentStorage for NativeStorage<'_> {
    fn append(
        &mut self,
        record: &core::TurnRecord,
    ) -> std::result::Result<core::StorageReceipt, core::AgentError> {
        let state = self.state.borrow();
        let final_response = state
            .responses
            .last()
            .ok_or_else(|| invalid("missing native terminal response"))?;
        if !final_response.tool_calls.is_empty()
            || record.response["final"] != json!(final_response.content)
        {
            return Err(invalid("native terminal record mismatch"));
        }
        let mut messages = self.pending.to_vec();
        messages.extend(state.generated.iter().cloned());
        if let Err(error) = self.session.append(&messages) {
            *self.original.borrow_mut() = Some(error);
            return Err(core::AgentError {
                phase: core::AgentPhase::Storage,
                message: "native session append failed; reopen or reconcile before retry".into(),
            });
        }
        Ok(core::StorageReceipt {
            reference: self.session.path().display().to_string(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    provider: &dyn LlmProvider,
    base: ChatRequest,
    session: &SessionStore,
    pending: &[Message],
    tools: &ToolRegistry,
    context: &ToolContext,
    cancel: &Cancel,
    max_steps: usize,
    poisoned: &mut bool,
) -> Result<SelectedTurn> {
    validate_history(&base.messages, max_steps)?;
    if !base.images.is_empty() || !base.files.is_empty() {
        return Err(AppError::new("selected core does not support attachments"));
    }
    let definition = tools
        .get("read_file")
        .filter(|tool| !tool.is_side_effecting())
        .ok_or_else(|| AppError::new("native read_file capability unavailable"))?
        .def();
    let native_input =
        json!({"model":base.model,"messages":base.messages,"max_tokens":base.max_tokens});
    let turn_id = format!("{}:{}", session.path().display(), base.messages.len());
    let state = Rc::new(RefCell::new(NativeState::default()));
    let original = Rc::new(RefCell::new(None));
    let possible_effect = Rc::new(Cell::new(false));
    let shared = state.clone();
    let errors = original.clone();
    let effect = possible_effect.clone();
    let expected_input = native_input.clone();
    let inference = core::CallbackToolWorkflowProvider::new(
        move |request: core::WorkflowRequest, signal: &dyn core::Cancellation| {
            if signal.is_cancelled() {
                return Err(core::AgentError {
                    phase: core::AgentPhase::Cancelled,
                    message: "cancelled".into(),
                });
            }
            if request.input != expected_input {
                return Err(invalid("native request input changed"));
            }
            let messages = native_messages(&base.messages, &shared.borrow().responses, &request)?;
            let definitions = request
                .tools
                .iter()
                .map(|tool| {
                    ToolDef::function(
                        tool.name.clone(),
                        tool.description.clone(),
                        tool.input_schema.clone(),
                    )
                })
                .collect::<Vec<_>>();
            bounded(
                &json!({"model":base.model,"messages":messages,"max_tokens":base.max_tokens,"tools":definitions}),
                PAYLOAD,
            )?;
            let native = ChatRequest {
                messages: messages.clone(),
                tools: definitions,
                ..base.clone()
            };
            effect.set(true);
            let response = match provider.chat(&native) {
                Ok(response) => response,
                Err(error) => {
                    *errors.borrow_mut() = Some(error);
                    return Err(core::AgentError {
                        phase: core::AgentPhase::Provider,
                        message: "native provider failed".into(),
                    });
                }
            };
            if signal.is_cancelled() {
                return Err(core::AgentError {
                    phase: core::AgentPhase::Cancelled,
                    message: "cancelled after native provider".into(),
                });
            }
            if response.truncated {
                return Err(invalid(
                    "selected nonstream route cannot accept truncated response",
                ));
            }
            let metadata = json!({"content":response.content,"calls":response.tool_calls,"model":response.model,
            "finish_reason":response.finish_reason,"truncated":response.truncated,
            "usage":response.usage.as_ref().map(|usage| json!({"prompt_tokens":usage.prompt_tokens,"completion_tokens":usage.completion_tokens,"total_tokens":usage.total_tokens}))});
            bounded(&metadata, PAYLOAD)?;
            let mut state = shared.borrow_mut();
            state.response_bytes = state.response_bytes.saturating_add(
                serde_json::to_vec(&metadata)
                    .map_err(|error| invalid(error.to_string()))?
                    .len(),
            );
            if state.response_bytes > TRANSCRIPT {
                return Err(invalid("native response history exceeds byte bound"));
            }
            let step = if response.tool_calls.is_empty() {
                let mut generated = messages[base.messages.len()..].to_vec();
                generated.push(Message::assistant(response.content.clone()));
                bounded(&json!(generated), TRANSCRIPT)?;
                state.generated = generated;
                core::ModelStep::Terminal(json!(response.content))
            } else {
                let mut calls = Vec::new();
                for call in &response.tool_calls {
                    if call.kind != "function" {
                        return Err(invalid("selected core requires native function tool calls"));
                    }
                    let arguments: Value = serde_json::from_str(&call.function.arguments)
                        .map_err(|_| invalid("selected core requires valid JSON tool arguments"))?;
                    calls.push(core::ToolCall {
                        key: core::CallKey {
                            turn_id: request.turn_id.clone(),
                            call_id: call.id.clone(),
                        },
                        name: call.function.name.clone(),
                        arguments,
                    });
                }
                core::ModelStep::ToolRequests(calls)
            };
            state.responses.push(response);
            Ok(step)
        },
    );
    let prompt = native_input["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut actor = core::ToolWorkflowAgent::new(
        inference,
        NativeManager,
        ReadExecutor {
            tools,
            context,
            state: state.clone(),
            original: original.clone(),
        },
        NativeStorage {
            session,
            pending,
            state: state.clone(),
            original: original.clone(),
        },
        prompt,
        core::WorkflowLimits {
            max_model_steps: max_steps.min(128),
            max_tool_calls: 128,
            max_payload_bytes: PAYLOAD,
            max_transcript_bytes: TRANSCRIPT,
        },
    )
    .map_err(|error| AppError::new(error.message))?;
    actor.set_tool_registry(Box::new(ReadDefinition(core::ToolDefinition {
        name: definition.function.name,
        description: definition.function.description,
        input_schema: definition.function.parameters,
    })));
    let result = actor.run_turn(
        core::TurnInput {
            turn_id,
            input: native_input,
        },
        &CancelView(cancel),
    );
    if let Err(failure) = result {
        *poisoned = possible_effect.get();
        if let Some(error) = original.borrow_mut().take() {
            return Err(error);
        }
        return Err(AppError::new(format!(
            "selected core {:?}: {}",
            failure.error.phase, failure.error.message
        )));
    }
    let mut state = state.borrow_mut();
    let response = state
        .responses
        .last()
        .cloned()
        .ok_or_else(|| AppError::new("selected core committed without native response"))?;
    Ok(SelectedTurn {
        response,
        steps: std::mem::take(&mut state.responses),
        generated: std::mem::take(&mut state.generated),
        events: std::mem::take(&mut state.events),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::{FunctionCall, ToolCall as NativeToolCall};
    use crate::agent::tools::FsBackend;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct NativeProviderFixture {
        calls: Cell<usize>,
        response: ChatResponse,
        cancel_after_call: Option<Cancel>,
    }
    impl LlmProvider for NativeProviderFixture {
        fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(request.tools.len(), 1);
            assert_eq!(request.tools[0].function.name, "read_file");
            if let Some(cancel) = &self.cancel_after_call {
                cancel.cancel();
            }
            Ok(self.response.clone())
        }
    }
    struct CountReads(Arc<AtomicUsize>);
    impl FsBackend for CountReads {
        fn read(&self, _: &Path) -> std::result::Result<String, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("unexpected read".into())
        }
        fn write(&self, _: &Path, _: &str) -> std::result::Result<(), String> {
            panic!("selected route must never invoke filesystem write")
        }
    }
    fn native_request() -> ChatRequest {
        ChatRequest {
            model: "fixture-model".into(),
            messages: vec![Message::system("system"), Message::user("read")],
            ..Default::default()
        }
    }
    fn native_response() -> ChatResponse {
        ChatResponse {
            content: "done".into(),
            tool_calls: vec![],
            model: Some("fixture-model".into()),
            finish_reason: Some("stop".into()),
            truncated: false,
            usage: None,
        }
    }

    #[test]
    fn cancellation_after_native_provider_prevents_read_and_append_and_poison_session() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionStore::new(temp.path().join("session.jsonl"));
        let cancel = Cancel::new();
        let mut response = native_response();
        response.tool_calls = vec![NativeToolCall {
            id: "read-1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: "{\"path\":\"answer.txt\"}".into(),
            },
        }];
        let provider = NativeProviderFixture {
            calls: Cell::new(0),
            response,
            cancel_after_call: Some(cancel.clone()),
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let context = ToolContext::new(temp.path())
            .with_cancel(cancel.clone())
            .with_fs(Arc::new(CountReads(reads.clone())));
        let tools = ToolRegistry::with_defaults();
        let request = native_request();
        let pending = request.messages.clone();
        let mut poisoned = false;
        let result = run(
            &provider,
            request,
            &session,
            &pending,
            &tools,
            &context,
            &cancel,
            4,
            &mut poisoned,
        );
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("cancelled after native provider"));
        assert_eq!(provider.calls.get(), 1, "no cancellation retry or fallback");
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(!session.path().exists());
        assert!(poisoned, "provider may already have performed an effect");
    }

    #[test]
    fn actual_session_append_failure_preserves_native_error_and_poison_without_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        std::fs::create_dir(&path).unwrap();
        let expected_error = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_err()
            .to_string();
        let session = SessionStore::new(&path);
        let provider = NativeProviderFixture {
            calls: Cell::new(0),
            response: native_response(),
            cancel_after_call: None,
        };
        let cancel = Cancel::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let context = ToolContext::new(temp.path())
            .with_cancel(cancel.clone())
            .with_fs(Arc::new(CountReads(reads.clone())));
        let tools = ToolRegistry::with_defaults();
        let request = native_request();
        let pending = request.messages.clone();
        let mut poisoned = false;
        let result = run(
            &provider,
            request,
            &session,
            &pending,
            &tools,
            &context,
            &cancel,
            4,
            &mut poisoned,
        );
        assert!(
            result.is_err(),
            "no successful turn after actual native append error"
        );
        assert_eq!(result.err().unwrap().to_string(), expected_error);
        assert_eq!(provider.calls.get(), 1, "append failure must not re-infer");
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(poisoned);
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    }
}
