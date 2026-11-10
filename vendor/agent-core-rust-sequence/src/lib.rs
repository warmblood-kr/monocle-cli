//! Model-only injected agent orchestration.
//!
//! Providers, managers and storage are host supplied. This module owns no
//! runtime, network client, database driver, credentials or retry policy.

mod agent;
mod history;
mod manager;
mod provider;
mod storage;
mod tool_workflow;
mod types;

pub use agent::Agent;
pub use history::{
    ArchiveError, ArchiveReceipt, ArchiveReference, ArchiveScope, ArchivedAgent, ArchivedTurn,
    HistoryArchive, HistoryAuthority, HistoryError, HistoryOutcome, HistoryProvider,
    HistoryRequest, HistoryTurnManager, InMemoryHistoryArchive, TurnProvenance, MAX_HISTORY_BYTES,
    MAX_HISTORY_REFERENCES, MAX_PROVIDER_PAYLOAD_BYTES,
};
pub use manager::DefaultChatTurnManager;
pub use provider::MonocleLlmProvider;
pub use storage::{DbAgentStorage, FileAgentStorage};
pub use tool_workflow::{
    CallKey, ModelStep, ToolCall, ToolExecutor, ToolResult, ToolStatus, ToolWorkflowAgent,
    ToolWorkflowManager, ToolWorkflowProvider, WorkflowExchange, WorkflowFailure, WorkflowLimits,
    WorkflowRequest,
};
pub use types::{
    AgentError, AgentOutcome, AgentPhase, AgentStorage, Cancellation, ChatTurnManager, LlmProvider,
    LlmRequest, LlmResponse, StorageReceipt, ToolDefinition, ToolRegistry, TurnInput, TurnRecord,
};

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tool_workflow_tests;
