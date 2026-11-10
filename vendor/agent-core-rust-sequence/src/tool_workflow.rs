//! Optional bounded serial tool workflow. The host supplies every capability;
//! this module validates protocol state and never retries a callback.
use crate::agent::{check_cancelled, error, stage_error};
use crate::types::*;
use serde_json::{json, Value};
use std::collections::HashSet;

const ABSOLUTE_MAX_STEPS: usize = 128;
const ABSOLUTE_MAX_CALLS: usize = 128;
const ABSOLUTE_MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const ABSOLUTE_MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
const MAX_DEFINITIONS: usize = 128;
const MAX_IDENTIFIER_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallKey {
    pub turn_id: String,
    pub call_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub key: CallKey,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelStep {
    Terminal(Value),
    ToolRequests(Vec<ToolCall>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Completed(Value),
    Denied(String),
    FailedNoEffect(String),
    /// The executor cannot establish whether the requested effect occurred.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub key: CallKey,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowExchange {
    pub step: ModelStep,
    pub results: Vec<ToolResult>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRequest {
    pub turn_id: String,
    pub system_prompt: String,
    pub input: Value,
    pub tools: Vec<ToolDefinition>,
    pub transcript: Vec<WorkflowExchange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowLimits {
    pub max_model_steps: usize,
    pub max_tool_calls: usize,
    pub max_payload_bytes: usize,
    pub max_transcript_bytes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowFailure {
    pub error: AgentError,
    /// Retains all trusted observations, including prior completed effects.
    pub transcript: Vec<WorkflowExchange>,
    /// Canonical dispatched calls whose effects or result correlation are uncertain.
    pub unresolved: Vec<CallKey>,
}

pub trait ToolWorkflowProvider {
    fn infer_step(
        &mut self,
        request: WorkflowRequest,
        cancel: &dyn Cancellation,
    ) -> Result<ModelStep, AgentError>;
}

pub trait ToolWorkflowManager {
    fn prepare_workflow(
        &mut self,
        input: TurnInput,
        system_prompt: &str,
    ) -> Result<WorkflowRequest, AgentError>;

    fn apply_tool_results(
        &mut self,
        request: &WorkflowRequest,
        step: &ModelStep,
        results: &[ToolResult],
    ) -> Result<WorkflowRequest, AgentError>;
}

/// Host-owned, serial effect executor. It must authorize the caller and
/// validate arguments against the host's current schema before any effect.
pub trait ToolExecutor {
    fn execute(&mut self, call: &ToolCall, cancel: &dyn Cancellation) -> ToolResult;
}

pub struct ToolWorkflowAgent<P, M, E, S> {
    provider: P,
    manager: M,
    executor: E,
    storage: S,
    system_prompt: String,
    limits: WorkflowLimits,
    tool_registry: Option<Box<dyn ToolRegistry>>,
}

impl<P, M, E, S> ToolWorkflowAgent<P, M, E, S>
where
    P: ToolWorkflowProvider,
    M: ToolWorkflowManager,
    E: ToolExecutor,
    S: AgentStorage,
{
    pub fn new(
        provider: P,
        manager: M,
        executor: E,
        storage: S,
        system_prompt: String,
        limits: WorkflowLimits,
    ) -> Result<Self, AgentError> {
        if system_prompt.trim().is_empty()
            || limits.max_model_steps == 0
            || limits.max_tool_calls == 0
            || limits.max_payload_bytes == 0
            || limits.max_transcript_bytes == 0
            || limits.max_model_steps > ABSOLUTE_MAX_STEPS
            || limits.max_tool_calls > ABSOLUTE_MAX_CALLS
            || limits.max_payload_bytes > ABSOLUTE_MAX_PAYLOAD_BYTES
            || limits.max_transcript_bytes > ABSOLUTE_MAX_TRANSCRIPT_BYTES
        {
            return Err(error(
                AgentPhase::Validation,
                "invalid workflow configuration",
            ));
        }
        check_payload(
            &json!(system_prompt),
            limits.max_payload_bytes,
            "system prompt",
        )?;
        Ok(Self {
            provider,
            manager,
            executor,
            storage,
            system_prompt,
            limits,
            tool_registry: None,
        })
    }

    /// Definitions disclose names and schemas to this agent's provider. The
    /// host must scope them to its current caller, tenant, and model. They do
    /// not grant execution authority; the executor authorizes every effect.
    pub fn set_tool_registry(&mut self, registry: Box<dyn ToolRegistry>) {
        self.tool_registry = Some(registry);
    }

    pub fn clear_tool_registry(&mut self) {
        self.tool_registry = None;
    }

    pub fn run_turn(
        &mut self,
        input: TurnInput,
        cancel: &dyn Cancellation,
    ) -> Result<AgentOutcome, WorkflowFailure> {
        let mut transcript = Vec::new();
        let mut unresolved = Vec::new();
        if input.turn_id.trim().is_empty() {
            return Err(failure(
                error(AgentPhase::Validation, "empty turn id"),
                transcript,
                unresolved,
            ));
        }
        if let Err(err) = check_payload(&input.input, self.limits.max_payload_bytes, "turn input") {
            return Err(failure(err, transcript, unresolved));
        }
        let turn_id = input.turn_id.clone();
        if let Err(err) = check_cancelled(cancel) {
            return Err(failure(err, transcript, unresolved));
        }
        let mut request = match self
            .manager
            .prepare_workflow(input.clone(), &self.system_prompt)
        {
            Ok(request) => request,
            Err(err) => {
                return Err(failure(
                    stage_error(err, AgentPhase::Manager, false),
                    transcript,
                    unresolved,
                ));
            }
        };
        if request.turn_id != turn_id
            || request.system_prompt != self.system_prompt
            || request.input != input.input
            || !request.transcript.is_empty()
        {
            return Err(failure(
                error(
                    AgentPhase::Manager,
                    "request identity or initial transcript changed",
                ),
                transcript,
                unresolved,
            ));
        }
        if let Err(err) = check_cancelled(cancel) {
            return Err(failure(err, transcript, unresolved));
        }
        if let Some(registry) = self.tool_registry.as_ref() {
            request.tools = match registry.definitions() {
                Ok(tools) => tools,
                Err(err) => {
                    return Err(failure(
                        stage_error(err, AgentPhase::Tools, false),
                        transcript,
                        unresolved,
                    ));
                }
            };
        } else {
            // As in the model-only agent, manager-supplied names are not a
            // disclosure grant. Only the host registry exposes tools.
            request.tools.clear();
        }
        if let Err(err) = validate_definitions(&request.tools, self.limits.max_payload_bytes) {
            return Err(failure(err, transcript, unresolved));
        }
        if let Err(err) = check_cancelled(cancel) {
            return Err(failure(err, transcript, unresolved));
        }
        let disclosed: HashSet<String> =
            request.tools.iter().map(|tool| tool.name.clone()).collect();
        let mut seen_call_ids = HashSet::new();
        let mut model_steps = 0usize;
        let mut dispatched_calls = 0usize;

        loop {
            if model_steps >= self.limits.max_model_steps {
                return Err(failure(
                    error(AgentPhase::Provider, "model step limit exceeded"),
                    transcript,
                    unresolved,
                ));
            }
            if let Err(err) = check_cancelled(cancel) {
                return Err(failure(err, transcript, unresolved));
            }
            model_steps += 1;
            let step = match self.provider.infer_step(request.clone(), cancel) {
                Ok(step) => step,
                Err(err) => {
                    return Err(failure(
                        stage_error(err, AgentPhase::Provider, true),
                        transcript,
                        unresolved,
                    ));
                }
            };
            if let Err(err) = check_payload(
                &wire_step(&step),
                self.limits.max_payload_bytes,
                "model step",
            ) {
                return Err(failure(err, transcript, unresolved));
            }
            let step_prefix = append_exchange(
                transcript.clone(),
                WorkflowExchange {
                    step: step.clone(),
                    results: vec![],
                },
            );
            if let Err(err) = validate_transcript(&step_prefix, self.limits.max_transcript_bytes) {
                return Err(failure(err, transcript, unresolved));
            }
            match step.clone() {
                ModelStep::Terminal(final_payload) => {
                    let completed = step_prefix;
                    let stored = json!({
                        "tool_workflow": { "version": 1, "transcript": wire_transcript(&completed) },
                        "final": final_payload,
                    });
                    if let Err(err) =
                        check_payload(&stored, self.limits.max_transcript_bytes, "stored record")
                    {
                        return Err(failure(err, completed, unresolved));
                    }
                    if let Err(err) = check_cancelled(cancel) {
                        return Err(failure(err, completed, unresolved));
                    }
                    let record = TurnRecord {
                        turn_id,
                        response: stored,
                    };
                    let receipt = match self.storage.append(&record) {
                        Ok(receipt) => receipt,
                        Err(err) => {
                            return Err(failure(
                                stage_error(err, AgentPhase::Storage, false),
                                completed,
                                unresolved,
                            ));
                        }
                    };
                    return Ok(AgentOutcome {
                        response: final_payload,
                        receipt,
                    });
                }
                ModelStep::ToolRequests(calls) => {
                    let step = ModelStep::ToolRequests(calls.clone());
                    if let Err(err) = validate_batch(
                        &calls,
                        &turn_id,
                        &disclosed,
                        &seen_call_ids,
                        self.limits.max_tool_calls.saturating_sub(dispatched_calls),
                        self.limits.max_payload_bytes,
                    ) {
                        return Err(failure(err, step_prefix.clone(), unresolved));
                    }
                    let reserve = calls
                        .len()
                        .saturating_mul(self.limits.max_payload_bytes.saturating_add(64));
                    let current_bytes = match serde_json::to_vec(&wire_transcript(&step_prefix)) {
                        Ok(bytes) => bytes.len(),
                        Err(_) => {
                            return Err(failure(
                                error(AgentPhase::Validation, "cannot encode workflow batch"),
                                step_prefix.clone(),
                                unresolved,
                            ));
                        }
                    };
                    if current_bytes.saturating_add(reserve) > self.limits.max_transcript_bytes {
                        return Err(failure(
                            error(
                                AgentPhase::Validation,
                                "tool batch exceeds transcript byte budget",
                            ),
                            step_prefix,
                            unresolved,
                        ));
                    }
                    let mut results = Vec::with_capacity(calls.len());
                    for call in calls {
                        if let Err(err) = check_cancelled(cancel) {
                            return Err(failure(
                                err,
                                append_exchange(
                                    transcript,
                                    WorkflowExchange {
                                        step: step.clone(),
                                        results,
                                    },
                                ),
                                unresolved,
                            ));
                        }
                        // Preflight accounted for the whole batch; consume a slot
                        // immediately before dispatch, without refund on failure.
                        dispatched_calls += 1;
                        seen_call_ids.insert(call.key.call_id.clone());
                        let result = self.executor.execute(&call, cancel);
                        if result.key != call.key {
                            let observed = if check_payload(
                                &wire_result(&result),
                                self.limits.max_payload_bytes,
                                "tool result",
                            )
                            .is_ok()
                            {
                                results.push(result);
                                results
                            } else {
                                results
                            };
                            unresolved.push(call.key.clone());
                            return Err(failure(
                                error(AgentPhase::Tools, "tool result correlation mismatch"),
                                append_exchange(
                                    transcript,
                                    WorkflowExchange {
                                        step: step.clone(),
                                        results: observed,
                                    },
                                ),
                                unresolved,
                            ));
                        }
                        if let Err(err) = check_payload(
                            &wire_result(&result),
                            self.limits.max_payload_bytes,
                            "tool result",
                        ) {
                            unresolved.push(call.key.clone());
                            return Err(failure(
                                err,
                                append_exchange(
                                    transcript,
                                    WorkflowExchange {
                                        step: step.clone(),
                                        results,
                                    },
                                ),
                                unresolved,
                            ));
                        }
                        let status = result.status.clone();
                        results.push(result);
                        if let Err(err) = check_cancelled(cancel) {
                            return Err(failure(
                                err,
                                append_exchange(
                                    transcript,
                                    WorkflowExchange {
                                        step: step.clone(),
                                        results,
                                    },
                                ),
                                unresolved,
                            ));
                        }
                        match status {
                            ToolStatus::Completed(_) => {}
                            ToolStatus::Unknown(_) => {
                                unresolved.push(call.key.clone());
                                return Err(failure(
                                    error(AgentPhase::Tools, "tool effect status unknown"),
                                    append_exchange(
                                        transcript,
                                        WorkflowExchange {
                                            step: step.clone(),
                                            results,
                                        },
                                    ),
                                    unresolved,
                                ));
                            }
                            ToolStatus::Denied(_) => {
                                return Err(failure(
                                    error(AgentPhase::Tools, "tool call denied"),
                                    append_exchange(
                                        transcript,
                                        WorkflowExchange {
                                            step: step.clone(),
                                            results,
                                        },
                                    ),
                                    unresolved,
                                ));
                            }
                            ToolStatus::FailedNoEffect(_) => {
                                return Err(failure(
                                    error(AgentPhase::Tools, "tool call failed without effect"),
                                    append_exchange(
                                        transcript,
                                        WorkflowExchange {
                                            step: step.clone(),
                                            results,
                                        },
                                    ),
                                    unresolved,
                                ));
                            }
                        }
                    }
                    let exchange = WorkflowExchange {
                        step: step.clone(),
                        results: results.clone(),
                    };
                    let expected = append_exchange(transcript.clone(), exchange);
                    if let Err(err) =
                        validate_transcript(&expected, self.limits.max_transcript_bytes)
                    {
                        return Err(failure(err, expected, unresolved));
                    }
                    if let Err(err) = check_cancelled(cancel) {
                        return Err(failure(err, expected, unresolved));
                    }
                    let next = match self.manager.apply_tool_results(&request, &step, &results) {
                        Ok(next) => next,
                        Err(err) => {
                            return Err(failure(
                                stage_error(err, AgentPhase::Manager, false),
                                expected,
                                unresolved,
                            ));
                        }
                    };
                    if next.turn_id != request.turn_id
                        || next.system_prompt != request.system_prompt
                        || next.input != request.input
                        || next.tools != request.tools
                        || next.transcript != expected
                    {
                        return Err(failure(
                            error(
                                AgentPhase::Manager,
                                "workflow manager changed immutable state or transcript",
                            ),
                            expected,
                            unresolved,
                        ));
                    }
                    request = next;
                    transcript = expected;
                }
            }
        }
    }
}

fn failure(
    error: AgentError,
    transcript: Vec<WorkflowExchange>,
    unresolved: Vec<CallKey>,
) -> WorkflowFailure {
    WorkflowFailure {
        error,
        transcript,
        unresolved,
    }
}

fn append_exchange(
    mut transcript: Vec<WorkflowExchange>,
    exchange: WorkflowExchange,
) -> Vec<WorkflowExchange> {
    transcript.push(exchange);
    transcript
}

fn check_payload(value: &Value, max_bytes: usize, label: &str) -> Result<(), AgentError> {
    let size = serde_json::to_vec(value)
        .map_err(|_| error(AgentPhase::Validation, format!("cannot encode {label}")))?
        .len();
    if size > max_bytes {
        return Err(error(
            AgentPhase::Validation,
            format!("{label} exceeds byte limit"),
        ));
    }
    Ok(())
}

fn validate_transcript(
    transcript: &[WorkflowExchange],
    max_bytes: usize,
) -> Result<(), AgentError> {
    check_payload(
        &wire_transcript(transcript),
        max_bytes,
        "workflow transcript",
    )
}

fn validate_definitions(
    definitions: &[ToolDefinition],
    max_bytes: usize,
) -> Result<(), AgentError> {
    if definitions.len() > MAX_DEFINITIONS {
        return Err(error(AgentPhase::Tools, "too many tool definitions"));
    }
    let wire = Value::Array(definitions.iter().map(|item| json!({
        "name":item.name, "description":item.description, "input_schema":item.input_schema,
    })).collect());
    check_payload(&wire, max_bytes, "tool definitions")?;
    let mut names = HashSet::with_capacity(definitions.len());
    for definition in definitions {
        if definition.name.is_empty()
            || definition.name.trim() != definition.name
            || definition.name.len() > MAX_IDENTIFIER_BYTES
        {
            return Err(error(AgentPhase::Tools, "invalid tool name"));
        }
        if !names.insert(definition.name.as_str()) {
            return Err(error(AgentPhase::Tools, "duplicate tool name"));
        }
        if !definition.input_schema.is_object() {
            return Err(error(
                AgentPhase::Tools,
                "tool input_schema must be an object",
            ));
        }
    }
    Ok(())
}

fn validate_batch(
    calls: &[ToolCall],
    turn_id: &str,
    disclosed: &HashSet<String>,
    seen_call_ids: &HashSet<String>,
    remaining: usize,
    max_payload_bytes: usize,
) -> Result<(), AgentError> {
    if calls.is_empty() {
        return Err(error(AgentPhase::Tools, "empty tool request batch"));
    }
    if calls.len() > remaining {
        return Err(error(AgentPhase::Tools, "tool call limit exceeded"));
    }
    let mut batch_ids = HashSet::with_capacity(calls.len());
    for call in calls {
        if call.key.turn_id != turn_id {
            return Err(error(AgentPhase::Tools, "foreign tool turn id"));
        }
        if call.key.call_id.trim().is_empty() || call.key.call_id.len() > MAX_IDENTIFIER_BYTES {
            return Err(error(AgentPhase::Tools, "invalid tool call id"));
        }
        if !batch_ids.insert(call.key.call_id.as_str()) || seen_call_ids.contains(&call.key.call_id)
        {
            return Err(error(AgentPhase::Tools, "reused tool call id"));
        }
        if !disclosed.contains(&call.name) {
            return Err(error(AgentPhase::Tools, "tool was not disclosed"));
        }
        check_payload(&wire_call(call), max_payload_bytes, "tool call")?;
    }
    Ok(())
}

fn wire_call(call: &ToolCall) -> Value {
    json!({ "key": {"turn_id":call.key.turn_id, "call_id":call.key.call_id}, "name":call.name, "arguments":call.arguments })
}
fn wire_step(step: &ModelStep) -> Value {
    match step {
        ModelStep::Terminal(value) => json!({"Terminal":value}),
        ModelStep::ToolRequests(calls) => {
            json!({"ToolRequests":calls.iter().map(wire_call).collect::<Vec<_>>() })
        }
    }
}
fn wire_result(result: &ToolResult) -> Value {
    let status = match &result.status {
        ToolStatus::Completed(value) => json!({"Completed":value}),
        ToolStatus::Denied(reason) => json!({"Denied":reason}),
        ToolStatus::FailedNoEffect(reason) => json!({"FailedNoEffect":reason}),
        ToolStatus::Unknown(reason) => json!({"Unknown":reason}),
    };
    json!({"key":{"turn_id":result.key.turn_id,"call_id":result.key.call_id},"status":status})
}
fn wire_transcript(transcript: &[WorkflowExchange]) -> Value {
    Value::Array(transcript.iter().map(|exchange| json!({
        "step":wire_step(&exchange.step), "results":exchange.results.iter().map(wire_result).collect::<Vec<_>>(),
    })).collect())
}
