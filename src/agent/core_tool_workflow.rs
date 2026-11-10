//! Explicit opt-in adapter from Monocle's native provider/tools to the reviewed
//! bounded serial tool-workflow actor. The default native Agent remains separate.
use crate::agent::providers::{
    ChatRequest, ChatResponse, FunctionCall, LlmProvider, Message, ToolCall as NativeToolCall,
    ToolDef,
};
use crate::agent::runner::{Approver, Cancel, Observer};
use crate::agent::session::SessionStore;
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry as NativeToolRegistry};
use crate::error::{AppError, Result};
use agent_core_rust_sequence as core;
use serde_json::{json, Value};
use std::cell::Cell;
use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const MAX_MODEL_STEPS: usize = 128;
const MAX_TOOL_CALLS: usize = 128;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;

fn pending_path(session: &SessionStore) -> PathBuf {
    let mut value = session.path().as_os_str().to_owned();
    value.push(".core-tool-workflow-pending");
    PathBuf::from(value)
}

pub(crate) fn ensure_no_pending_dispatches(session: &SessionStore) -> Result<()> {
    let path = pending_path(session);
    match fs::symlink_metadata(&path) {
        Ok(_) => Err(AppError::new(format!(
            "agent: unresolved tool dispatch journal at {}; inspect effects and session JSONL, then reconcile manually before reopening this named session",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::new(format!(
            "agent: cannot verify dispatch journal {}: {error}",
            path.display()
        ))),
    }
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn open_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

struct DispatchJournal {
    dir: PathBuf,
    active: bool,
    turn_id: String,
    next_call: usize,
    current_call: usize,
}

impl DispatchJournal {
    fn new(session: &SessionStore, turn_id: String) -> Self {
        Self {
            dir: pending_path(session),
            active: false,
            turn_id,
            next_call: 0,
            current_call: 0,
        }
    }

    fn begin(&mut self, call: &core::ToolCall) -> std::io::Result<()> {
        if !self.active {
            let parent = self.dir.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "session path has no parent",
                )
            })?;
            fs::create_dir_all(parent)?;
            sync_dir(parent)?;
            if let Some(grandparent) = parent.parent() {
                sync_dir(grandparent)?;
            }
            create_private_dir(&self.dir)?;
            let mut file = open_private_new(&self.dir.join("journal.json"))?;
            serde_json::to_writer(
                &mut file,
                &json!({
                    "version": 1,
                    "turn_id": self.turn_id,
                    "state": "uncertain",
                    "recovery": "manual-reconciliation-required"
                }),
            )
            .map_err(std::io::Error::other)?;
            file.sync_all()?;
            sync_dir(&self.dir)?;
            if let Some(parent) = self.dir.parent() {
                sync_dir(parent)?;
            }
            self.active = true;
        }
        let call_index = self.next_call;
        let file_name = format!("call-{call_index:06}.json");
        self.next_call += 1;
        let mut file = open_private_new(&self.dir.join(file_name))?;
        serde_json::to_writer(
            &mut file,
            &json!({
                "version": 1,
                "turn_id": call.key.turn_id,
                "call_id": call.key.call_id,
                "tool": call.name,
                "state": "dispatch-intent",
            }),
        )
        .map_err(std::io::Error::other)?;
        file.sync_all()?;
        sync_dir(&self.dir)?;
        self.current_call = call_index;
        Ok(())
    }

    fn outcome(&self, call_id: &str, state: &str) -> std::io::Result<()> {
        let name = format!("{:06}", self.current_call);
        let path = self.dir.join(format!("outcome-{name}.json"));
        let temp = self.dir.join(format!(".outcome-{name}.tmp"));
        let mut file = open_private_new(&temp)?;
        serde_json::to_writer(
            &mut file,
            &json!({"version": 1, "call_id": call_id, "state": state}),
        )
        .map_err(std::io::Error::other)?;
        file.sync_all()?;
        fs::rename(temp, path)?;
        sync_dir(&self.dir)
    }

    fn clear_after_commit(&mut self) -> std::io::Result<()> {
        if self.active {
            fs::remove_dir_all(&self.dir)?;
            if let Some(parent) = self.dir.parent() {
                sync_dir(parent)?;
            }
            self.active = false;
        }
        Ok(())
    }
}

fn core_error(error: core::AgentError) -> AppError {
    AppError::new(match error.phase {
        core::AgentPhase::Cancelled => "core tool workflow: cancelled",
        core::AgentPhase::Validation => "core tool workflow: invalid request",
        core::AgentPhase::Tools => "core tool workflow: tool failed or was denied",
        core::AgentPhase::Storage => "core tool workflow: session append failed",
        _ => "core tool workflow: agent failed",
    })
}

struct CancelAdapter<'a>(&'a Cancel);
impl core::Cancellation for CancelAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[derive(Clone)]
struct ToolEvent {
    id: String,
    name: String,
    args: Value,
    outcome: ToolOutcomeSnapshot,
}

#[derive(Clone)]
struct ToolOutcomeSnapshot {
    llm: String,
    ui: Option<String>,
    is_error: bool,
}
impl From<&ToolOutcome> for ToolOutcomeSnapshot {
    fn from(value: &ToolOutcome) -> Self {
        Self {
            llm: value.llm.clone(),
            ui: value.ui.clone(),
            is_error: value.is_error,
        }
    }
}
struct NativeWorkflowProvider<'a, P: LlmProvider + ?Sized> {
    provider: &'a P,
    generated: Rc<RefCell<Vec<Message>>>,
    responses: Rc<RefCell<Vec<ChatResponse>>>,
    original_error: Rc<RefCell<Option<AppError>>>,
    tool_step_content_bytes: usize,
}

impl<P: LlmProvider + ?Sized> core::ToolWorkflowProvider for NativeWorkflowProvider<'_, P> {
    fn infer_step(
        &mut self,
        request: core::WorkflowRequest,
        cancel: &dyn core::Cancellation,
    ) -> std::result::Result<core::ModelStep, core::AgentError> {
        if cancel.is_cancelled() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Cancelled,
                message: "cancelled".into(),
            });
        }
        let messages: Vec<Message> = serde_json::from_value(request.input["messages"].clone())
            .map_err(|_| core::AgentError {
                phase: core::AgentPhase::Validation,
                message: "invalid native messages".into(),
            })?;
        let mut messages = messages;
        let prior_responses = self.responses.borrow();
        if prior_responses.len() != request.transcript.len() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Validation,
                message: "native response history does not match workflow transcript".into(),
            });
        }
        for (exchange, prior_response) in request.transcript.iter().zip(prior_responses.iter()) {
            if let core::ModelStep::ToolRequests(calls) = &exchange.step {
                messages.push(Message::assistant_with_tool_calls(
                    prior_response.content.clone(),
                    calls
                        .iter()
                        .map(|call| NativeToolCall {
                            id: call.key.call_id.clone(),
                            kind: "function".into(),
                            function: FunctionCall {
                                name: call.name.clone(),
                                arguments: serde_json::to_string(&call.arguments)
                                    .unwrap_or_else(|_| "{}".into()),
                            },
                        })
                        .collect(),
                ));
                for result in &exchange.results {
                    let content = match &result.status {
                        core::ToolStatus::Completed(value) => {
                            value["llm"].as_str().unwrap_or_default().to_owned()
                        }
                        core::ToolStatus::Denied(reason)
                        | core::ToolStatus::FailedNoEffect(reason)
                        | core::ToolStatus::Unknown(reason) => reason.clone(),
                    };
                    messages.push(Message::tool(result.key.call_id.clone(), content));
                }
            }
        }
        let tool_defs = request
            .tools
            .iter()
            .map(|tool| {
                ToolDef::function(
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.input_schema.clone(),
                )
            })
            .collect();
        drop(prior_responses);
        let native_request = ChatRequest {
            model: request
                .input
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            messages,
            max_tokens: request.input["max_tokens"].as_i64(),
            tools: tool_defs,
            ..Default::default()
        };
        let response = match self.provider.chat(&native_request) {
            Ok(response) => response,
            Err(error) => {
                *self.original_error.borrow_mut() = Some(error);
                return Err(core::AgentError {
                    phase: core::AgentPhase::Provider,
                    message: "native provider failed".into(),
                });
            }
        };
        if cancel.is_cancelled() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Cancelled,
                message: "cancelled".into(),
            });
        }
        if !response.tool_calls.is_empty() {
            let content_bytes = response.content.len();
            if content_bytes > MAX_PAYLOAD_BYTES
                || self.tool_step_content_bytes.saturating_add(content_bytes) > MAX_TRANSCRIPT_BYTES
            {
                return Err(core::AgentError {
                    phase: core::AgentPhase::Validation,
                    message: "assistant tool-step content exceeds workflow bounds".into(),
                });
            }
            self.tool_step_content_bytes += content_bytes;
        }
        let step = if response.tool_calls.is_empty() {
            self.generated
                .borrow_mut()
                .push(Message::assistant(response.content.clone()));
            core::ModelStep::Terminal(response_value(&response))
        } else {
            let mut calls = Vec::with_capacity(response.tool_calls.len());
            for call in &response.tool_calls {
                let arguments: Value =
                    serde_json::from_str(&call.function.arguments).map_err(|_| {
                        core::AgentError {
                            phase: core::AgentPhase::Validation,
                            message: "malformed tool arguments".into(),
                        }
                    })?;
                calls.push(core::ToolCall {
                    key: core::CallKey {
                        turn_id: request.turn_id.clone(),
                        call_id: call.id.clone(),
                    },
                    name: call.function.name.clone(),
                    arguments,
                });
            }
            self.generated
                .borrow_mut()
                .push(Message::assistant_with_tool_calls(
                    response.content.clone(),
                    response.tool_calls.clone(),
                ));
            core::ModelStep::ToolRequests(calls)
        };
        self.responses.borrow_mut().push(response);
        Ok(step)
    }
}

fn response_value(response: &ChatResponse) -> Value {
    json!({
        "content": response.content,
        "tool_calls": response.tool_calls,
        "model": response.model,
        "finish_reason": response.finish_reason,
        "truncated": response.truncated,
        "usage": response.usage.as_ref().map(|usage| json!({
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
        })),
    })
}

struct NativeWorkflowManager {}
impl core::ToolWorkflowManager for NativeWorkflowManager {
    fn prepare_workflow(
        &mut self,
        input: core::TurnInput,
        system_prompt: &str,
    ) -> std::result::Result<core::WorkflowRequest, core::AgentError> {
        Ok(core::WorkflowRequest {
            turn_id: input.turn_id,
            system_prompt: system_prompt.to_owned(),
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
        let _ = results;
        let mut next = request.clone();
        next.transcript.push(core::WorkflowExchange {
            step: step.clone(),
            results: results.to_vec(),
        });
        Ok(next)
    }
}

struct NativeToolExecutor<'a> {
    tools: &'a NativeToolRegistry,
    context: &'a ToolContext,
    approver: &'a mut dyn Approver,
    events: Rc<RefCell<Vec<ToolEvent>>>,
    generated: Rc<RefCell<Vec<Message>>>,
    journal: Rc<RefCell<DispatchJournal>>,
    poisoned: Rc<Cell<bool>>,
    original_error: Rc<RefCell<Option<AppError>>>,
}
impl core::ToolExecutor for NativeToolExecutor<'_> {
    fn execute(
        &mut self,
        call: &core::ToolCall,
        cancel: &dyn core::Cancellation,
    ) -> core::ToolResult {
        let key = call.key.clone();
        let make = |status| core::ToolResult {
            key: key.clone(),
            status,
        };
        if cancel.is_cancelled() {
            return make(core::ToolStatus::FailedNoEffect(
                "cancelled before dispatch".into(),
            ));
        }
        let side_effecting = self
            .tools
            .get(&call.name)
            .map(|tool| tool.is_side_effecting())
            .unwrap_or(true);
        if side_effecting
            && !self
                .approver
                .approve(&call.key.call_id, &call.name, &call.arguments)
        {
            let outcome = ToolOutcome::error(format!("tool call `{}` denied", call.name));
            self.events.borrow_mut().push(ToolEvent {
                id: call.key.call_id.clone(),
                name: call.name.clone(),
                args: call.arguments.clone(),
                outcome: ToolOutcomeSnapshot::from(&outcome),
            });
            self.generated
                .borrow_mut()
                .push(Message::tool(call.key.call_id.clone(), outcome.llm.clone()));
            return make(core::ToolStatus::Denied(outcome.llm));
        }
        if let Err(error) = self.journal.borrow_mut().begin(call) {
            self.poisoned.set(true);
            *self.original_error.borrow_mut() = Some(AppError::new(format!(
                "dispatch journal could not be made durable; tool was not dispatched: {error}"
            )));
            return make(core::ToolStatus::FailedNoEffect(format!(
                "dispatch journal could not be made durable; tool was not dispatched: {error}"
            )));
        }
        let outcome = self.tools.run(self.context, &call.name, &call.arguments);
        self.events.borrow_mut().push(ToolEvent {
            id: call.key.call_id.clone(),
            name: call.name.clone(),
            args: call.arguments.clone(),
            outcome: ToolOutcomeSnapshot::from(&outcome),
        });
        let status = if outcome.is_error {
            // The native ToolOutcome has no no-effect guarantee. Preserve the
            // possibility that a failing tool partially acted; never retry it.
            core::ToolStatus::Unknown(outcome.llm.clone())
        } else {
            core::ToolStatus::Completed(json!({"llm": outcome.llm, "ui": outcome.ui}))
        };
        let state = match &status {
            core::ToolStatus::Completed(_) => "completed",
            core::ToolStatus::Denied(_) => "denied",
            core::ToolStatus::FailedNoEffect(_) => "failed-no-effect",
            core::ToolStatus::Unknown(_) => "unknown",
        };
        if self
            .journal
            .borrow()
            .outcome(&call.key.call_id, state)
            .is_err()
        {
            self.poisoned.set(true);
        }
        let detail = match &status {
            core::ToolStatus::Completed(value) => value["llm"].as_str().unwrap_or_default(),
            core::ToolStatus::Denied(reason)
            | core::ToolStatus::FailedNoEffect(reason)
            | core::ToolStatus::Unknown(reason) => reason,
        };
        self.generated
            .borrow_mut()
            .push(Message::tool(call.key.call_id.clone(), detail));
        make(status)
    }
}

struct SessionAppend<'a> {
    session: &'a SessionStore,
    pending: Vec<Message>,
    generated: Rc<RefCell<Vec<Message>>>,
    cursor: &'a mut usize,
    committed_len: usize,
    original_error: Rc<RefCell<Option<AppError>>>,
    journal: Rc<RefCell<DispatchJournal>>,
    poisoned: Rc<Cell<bool>>,
}
impl core::AgentStorage for SessionAppend<'_> {
    fn append(
        &mut self,
        record: &core::TurnRecord,
    ) -> std::result::Result<core::StorageReceipt, core::AgentError> {
        if record.response["final"]["content"].as_str().is_none() {
            return Err(core::AgentError {
                phase: core::AgentPhase::Manager,
                message: "missing terminal content".into(),
            });
        }
        let mut messages = self.pending.clone();
        messages.extend(self.generated.borrow().iter().cloned());
        if let Err(error) = self.session.append(&messages) {
            self.poisoned.set(true);
            *self.original_error.borrow_mut() = Some(error);
            return Err(core::AgentError {
                phase: core::AgentPhase::Storage,
                message: "append failed".into(),
            });
        }
        *self.cursor = self.committed_len + self.generated.borrow().len();
        if let Err(error) = self.journal.borrow_mut().clear_after_commit() {
            self.poisoned.set(true);
            *self.original_error.borrow_mut() = Some(AppError::new(format!(
                "session turn committed but dispatch journal cleanup failed: {error}"
            )));
            return Err(core::AgentError {
                phase: core::AgentPhase::Storage,
                message: "turn committed but recovery journal remains".into(),
            });
        }
        Ok(core::StorageReceipt {
            reference: self.session.path().display().to_string(),
        })
    }
}

fn report_tool_events(observer: &mut dyn Observer, events: &[ToolEvent]) {
    for event in events {
        observer.on_tool_call(&event.id, &event.name, &event.args);
        let outcome = if event.outcome.is_error {
            ToolOutcome::error(event.outcome.llm.clone())
        } else {
            ToolOutcome::ok(event.outcome.llm.clone())
        };
        let outcome = match &event.outcome.ui {
            Some(ui) => outcome.with_ui(ui.clone()),
            None => outcome,
        };
        observer.on_tool_result(&event.id, &event.name, &outcome);
    }
}

/// Execute a serial bounded workflow against the existing native chat provider,
/// scoped tool registry/approval seam, and named JSONL session.
pub(crate) fn run_tool_workflow<P: LlmProvider + ?Sized>(
    provider: &P,
    request: &ChatRequest,
    conversation: &mut Vec<Message>,
    observer: &mut dyn Observer,
    cancel: &Cancel,
    persisted: &mut usize,
    session: Option<&SessionStore>,
    poisoned: &mut bool,
    tools: &NativeToolRegistry,
    context: &ToolContext,
    approver: &mut dyn Approver,
    max_steps: usize,
) -> Result<ChatResponse> {
    let session =
        session.ok_or_else(|| AppError::new("core tool workflow: named session required"))?;
    ensure_no_pending_dispatches(session)?;
    if *poisoned {
        return Err(AppError::new(
            "core tool workflow: session requires reopen or reconciliation",
        ));
    }
    if *persisted > conversation.len() || request.messages.last().is_none_or(|m| m.role != "user") {
        return Err(AppError::new("core tool workflow: invalid request history"));
    }
    if max_steps == 0 {
        return Err(AppError::new(
            "core tool workflow: max steps must be positive",
        ));
    }
    let system_prompt = request
        .messages
        .iter()
        .find(|message| message.role == "system")
        .and_then(|message| message.content.as_deref())
        .filter(|prompt| !prompt.trim().is_empty())
        .ok_or_else(|| AppError::new("core tool workflow: system prompt required"))?;

    let generated = Rc::new(RefCell::new(Vec::new()));
    let poison_state = Rc::new(Cell::new(*poisoned));
    let responses = Rc::new(RefCell::new(Vec::<ChatResponse>::new()));
    let events = Rc::new(RefCell::new(Vec::<ToolEvent>::new()));
    let original_error = Rc::new(RefCell::new(None));
    let native_provider = NativeWorkflowProvider {
        provider,
        generated: generated.clone(),
        responses: responses.clone(),
        original_error: original_error.clone(),
        tool_step_content_bytes: 0,
    };
    let manager = NativeWorkflowManager {};
    let turn_id = format!(
        "{}-{}",
        session
            .path()
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("session"),
        *persisted
    );
    let journal = Rc::new(RefCell::new(DispatchJournal::new(session, turn_id.clone())));
    let executor = NativeToolExecutor {
        tools,
        context,
        approver,
        events: events.clone(),
        generated: generated.clone(),
        journal: journal.clone(),
        poisoned: poison_state.clone(),
        original_error: original_error.clone(),
    };
    let pending = conversation[*persisted..].to_vec();
    let committed_len = conversation.len();
    let storage = SessionAppend {
        session,
        pending,
        generated: generated.clone(),
        cursor: persisted,
        committed_len,
        original_error: original_error.clone(),
        journal: journal.clone(),
        poisoned: poison_state.clone(),
    };
    let mut actor = core::ToolWorkflowAgent::new(
        native_provider,
        manager,
        executor,
        storage,
        system_prompt.to_owned(),
        core::WorkflowLimits {
            max_model_steps: max_steps.min(MAX_MODEL_STEPS),
            max_tool_calls: MAX_TOOL_CALLS,
            max_payload_bytes: MAX_PAYLOAD_BYTES,
            max_transcript_bytes: MAX_TRANSCRIPT_BYTES,
        },
    )
    .map_err(core_error)?;
    let definitions = tools
        .defs()
        .into_iter()
        .map(|definition| core::ToolDefinition {
            name: definition.function.name,
            description: definition.function.description,
            input_schema: definition.function.parameters,
        })
        .collect();
    actor.set_tool_registry(Box::new(NativeToolRegistryAdapter(definitions)));

    let input = json!({
        "model": request.model.clone(),
        "max_tokens": request.max_tokens,
        "messages": request.messages.clone(),
    });
    let outcome = actor.run_turn(core::TurnInput { turn_id, input }, &CancelAdapter(cancel));
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(failure) => {
            report_tool_events(observer, &events.borrow());
            if poison_state.get() || journal.borrow().active {
                poison_state.set(true);
                *poisoned = true;
                conversation.extend(generated.borrow().iter().cloned());
                eprintln!(
                    "Core tool workflow stopped with uncertain tool effects. Session is blocked until the dispatch journal at {} is manually reconciled; generated failure transcript remains in memory.",
                    journal.borrow().dir.display()
                );
            }
            if let Some(error) = original_error.borrow_mut().take() {
                return Err(error);
            }
            return Err(core_error(failure.error));
        }
    };
    let response = responses
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| AppError::new("core tool workflow: missing provider response"))?;
    report_tool_events(observer, &events.borrow());
    for step_response in responses.borrow().iter() {
        observer.on_turn_step(step_response);
    }
    *poisoned = poison_state.get();
    if response.truncated {
        observer.on_notice("[generation was cut short — showing partial output]");
    }
    if !response.content.is_empty() {
        observer.on_text_delta(&response.content);
    }
    conversation.extend(generated.borrow().iter().cloned());
    let _ = outcome.receipt;
    Ok(response)
}

struct NativeToolRegistryAdapter(Vec<core::ToolDefinition>);
impl core::ToolRegistry for NativeToolRegistryAdapter {
    fn definitions(&self) -> std::result::Result<Vec<core::ToolDefinition>, core::AgentError> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::core::ToolExecutor;
    use super::*;
    use crate::agent::runner::AllowAll;
    use crate::agent::tools::Tool;
    use std::cell::Cell;

    struct EffectProbe(Rc<Cell<usize>>);
    impl Tool for EffectProbe {
        fn name(&self) -> &str {
            "effect_probe"
        }
        fn description(&self) -> &str {
            "test-only effect probe"
        }
        fn parameters(&self) -> Value {
            json!({"type":"object"})
        }
        fn run(&self, _context: &ToolContext, _args: &Value) -> ToolOutcome {
            self.0.set(self.0.get() + 1);
            ToolOutcome::ok("effect happened")
        }
    }

    #[test]
    fn journal_write_failure_prevents_executor_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let marker_file = temp.path().join("session.jsonl.core-tool-workflow-pending");
        fs::write(&marker_file, b"occupied; do not overwrite").unwrap();
        let effect_calls = Rc::new(Cell::new(0));
        let mut tools = NativeToolRegistry::new();
        tools.add(Box::new(EffectProbe(effect_calls.clone())));
        let mut approver = AllowAll;
        let poisoned = Rc::new(Cell::new(false));
        let generated = Rc::new(RefCell::new(Vec::new()));
        let journal = Rc::new(RefCell::new(DispatchJournal {
            dir: marker_file.clone(),
            active: false,
            turn_id: "turn-1".into(),
            next_call: 0,
            current_call: 0,
        }));
        let mut executor = NativeToolExecutor {
            tools: &tools,
            context: &ToolContext::new(temp.path()),
            approver: &mut approver,
            events: Rc::new(RefCell::new(Vec::new())),
            generated,
            journal,
            poisoned: poisoned.clone(),
            original_error: Rc::new(RefCell::new(None)),
        };
        let call = core::ToolCall {
            key: core::CallKey {
                turn_id: "turn-1".into(),
                call_id: "call-1".into(),
            },
            name: "effect_probe".into(),
            arguments: json!({}),
        };

        let result = executor.execute(&call, &CancelAdapter(&Cancel::new()));

        assert!(matches!(result.status, core::ToolStatus::FailedNoEffect(_)));
        assert_eq!(
            effect_calls.get(),
            0,
            "no tool effect before durable intent"
        );
        assert!(poisoned.get(), "failed journal setup blocks later use");
        assert_eq!(
            fs::read(&marker_file).unwrap(),
            b"occupied; do not overwrite"
        );
    }
}
