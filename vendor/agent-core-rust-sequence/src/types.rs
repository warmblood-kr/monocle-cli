use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPhase {
    Validation,
    Cancelled,
    Manager,
    Tools,
    Provider,
    Storage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub phase: AgentPhase,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnInput {
    pub turn_id: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Supplies model-visible tool definitions only; this grants no execution authority.
///
/// The host must install only definitions safe for disclosure through this
/// specific `Agent` and its configured provider. Scope the definitions to the
/// current caller, tenant, and model. `Agent` does not enforce principal
/// authorization; any tool execution must perform authorization separately.
pub trait ToolRegistry {
    fn definitions(&self) -> Result<Vec<ToolDefinition>, AgentError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub turn_id: String,
    pub system_prompt: String,
    pub input: Value,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnRecord {
    pub turn_id: String,
    pub response: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReceipt {
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    pub response: Value,
    pub receipt: StorageReceipt,
}

/// Cooperative cancellation signal. Synchronous callbacks cannot be forcibly
/// interrupted; their host owns cancellation and joining of blocking work.
pub trait Cancellation {
    fn is_cancelled(&self) -> bool;
}

pub trait LlmProvider {
    fn infer(
        &mut self,
        request: LlmRequest,
        cancel: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError>;
}

pub trait ChatTurnManager {
    fn prepare(&mut self, input: TurnInput, system_prompt: &str) -> Result<LlmRequest, AgentError>;

    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError>;
}

pub trait AgentStorage {
    fn append(&mut self, record: &TurnRecord) -> Result<StorageReceipt, AgentError>;
}
