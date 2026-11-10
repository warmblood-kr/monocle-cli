use crate::types::*;

pub(crate) fn error(phase: AgentPhase, message: impl Into<String>) -> AgentError {
    AgentError {
        phase,
        message: message.into(),
    }
}

pub(crate) fn check_cancelled(cancel: &dyn Cancellation) -> Result<(), AgentError> {
    if cancel.is_cancelled() {
        Err(error(AgentPhase::Cancelled, "cancelled"))
    } else {
        Ok(())
    }
}

/// Re-label a component error with the stage that was invoked. Only the
/// provider may preserve an explicitly reported cooperative cancellation.
pub(crate) fn stage_error(
    mut err: AgentError,
    phase: AgentPhase,
    preserve_cancelled: bool,
) -> AgentError {
    if !preserve_cancelled || err.phase != AgentPhase::Cancelled {
        err.phase = phase;
    }
    err
}

/// One mutable agent executes one synchronous turn at a time.
pub struct Agent<P: LlmProvider, M: ChatTurnManager, S: AgentStorage> {
    provider: P,
    manager: M,
    storage: S,
    pub(super) system_prompt: String,
    tool_registry: Option<Box<dyn ToolRegistry>>,
}

impl<P: LlmProvider, M: ChatTurnManager, S: AgentStorage> Agent<P, M, S> {
    /// Validate configuration without invoking a component or performing I/O.
    pub fn new(
        provider: P,
        manager: M,
        storage: S,
        system_prompt: String,
    ) -> Result<Self, AgentError> {
        if system_prompt.trim().is_empty() {
            return Err(error(AgentPhase::Validation, "empty system prompt"));
        }
        Ok(Self {
            provider,
            manager,
            storage,
            system_prompt,
            tool_registry: None,
        })
    }

    /// Store an optional registry without invoking it. Its definitions may be
    /// disclosed to this Agent's configured provider, so the host must scope
    /// them to the current caller, tenant, and model and ensure they are safe
    /// for that disclosure. Agent does not enforce principal authorization;
    /// execution must authorize separately. Replace or clear the registry
    /// whenever caller, tenant, or model authority changes.
    pub fn set_tool_registry(&mut self, registry: Box<dyn ToolRegistry>) {
        self.tool_registry = Some(registry);
    }

    /// Remove this agent's model-visible tool definitions.
    pub fn clear_tool_registry(&mut self) {
        self.tool_registry = None;
    }

    /// Prepare, infer, finalize and append once each, with cooperative checks
    /// before every stage. No stage is retried and storage errors do not retry
    /// inference. An error does not claim that earlier side effects were absent.
    pub fn run_turn(
        &mut self,
        input: TurnInput,
        cancel: &dyn Cancellation,
    ) -> Result<AgentOutcome, AgentError> {
        if input.turn_id.trim().is_empty() {
            return Err(error(AgentPhase::Validation, "empty turn id"));
        }
        let turn_id = input.turn_id.clone();

        check_cancelled(cancel)?;
        let request = self
            .manager
            .prepare(input, &self.system_prompt)
            .map_err(|err| stage_error(err, AgentPhase::Manager, false))?;
        // Validate identity before sampling cancellation at the next boundary.
        if request.turn_id != turn_id || request.system_prompt != self.system_prompt {
            return Err(error(AgentPhase::Manager, "request identity changed"));
        }

        check_cancelled(cancel)?;
        let mut request = request;
        if let Some(registry) = self.tool_registry.as_ref() {
            let definitions = registry.definitions().map_err(|err| AgentError {
                phase: AgentPhase::Tools,
                message: err.message,
            })?;
            validate_tool_definitions(&definitions)?;
            request.tools = definitions;
            check_cancelled(cancel)?;
        } else {
            request.tools.clear();
        }
        let response = self
            .provider
            .infer(request, cancel)
            .map_err(|err| stage_error(err, AgentPhase::Provider, true))?;

        check_cancelled(cancel)?;
        let expected_response = response.payload.clone();
        let record = self
            .manager
            .finish(&turn_id, response)
            .map_err(|err| stage_error(err, AgentPhase::Manager, false))?;
        // Validate identity before sampling cancellation at the next boundary.
        if record.turn_id != turn_id || record.response != expected_response {
            return Err(error(AgentPhase::Manager, "record identity changed"));
        }

        check_cancelled(cancel)?;
        let receipt = self
            .storage
            .append(&record)
            .map_err(|err| stage_error(err, AgentPhase::Storage, false))?;
        Ok(AgentOutcome {
            response: record.response,
            receipt,
        })
    }
}

fn validate_tool_definitions(definitions: &[ToolDefinition]) -> Result<(), AgentError> {
    if definitions.len() > 128 {
        return Err(error(AgentPhase::Tools, "too many tool definitions"));
    }
    let mut names = std::collections::HashSet::with_capacity(definitions.len());
    for definition in definitions {
        if definition.name.is_empty() || definition.name.trim() != definition.name {
            return Err(error(AgentPhase::Tools, "invalid tool name"));
        }
        if !names.insert(definition.name.as_bytes()) {
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
