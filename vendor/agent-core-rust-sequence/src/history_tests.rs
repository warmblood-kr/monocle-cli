use super::*;
use crate::{
    AgentError, Cancellation, LlmRequest, LlmResponse, ToolDefinition, TurnInput, TurnRecord,
};
use serde_json::json;
use std::cell::Cell;

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct Counts {
    auth: Cell<usize>,
    loads: Cell<usize>,
    appends: Cell<usize>,
    prepares: Cell<usize>,
    finishes: Cell<usize>,
    inferences: Cell<usize>,
    source_auth: Cell<usize>,
}

struct Auth {
    counts: std::rc::Rc<Counts>,
    allow: bool,
    unavailable: bool,
    allow_source: bool,
}
impl HistoryAuthority for Auth {
    fn authorize(&self, _: &ArchiveScope, _: &TurnProvenance) -> Result<(), ArchiveError> {
        self.counts.auth.set(self.counts.auth.get() + 1);
        if self.unavailable {
            Err(ArchiveError::Unavailable)
        } else if self.allow {
            Ok(())
        } else {
            Err(ArchiveError::Denied)
        }
    }

    fn authorize_source_disclosure(
        &self,
        _: &ArchivedTurn,
        _: &ArchiveScope,
        _: &TurnProvenance,
    ) -> Result<(), ArchiveError> {
        self.counts
            .source_auth
            .set(self.counts.source_auth.get() + 1);
        if self.allow_source {
            Ok(())
        } else {
            Err(ArchiveError::Denied)
        }
    }
}

struct CountingArchive {
    inner: InMemoryHistoryArchive,
    counts: std::rc::Rc<Counts>,
    append_error: Option<ArchiveError>,
    cancel_on_load: Option<std::rc::Rc<Cell<bool>>>,
}
impl HistoryArchive for CountingArchive {
    fn append_completed(&mut self, record: &ArchivedTurn) -> Result<ArchiveReceipt, ArchiveError> {
        self.counts.appends.set(self.counts.appends.get() + 1);
        if let Some(error) = self.append_error.clone() {
            return Err(error);
        }
        self.inner.append_completed(record)
    }
    fn load_reference(
        &mut self,
        reference: &ArchiveReference,
    ) -> Result<ArchivedTurn, ArchiveError> {
        self.counts.loads.set(self.counts.loads.get() + 1);
        if let Some(cancelled) = &self.cancel_on_load {
            cancelled.set(true);
        }
        self.inner.load_reference(reference)
    }
}

struct Manager {
    counts: std::rc::Rc<Counts>,
    seen: Vec<Vec<ArchivedTurn>>,
    fail_prepare: bool,
    fail_finish: bool,
    mismatch_finish: bool,
    mismatch_input: bool,
}
impl HistoryTurnManager for Manager {
    fn prepare_with_history(
        &mut self,
        input: TurnInput,
        prompt: &str,
        history: &[ArchivedTurn],
    ) -> Result<LlmRequest, AgentError> {
        self.counts.prepares.set(self.counts.prepares.get() + 1);
        self.seen.push(history.to_vec());
        if self.fail_prepare {
            return Err(AgentError {
                phase: crate::AgentPhase::Manager,
                message: "prepare failed".into(),
            });
        }
        Ok(LlmRequest {
            turn_id: input.turn_id,
            system_prompt: prompt.to_owned(),
            input: if self.mismatch_input {
                json!({"forged":true})
            } else {
                input.input
            },
            tools: Vec::<ToolDefinition>::new(),
        })
    }
    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError> {
        self.counts.finishes.set(self.counts.finishes.get() + 1);
        if self.fail_finish {
            return Err(AgentError {
                phase: crate::AgentPhase::Manager,
                message: "finish failed".into(),
            });
        }
        Ok(TurnRecord {
            turn_id: if self.mismatch_finish {
                "forged".into()
            } else {
                turn_id.to_owned()
            },
            response: if self.mismatch_finish {
                json!({"forged":true})
            } else {
                response.payload
            },
        })
    }
}

struct Provider {
    counts: std::rc::Rc<Counts>,
    seen: Vec<HistoryRequest>,
    payloads: Vec<Vec<u8>>,
    fail: bool,
}
impl HistoryProvider for Provider {
    fn infer_history(
        &mut self,
        request: HistoryRequest,
        serialized_payload: Vec<u8>,
        _: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError> {
        self.counts.inferences.set(self.counts.inferences.get() + 1);
        self.seen.push(request);
        self.payloads.push(serialized_payload);
        if self.fail {
            return Err(AgentError {
                phase: crate::AgentPhase::Provider,
                message: "provider failed".into(),
            });
        }
        Ok(LlmResponse {
            payload: json!({"answer":"a2"}),
        })
    }
}

fn scope(principal: &str) -> ArchiveScope {
    ArchiveScope {
        tenant_id: "tenant-a".into(),
        principal_id: principal.into(),
        conversation_id: "chat-1".into(),
    }
}
fn provenance(model: &str) -> TurnProvenance {
    TurnProvenance {
        agent_id: "agent-1".into(),
        provider_id: "provider-1".into(),
        model_id: model.into(),
    }
}
fn entry(id: &str, caller: &ArchiveScope, source_turn: &str) -> ArchivedTurn {
    ArchivedTurn {
        reference: ArchiveReference {
            scope: caller.clone(),
            record_id: id.into(),
            source_turn_id: source_turn.into(),
        },
        provenance: provenance("model-1"),
        request: LlmRequest {
            turn_id: source_turn.into(),
            system_prompt: "system".into(),
            input: json!({"task":"plan"}),
            tools: vec![],
        },
        response: LlmResponse {
            payload: json!({"plan":["one","two"]}),
        },
        source_references: vec![],
    }
}
fn actor(
    archive: InMemoryHistoryArchive,
    caller: ArchiveScope,
    counts: std::rc::Rc<Counts>,
    allow: bool,
    record_id: &str,
) -> ArchivedAgent<Provider, Manager, CountingArchive, Auth> {
    ArchivedAgent::new(
        Provider {
            counts: counts.clone(),
            seen: vec![],
            payloads: vec![],
            fail: false,
        },
        Manager {
            counts: counts.clone(),
            seen: vec![],
            fail_prepare: false,
            fail_finish: false,
            mismatch_finish: false,
            mismatch_input: false,
        },
        CountingArchive {
            inner: archive,
            counts: counts.clone(),
            append_error: None,
            cancel_on_load: None,
        },
        Auth {
            counts,
            allow,
            unavailable: false,
            allow_source: true,
        },
        "system".into(),
        caller,
        provenance("model-1"),
        record_id.into(),
    )
    .unwrap()
}

#[test]
fn two_turn_actor_loads_snapshot_and_appends_after_success_once() {
    let caller = scope("alice");
    let original = entry("r1", &caller, "a1");
    let mut archive = InMemoryHistoryArchive::default();
    let expected = archive.append_completed(&original).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(archive, caller, counts.clone(), true, "r2");
    let outcome = sut
        .run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!({"critique":true}),
            },
            vec![expected.reference.clone()],
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(sut.manager.seen[0], vec![original.clone()]);
    assert_eq!(sut.provider.seen[0].history, vec![original]);
    assert_eq!(sut.provider.seen[0].request.turn_id, "a2");
    assert_eq!(
        sut.provider.payloads[0],
        sut.provider.seen[0].to_serialized_json().unwrap()
    );
    assert_eq!(outcome.response, json!({"answer":"a2"}));
    assert_eq!(outcome.receipt.reference.record_id, "r2");
    assert_eq!(counts.loads.get(), 1);
    assert_eq!(counts.prepares.get(), 1);
    assert_eq!(counts.inferences.get(), 1);
    assert_eq!(counts.finishes.get(), 1);
    assert_eq!(counts.appends.get(), 1);
    let saved = sut
        .archive
        .inner
        .load_reference(&outcome.receipt.reference)
        .unwrap();
    assert_eq!(saved.request.turn_id, "a2");
    assert_eq!(saved.request.input, json!({"critique":true}));
    assert_eq!(saved.response.payload, json!({"answer":"a2"}));
    assert_eq!(saved.source_references, vec![expected.reference]);
}

#[test]
fn denied_and_missing_are_distinct_and_stop_before_manager_provider_append() {
    let counts = std::rc::Rc::new(Counts::default());
    let bob = scope("bob");
    let alice_ref = entry("r1", &scope("alice"), "a1").reference;
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        bob.clone(),
        counts.clone(),
        false,
        "r2",
    );
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![alice_ref],
            &NeverCancel
        ),
        Err(HistoryError::Archive(ArchiveError::Denied))
    );
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![entry("r9", &bob, "a9").reference],
            &NeverCancel,
        ),
        Err(HistoryError::Archive(ArchiveError::Denied))
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (0, 0, 0, 0)
    );

    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    let missing = entry("missing", &scope("alice"), "old").reference;
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![missing],
            &NeverCancel
        ),
        Err(HistoryError::Archive(ArchiveError::Missing))
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0, 0)
    );
}

#[test]
fn current_authority_denial_stops_reference_free_inference() {
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        false,
        "r2",
    );
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Archive(ArchiveError::Denied))
    );
    assert_eq!(
        (
            counts.auth.get(),
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0, 0, 0)
    );
}

#[test]
fn snapshots_are_owned_and_record_collision_does_not_overwrite() {
    let caller = scope("alice");
    let mut original = entry("r1", &caller, "a1");
    let expected = original.clone();
    let mut archive = InMemoryHistoryArchive::default();
    let receipt = archive.append_completed(&original).unwrap();
    original.response.payload["plan"][0] = json!("caller mutation");
    assert_eq!(
        archive
            .load_reference(&receipt.reference)
            .unwrap()
            .response
            .payload,
        json!({"plan":["one","two"]})
    );
    let mut loaded = archive.load_reference(&receipt.reference).unwrap();
    loaded.response.payload["plan"][0] = json!("mutated");
    assert_eq!(
        archive.load_reference(&receipt.reference).unwrap(),
        expected
    );
    assert_eq!(
        archive
            .append_completed(&entry("r1", &caller, "different"))
            .unwrap_err(),
        ArchiveError::Conflict
    );
    assert_eq!(
        archive.load_reference(&receipt.reference).unwrap(),
        expected
    );
}

#[test]
fn actor_record_id_collision_is_conflict_and_preserves_existing_record() {
    let caller = scope("alice");
    let original = entry("r2", &caller, "old-turn");
    let mut archive = InMemoryHistoryArchive::default();
    let prior_receipt = archive.append_completed(&original).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(archive, caller, counts.clone(), true, "r2");
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Archive(ArchiveError::Conflict))
    );
    assert_eq!(
        (
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 1)
    );
    assert_eq!(
        sut.archive
            .inner
            .load_reference(&prior_receipt.reference)
            .unwrap(),
        original
    );
}

#[test]
fn malformed_post_append_receipt_is_outcome_unknown_without_retry() {
    let caller = scope("alice");
    let mut archive = InMemoryHistoryArchive::default();
    archive.set_receipt_override(Some(ArchiveReference {
        scope: caller.clone(),
        record_id: "wrong".into(),
        source_turn_id: "a2".into(),
    }));
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(archive, caller, counts.clone(), true, "r2");
    let result = sut.run_archived_turn(
        TurnInput {
            turn_id: "a2".into(),
            input: json!(null),
        },
        vec![],
        &NeverCancel,
    );
    assert_eq!(
        result,
        Err(HistoryError::Archive(ArchiveError::OutcomeUnknown))
    );
    assert_eq!(
        (
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 1)
    );
    let saved = sut
        .archive
        .inner
        .load_reference(&ArchiveReference {
            scope: scope("alice"),
            record_id: "r2".into(),
            source_turn_id: "a2".into(),
        })
        .unwrap();
    assert_eq!(saved.request.turn_id, "a2");
}

#[test]
fn manager_and_provider_errors_are_single_attempts_without_archive_append() {
    let caller = scope("alice");
    let counts = std::rc::Rc::new(Counts::default());
    let mut prepare_error = actor(
        InMemoryHistoryArchive::default(),
        caller.clone(),
        counts.clone(),
        true,
        "r2",
    );
    prepare_error.manager.fail_prepare = true;
    assert!(matches!(
        prepare_error.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Manager(_))
    ));
    assert_eq!(
        (
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0)
    );

    let counts = std::rc::Rc::new(Counts::default());
    let mut provider_error = actor(
        InMemoryHistoryArchive::default(),
        caller.clone(),
        counts.clone(),
        true,
        "r2",
    );
    provider_error.provider.fail = true;
    assert!(matches!(
        provider_error.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Provider(_))
    ));
    assert_eq!(
        (
            counts.prepares.get(),
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 0, 0)
    );

    let counts = std::rc::Rc::new(Counts::default());
    let mut finish_error = actor(
        InMemoryHistoryArchive::default(),
        caller,
        counts.clone(),
        true,
        "r2",
    );
    finish_error.manager.fail_finish = true;
    assert!(matches!(
        finish_error.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Manager(_))
    ));
    assert_eq!(
        (
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 0)
    );
}

#[test]
fn malformed_manager_response_correlation_fails_before_archive_append() {
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    sut.manager.mismatch_finish = true;
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![],
            &NeverCancel
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        (
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 0)
    );
}

struct CancelFlag(std::rc::Rc<Cell<bool>>);
impl Cancellation for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

#[test]
fn cancellation_after_reference_load_stops_before_manager_and_provider() {
    let caller = scope("alice");
    let record = entry("r1", &caller, "a1");
    let mut inner = InMemoryHistoryArchive::default();
    let receipt = inner.append_completed(&record).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(inner, caller, counts.clone(), true, "r2");
    let cancelled = std::rc::Rc::new(Cell::new(false));
    sut.archive.cancel_on_load = Some(cancelled.clone());
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![receipt.reference],
            &CancelFlag(cancelled),
        ),
        Err(HistoryError::Cancelled)
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0, 0)
    );
}

#[test]
fn source_record_disclosure_is_authorized_after_load_before_manager_and_provider() {
    let caller = scope("alice");
    let record = entry("r1", &caller, "a1");
    let mut inner = InMemoryHistoryArchive::default();
    let receipt = inner.append_completed(&record).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(inner, caller, counts.clone(), true, "r2");
    sut.authority.allow_source = false;
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![receipt.reference],
            &NeverCancel,
        ),
        Err(HistoryError::Archive(ArchiveError::Denied))
    );
    assert_eq!(
        (
            counts.auth.get(),
            counts.loads.get(),
            counts.source_auth.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 1, 1, 0, 0, 0)
    );
}

#[test]
fn serialized_history_byte_limit_is_inclusive_then_rejects_next_byte() {
    let caller = scope("alice");
    let mut record = entry("r1", &caller, "a1");
    record.response.payload = json!("");
    let overhead = serialized_history_bytes(&[record.clone()]).unwrap();
    record.response.payload = json!("x".repeat(MAX_HISTORY_BYTES - overhead));
    assert_eq!(
        serialized_history_bytes(&[record.clone()]).unwrap(),
        MAX_HISTORY_BYTES
    );
    let mut inner = InMemoryHistoryArchive::default();
    let receipt = inner.append_completed(&record).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(inner, caller.clone(), counts.clone(), true, "r2");
    assert!(sut
        .run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![receipt.reference],
            &NeverCancel,
        )
        .is_ok());
    assert_eq!(
        serialized_history_bytes(&sut.provider.seen[0].history).unwrap(),
        MAX_HISTORY_BYTES
    );

    let mut oversized = entry("r1-large", &caller, "a1-large");
    oversized.response.payload = json!("");
    let oversized_overhead = serialized_history_bytes(&[oversized.clone()]).unwrap();
    oversized.response.payload = json!("x".repeat(MAX_HISTORY_BYTES - oversized_overhead + 1));
    assert_eq!(
        serialized_history_bytes(&[oversized.clone()]).unwrap(),
        MAX_HISTORY_BYTES + 1
    );
    let mut inner = InMemoryHistoryArchive::default();
    let receipt = inner.append_completed(&oversized).unwrap();
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(inner, caller, counts.clone(), true, "r2");
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(null)
            },
            vec![receipt.reference],
            &NeverCancel,
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.source_auth.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 1, 0, 0, 0)
    );
}

#[test]
fn manager_must_preserve_exact_current_input_before_provider_disclosure() {
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    sut.manager.mismatch_input = true;
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!({"secret":"exact"})
            },
            vec![],
            &NeverCancel,
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        (
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0)
    );
}

#[test]
fn oversized_current_request_is_rejected_before_provider_and_append() {
    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!("x".repeat(131_073))
            },
            vec![],
            &NeverCancel,
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        (
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0)
    );
}

#[test]
fn complete_provider_json_payload_accepts_exact_limit_and_rejects_next_byte() {
    let empty = HistoryRequest {
        request: LlmRequest {
            turn_id: "a2".into(),
            system_prompt: "system".into(),
            input: json!(""),
            tools: vec![],
        },
        history: vec![],
    };
    let overhead = empty.to_serialized_json().unwrap().len();
    let exact_input = "x".repeat(MAX_PROVIDER_PAYLOAD_BYTES - overhead);
    let exact = HistoryRequest {
        request: LlmRequest {
            input: json!(exact_input.clone()),
            ..empty.request.clone()
        },
        history: vec![],
    };
    assert_eq!(
        exact.to_serialized_json().unwrap().len(),
        MAX_PROVIDER_PAYLOAD_BYTES
    );

    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    let outcome = sut
        .run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!(exact_input.clone()),
            },
            vec![],
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(outcome.response, json!({"answer":"a2"}));
    assert_eq!(sut.provider.payloads[0].len(), MAX_PROVIDER_PAYLOAD_BYTES);
    assert_eq!(
        sut.provider.payloads[0],
        sut.provider.seen[0].to_serialized_json().unwrap()
    );
    assert_eq!(sut.provider.seen[0].request.input, json!(exact_input));

    let counts = std::rc::Rc::new(Counts::default());
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        scope("alice"),
        counts.clone(),
        true,
        "r2",
    );
    assert_eq!(
        sut.run_archived_turn(
            TurnInput {
                turn_id: "a2".into(),
                input: json!("x".repeat(MAX_PROVIDER_PAYLOAD_BYTES - overhead + 1))
            },
            vec![],
            &NeverCancel,
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        (
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (1, 0, 0)
    );
}

#[test]
fn invalid_reference_lists_stop_before_archive_lookup() {
    let caller = scope("alice");
    let counts = std::rc::Rc::new(Counts::default());
    let ref1 = entry("r1", &caller, "a1").reference;
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        caller,
        counts.clone(),
        true,
        "r2",
    );
    let input = TurnInput {
        turn_id: "a2".into(),
        input: json!(null),
    };
    assert_eq!(
        sut.run_archived_turn(
            input.clone(),
            vec![ref1.clone(), ref1.clone()],
            &NeverCancel
        ),
        Err(HistoryError::Validation)
    );
    assert_eq!(
        sut.run_archived_turn(
            input,
            vec![entry("r1", &scope("other"), "a1").reference],
            &NeverCancel
        ),
        Err(HistoryError::Archive(ArchiveError::Denied))
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (0, 0, 0, 0)
    );
}

struct CancelNow;
impl Cancellation for CancelNow {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn unavailable_authority_and_precancelled_turn_have_no_archive_or_model_effects() {
    let counts = std::rc::Rc::new(Counts::default());
    let caller = scope("alice");
    let reference = entry("missing", &caller, "a1").reference;
    let mut sut = ArchivedAgent::new(
        Provider {
            counts: counts.clone(),
            seen: vec![],
            payloads: vec![],
            fail: false,
        },
        Manager {
            counts: counts.clone(),
            seen: vec![],
            fail_prepare: false,
            fail_finish: false,
            mismatch_finish: false,
            mismatch_input: false,
        },
        CountingArchive {
            inner: InMemoryHistoryArchive::default(),
            counts: counts.clone(),
            append_error: None,
            cancel_on_load: None,
        },
        Auth {
            counts: counts.clone(),
            allow: false,
            unavailable: true,
            allow_source: false,
        },
        "system".into(),
        caller.clone(),
        provenance("model-1"),
        "r2".into(),
    )
    .unwrap();
    let input = TurnInput {
        turn_id: "a2".into(),
        input: json!(null),
    };
    assert_eq!(
        sut.run_archived_turn(input.clone(), vec![reference], &NeverCancel),
        Err(HistoryError::Archive(ArchiveError::Unavailable))
    );
    assert_eq!(
        sut.run_archived_turn(input, vec![], &CancelNow),
        Err(HistoryError::Cancelled)
    );
    assert_eq!(
        (
            counts.loads.get(),
            counts.prepares.get(),
            counts.inferences.get(),
            counts.appends.get()
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn unknown_append_error_is_preserved_and_never_retried() {
    let counts = std::rc::Rc::new(Counts::default());
    let caller = scope("alice");
    let mut sut = actor(
        InMemoryHistoryArchive::default(),
        caller,
        counts.clone(),
        true,
        "r2",
    );
    sut.archive.append_error = Some(ArchiveError::OutcomeUnknown);
    let result = sut.run_archived_turn(
        TurnInput {
            turn_id: "a2".into(),
            input: json!(null),
        },
        vec![],
        &NeverCancel,
    );
    assert_eq!(
        result,
        Err(HistoryError::Archive(ArchiveError::OutcomeUnknown))
    );
    assert_eq!(
        (
            counts.inferences.get(),
            counts.finishes.get(),
            counts.appends.get()
        ),
        (1, 1, 1)
    );
}
