use crate::{AgentError, Cancellation, LlmRequest, LlmResponse, TurnInput, TurnRecord};

/// The B/C reference lane is deliberately capped at one selected turn.
pub const MAX_HISTORY_REFERENCES: usize = 1;
/// Maximum size of the serialized selected history array, including JSON
/// framing and escaped content.
pub const MAX_HISTORY_BYTES: usize = 65_536;
/// Maximum size of the complete canonical JSON payload sent to the provider,
/// including current request, tools, selected history and envelope framing.
pub const MAX_PROVIDER_PAYLOAD_BYTES: usize = 131_072;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveScope {
    pub tenant_id: String,
    pub principal_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnProvenance {
    pub agent_id: String,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveReference {
    pub scope: ArchiveScope,
    pub record_id: String,
    pub source_turn_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArchivedTurn {
    pub reference: ArchiveReference,
    pub provenance: TurnProvenance,
    pub request: LlmRequest,
    pub response: LlmResponse,
    pub source_references: Vec<ArchiveReference>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRequest {
    pub request: LlmRequest,
    pub history: Vec<ArchivedTurn>,
}

impl HistoryRequest {
    /// Encode the exact provider payload used by the actor's total-byte check.
    /// The envelope is JSON `{ "request": {..}, "history": [..] }` with no
    /// implementation-defined history concatenation convention.
    pub fn to_serialized_json(&self) -> Result<Vec<u8>, HistoryError> {
        let payload = serde_json::json!({
            "request": llm_request_value(&self.request),
            "history": archived_turn_values(&self.history),
        });
        serde_json::to_vec(&payload).map_err(|_| HistoryError::Validation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReceipt {
    pub reference: ArchiveReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    Denied,
    Missing,
    Conflict,
    Unavailable,
    OutcomeUnknown,
    InvalidRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    Validation,
    Cancelled,
    Manager(AgentError),
    Provider(AgentError),
    Archive(ArchiveError),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryOutcome {
    pub response: serde_json::Value,
    pub receipt: ArchiveReceipt,
}

pub trait HistoryAuthority {
    /// Consult current host authority on every reference before archive lookup.
    fn authorize(
        &self,
        scope: &ArchiveScope,
        provenance: &TurnProvenance,
    ) -> Result<(), ArchiveError>;

    /// Decide whether this exact loaded source record may be disclosed to the
    /// configured destination scope and model. This callback is required so
    /// the host owns source-record disclosure policy.
    fn authorize_source_disclosure(
        &self,
        record: &ArchivedTurn,
        destination_scope: &ArchiveScope,
        destination: &TurnProvenance,
    ) -> Result<(), ArchiveError>;
}

pub trait HistoryArchive {
    fn append_completed(&mut self, record: &ArchivedTurn) -> Result<ArchiveReceipt, ArchiveError>;
    fn load_reference(
        &mut self,
        reference: &ArchiveReference,
    ) -> Result<ArchivedTurn, ArchiveError>;
}

pub trait HistoryTurnManager {
    fn prepare_with_history(
        &mut self,
        input: TurnInput,
        system_prompt: &str,
        history: &[ArchivedTurn],
    ) -> Result<LlmRequest, AgentError>;
    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError>;
}

pub trait HistoryProvider {
    /// Receive typed context plus its canonical, size-checked JSON bytes. A
    /// host adapter should send `serialized_payload` unchanged so the enforced
    /// limit applies to the exact provider request body.
    fn infer_history(
        &mut self,
        request: HistoryRequest,
        serialized_payload: Vec<u8>,
        cancel: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError>;
}

/// Simple in-process immutable archive. It owns inserted and returned clones;
/// process lifetime is the only lifetime guarantee.
#[derive(Debug, Default)]
pub struct InMemoryHistoryArchive {
    records: Vec<ArchivedTurn>,
    #[cfg(test)]
    receipt_override: Option<ArchiveReference>,
}

impl InMemoryHistoryArchive {
    #[cfg(test)]
    pub fn set_receipt_override(&mut self, reference: Option<ArchiveReference>) {
        self.receipt_override = reference;
    }
}

impl HistoryArchive for InMemoryHistoryArchive {
    fn append_completed(&mut self, record: &ArchivedTurn) -> Result<ArchiveReceipt, ArchiveError> {
        if self
            .records
            .iter()
            .any(|item| item.reference.record_id == record.reference.record_id)
        {
            return Err(ArchiveError::Conflict);
        }
        if record.reference.record_id.trim().is_empty()
            || record.reference.source_turn_id.trim().is_empty()
            || record.request.turn_id != record.reference.source_turn_id
            || record.reference.scope.tenant_id.trim().is_empty()
            || record.reference.scope.principal_id.trim().is_empty()
            || record.reference.scope.conversation_id.trim().is_empty()
            || record.provenance.agent_id.trim().is_empty()
            || record.provenance.provider_id.trim().is_empty()
            || record.provenance.model_id.trim().is_empty()
        {
            return Err(ArchiveError::InvalidRecord);
        }
        self.records.push(record.clone());
        #[cfg(test)]
        let receipt_reference = self
            .receipt_override
            .clone()
            .unwrap_or_else(|| record.reference.clone());
        #[cfg(not(test))]
        let receipt_reference = record.reference.clone();
        Ok(ArchiveReceipt {
            reference: receipt_reference,
        })
    }
    fn load_reference(
        &mut self,
        reference: &ArchiveReference,
    ) -> Result<ArchivedTurn, ArchiveError> {
        self.records
            .iter()
            .find(|item| item.reference == *reference)
            .cloned()
            .ok_or(ArchiveError::Missing)
    }
}

/// Model-only actor for a later turn with explicitly selected archived snapshots.
pub struct ArchivedAgent<
    P: HistoryProvider,
    M: HistoryTurnManager,
    A: HistoryArchive,
    H: HistoryAuthority,
> {
    provider: P,
    manager: M,
    archive: A,
    authority: H,
    system_prompt: String,
    scope: ArchiveScope,
    provenance: TurnProvenance,
    record_id: String,
}

impl<P: HistoryProvider, M: HistoryTurnManager, A: HistoryArchive, H: HistoryAuthority>
    ArchivedAgent<P, M, A, H>
{
    pub fn new(
        provider: P,
        manager: M,
        archive: A,
        authority: H,
        system_prompt: String,
        scope: ArchiveScope,
        provenance: TurnProvenance,
        record_id: String,
    ) -> Result<Self, HistoryError> {
        if system_prompt.trim().is_empty()
            || record_id.trim().is_empty()
            || scope.tenant_id.trim().is_empty()
            || scope.principal_id.trim().is_empty()
            || scope.conversation_id.trim().is_empty()
            || provenance.agent_id.trim().is_empty()
            || provenance.provider_id.trim().is_empty()
            || provenance.model_id.trim().is_empty()
        {
            return Err(HistoryError::Validation);
        }
        Ok(Self {
            provider,
            manager,
            archive,
            authority,
            system_prompt,
            scope,
            provenance,
            record_id,
        })
    }

    pub fn run_archived_turn(
        &mut self,
        input: TurnInput,
        references: Vec<ArchiveReference>,
        cancel: &dyn Cancellation,
    ) -> Result<HistoryOutcome, HistoryError> {
        if input.turn_id.trim().is_empty()
            || references.len() > MAX_HISTORY_REFERENCES
            || references.iter().any(|reference| {
                references
                    .iter()
                    .filter(|other| *other == reference)
                    .count()
                    > 1
            })
        {
            return Err(HistoryError::Validation);
        }
        // Reject foreign locators before either authority or archive lookup.
        if references
            .iter()
            .any(|reference| reference.scope != self.scope)
        {
            return Err(HistoryError::Archive(ArchiveError::Denied));
        }
        check_cancelled(cancel)?;
        // Re-check the current caller/model authority for each run, including
        // turns with no selected history reference.
        self.authority
            .authorize(&self.scope, &self.provenance)
            .map_err(HistoryError::Archive)?;
        let mut snapshots = Vec::with_capacity(references.len());
        for reference in &references {
            let record = self
                .archive
                .load_reference(reference)
                .map_err(HistoryError::Archive)?;
            if record.reference != *reference
                || record.reference.scope != self.scope
                || record.reference.source_turn_id != record.request.turn_id
                || record.provenance.agent_id.trim().is_empty()
                || record.provenance.provider_id.trim().is_empty()
                || record.provenance.model_id.trim().is_empty()
            {
                return Err(HistoryError::Archive(ArchiveError::InvalidRecord));
            }
            self.authority
                .authorize_source_disclosure(&record, &self.scope, &self.provenance)
                .map_err(HistoryError::Archive)?;
            snapshots.push(record);
        }
        if serialized_history_bytes(&snapshots)? > MAX_HISTORY_BYTES {
            return Err(HistoryError::Validation);
        }
        check_cancelled(cancel)?;
        let expected_turn_id = input.turn_id.clone();
        let expected_input = input.input.clone();
        let request = self
            .manager
            .prepare_with_history(input, &self.system_prompt, &snapshots)
            .map_err(HistoryError::Manager)?;
        if request.turn_id != expected_turn_id
            || request.system_prompt != self.system_prompt
            || request.input != expected_input
        {
            return Err(HistoryError::Validation);
        }
        check_cancelled(cancel)?;
        let turn_id = request.turn_id.clone();
        let provider_request = HistoryRequest {
            request: request.clone(),
            history: snapshots,
        };
        let serialized_payload = provider_request.to_serialized_json()?;
        if serialized_payload.len() > MAX_PROVIDER_PAYLOAD_BYTES {
            return Err(HistoryError::Validation);
        }
        let response = self
            .provider
            .infer_history(provider_request, serialized_payload, cancel)
            .map_err(HistoryError::Provider)?;
        check_cancelled(cancel)?;
        let expected = response.payload.clone();
        let archived_response = response.clone();
        let record = self
            .manager
            .finish(&turn_id, response)
            .map_err(HistoryError::Manager)?;
        if record.turn_id != turn_id || record.response != expected {
            return Err(HistoryError::Validation);
        }
        check_cancelled(cancel)?;
        let entry = ArchivedTurn {
            reference: ArchiveReference {
                scope: self.scope.clone(),
                record_id: self.record_id.clone(),
                source_turn_id: turn_id,
            },
            provenance: self.provenance.clone(),
            request,
            response: archived_response,
            source_references: references,
        };
        let receipt = self
            .archive
            .append_completed(&entry)
            .map_err(HistoryError::Archive)?;
        if receipt.reference != entry.reference {
            return Err(HistoryError::Archive(ArchiveError::OutcomeUnknown));
        }
        Ok(HistoryOutcome {
            response: record.response,
            receipt,
        })
    }
}

fn serialized_history_bytes(history: &[ArchivedTurn]) -> Result<usize, HistoryError> {
    serde_json::to_vec(&archived_turn_values(history))
        .map(|bytes| bytes.len())
        .map_err(|_| HistoryError::Validation)
}

fn archived_turn_values(history: &[ArchivedTurn]) -> Vec<serde_json::Value> {
    history.iter().map(archived_turn_value).collect()
}

fn archived_turn_value(record: &ArchivedTurn) -> serde_json::Value {
    serde_json::json!({
        "reference": {
            "scope": archive_scope_value(&record.reference.scope),
            "record_id": record.reference.record_id,
            "source_turn_id": record.reference.source_turn_id,
        },
        "provenance": {
            "agent_id": record.provenance.agent_id,
            "provider_id": record.provenance.provider_id,
            "model_id": record.provenance.model_id,
        },
        "request": llm_request_value(&record.request),
        "response": { "payload": record.response.payload },
        "source_references": record.source_references.iter().map(|reference| serde_json::json!({
            "scope": archive_scope_value(&reference.scope),
            "record_id": reference.record_id,
            "source_turn_id": reference.source_turn_id,
        })).collect::<Vec<_>>(),
    })
}

fn llm_request_value(request: &LlmRequest) -> serde_json::Value {
    serde_json::json!({
        "turn_id": request.turn_id,
        "system_prompt": request.system_prompt,
        "input": request.input,
        "tools": request.tools.iter().map(|tool| serde_json::json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.input_schema,
        })).collect::<Vec<_>>(),
    })
}

fn archive_scope_value(scope: &ArchiveScope) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": scope.tenant_id,
        "principal_id": scope.principal_id,
        "conversation_id": scope.conversation_id,
    })
}

fn check_cancelled(cancel: &dyn Cancellation) -> Result<(), HistoryError> {
    if cancel.is_cancelled() {
        Err(HistoryError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod bounded_context_tests {
    use super::MAX_HISTORY_REFERENCES;
    #[test]
    fn selected_context_bound_is_exactly_one_turn() {
        assert_eq!(MAX_HISTORY_REFERENCES, 1);
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
