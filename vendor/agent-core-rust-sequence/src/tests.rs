use super::*;
use serde_json::json;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn native_input() -> Value {
    json!({
        "messages": [{"role": "user", "content": "hi"}],
        "unknown": {"nested": [1, {"preserve": true}]},
        "tool_calls": [{"id": "opaque", "function": {"name": "not-run"}}],
        "usage": {"input_tokens": 17}
    })
}

fn native_response() -> Value {
    json!({
        "text": "ok",
        "usage": {"input_tokens": 17, "output_tokens": 3},
        "tool_calls": [{"id": "opaque-response", "arguments": {"x": 1}}],
        "native": {"trace": ["a", "b"]}
    })
}

#[derive(Default)]
struct Counts {
    prepare: Cell<usize>,
    infer: Cell<usize>,
    finish: Cell<usize>,
    append: Cell<usize>,
}

struct Flag(bool);
impl Cancellation for Flag {
    fn is_cancelled(&self) -> bool {
        self.0
    }
}

struct CancelAt {
    checks: Cell<usize>,
    cancel_at: usize,
}
impl Cancellation for CancelAt {
    fn is_cancelled(&self) -> bool {
        let next = self.checks.get() + 1;
        self.checks.set(next);
        next >= self.cancel_at
    }
}

struct ScriptManager {
    counts: Rc<Counts>,
    prepare_error: Option<AgentError>,
    finish_error: Option<AgentError>,
    bad_request: bool,
    bad_record: bool,
}
impl ChatTurnManager for ScriptManager {
    fn prepare(&mut self, input: TurnInput, system_prompt: &str) -> Result<LlmRequest, AgentError> {
        self.counts.prepare.set(self.counts.prepare.get() + 1);
        if let Some(err) = self.prepare_error.take() {
            return Err(err);
        }
        Ok(LlmRequest {
            turn_id: if self.bad_request {
                "other".into()
            } else {
                input.turn_id
            },
            system_prompt: if self.bad_request {
                "other"
            } else {
                system_prompt
            }
            .into(),
            input: input.input,
            tools: vec![],
        })
    }

    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError> {
        self.counts.finish.set(self.counts.finish.get() + 1);
        if let Some(err) = self.finish_error.take() {
            return Err(err);
        }
        Ok(TurnRecord {
            turn_id: if self.bad_record { "other" } else { turn_id }.into(),
            response: if self.bad_record {
                json!({"changed": true})
            } else {
                response.payload
            },
        })
    }
}

struct ScriptProvider {
    counts: Rc<Counts>,
    error: Option<AgentError>,
}
impl LlmProvider for ScriptProvider {
    fn infer(
        &mut self,
        _request: LlmRequest,
        _cancel: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError> {
        self.counts.infer.set(self.counts.infer.get() + 1);
        if let Some(err) = self.error.take() {
            return Err(err);
        }
        Ok(LlmResponse {
            payload: native_response(),
        })
    }
}

struct ScriptStorage {
    counts: Rc<Counts>,
    error: Option<AgentError>,
}
impl AgentStorage for ScriptStorage {
    fn append(&mut self, _record: &TurnRecord) -> Result<StorageReceipt, AgentError> {
        self.counts.append.set(self.counts.append.get() + 1);
        if let Some(err) = self.error.take() {
            return Err(err);
        }
        Ok(StorageReceipt {
            reference: "test-receipt".into(),
        })
    }
}

fn scripted_agent(
    counts: Rc<Counts>,
    prepare_error: Option<AgentError>,
    provider_error: Option<AgentError>,
    finish_error: Option<AgentError>,
    storage_error: Option<AgentError>,
    bad_request: bool,
    bad_record: bool,
) -> Agent<ScriptProvider, ScriptManager, ScriptStorage> {
    Agent::new(
        ScriptProvider {
            counts: counts.clone(),
            error: provider_error,
        },
        ScriptManager {
            counts: counts.clone(),
            prepare_error,
            finish_error,
            bad_request,
            bad_record,
        },
        ScriptStorage {
            counts,
            error: storage_error,
        },
        "  keep prompt bytes  ".into(),
    )
    .unwrap()
}

fn input() -> TurnInput {
    TurnInput {
        turn_id: " turn-1 ".into(),
        input: native_input(),
    }
}

fn component_error(phase: AgentPhase, message: &str) -> AgentError {
    AgentError {
        phase,
        message: message.into(),
    }
}

struct TestRegistry {
    calls: Rc<Cell<usize>>,
    result: Result<Vec<ToolDefinition>, AgentError>,
}
impl ToolRegistry for TestRegistry {
    fn definitions(&self) -> Result<Vec<ToolDefinition>, AgentError> {
        self.calls.set(self.calls.get() + 1);
        self.result.clone()
    }
}

struct DisclosureManager {
    tools: Vec<ToolDefinition>,
}
impl ChatTurnManager for DisclosureManager {
    fn prepare(&mut self, input: TurnInput, prompt: &str) -> Result<LlmRequest, AgentError> {
        Ok(LlmRequest {
            turn_id: input.turn_id,
            system_prompt: prompt.to_owned(),
            input: input.input,
            tools: self.tools.clone(),
        })
    }
    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError> {
        Ok(TurnRecord {
            turn_id: turn_id.into(),
            response: response.payload,
        })
    }
}

struct CaptureProvider {
    observed: Rc<RefCell<Option<Vec<ToolDefinition>>>>,
    calls: Rc<Cell<usize>>,
}
impl LlmProvider for CaptureProvider {
    fn infer(
        &mut self,
        request: LlmRequest,
        _: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError> {
        self.calls.set(self.calls.get() + 1);
        *self.observed.borrow_mut() = Some(request.tools);
        Ok(LlmResponse {
            payload: native_response(),
        })
    }
}

fn tool(name: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: "  description bytes  ".into(),
        input_schema: schema,
    }
}

fn capture_agent(
    manager_tools: Vec<ToolDefinition>,
    observed: Rc<RefCell<Option<Vec<ToolDefinition>>>>,
    calls: Rc<Cell<usize>>,
) -> (
    Agent<CaptureProvider, DisclosureManager, ScriptStorage>,
    Rc<Counts>,
) {
    let counts = Rc::new(Counts::default());
    let agent = Agent::new(
        CaptureProvider { observed, calls },
        DisclosureManager {
            tools: manager_tools,
        },
        ScriptStorage {
            counts: counts.clone(),
            error: None,
        },
        "prompt".into(),
    )
    .unwrap();
    (agent, counts)
}

#[test]
fn registry_is_lazy_scoped_and_overrides_manager_disclosures() {
    let observed = Rc::new(RefCell::new(None));
    let provider_calls = Rc::new(Cell::new(0));
    let manager_definition = tool("manager-only", json!({}));
    let (mut agent, _) = capture_agent(
        vec![manager_definition],
        observed.clone(),
        provider_calls.clone(),
    );
    agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(*observed.borrow(), Some(Vec::new()));

    let definitions = vec![tool(
        "lookup",
        json!({"type":"object", "properties":{"x":{"type":"string"}}}),
    )];
    let registry_calls = Rc::new(Cell::new(0));
    agent.set_tool_registry(Box::new(TestRegistry {
        calls: registry_calls.clone(),
        result: Ok(definitions.clone()),
    }));
    assert_eq!(registry_calls.get(), 0);
    *observed.borrow_mut() = None;
    agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(registry_calls.get(), 1);
    assert_eq!(*observed.borrow(), Some(definitions));

    let replacement = vec![tool("replacement", json!({"type":"object"}))];
    let replacement_calls = Rc::new(Cell::new(0));
    agent.set_tool_registry(Box::new(TestRegistry {
        calls: replacement_calls.clone(),
        result: Ok(replacement.clone()),
    }));
    assert_eq!(registry_calls.get(), 1);
    assert_eq!(replacement_calls.get(), 0);
    *observed.borrow_mut() = None;
    agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(registry_calls.get(), 1);
    assert_eq!(replacement_calls.get(), 1);
    assert_eq!(*observed.borrow(), Some(replacement));

    agent.clear_tool_registry();
    *observed.borrow_mut() = None;
    agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(*observed.borrow(), Some(Vec::new()));
    assert_eq!(registry_calls.get(), 1);
    assert_eq!(replacement_calls.get(), 1);
    assert_eq!(provider_calls.get(), 4);
}

#[test]
fn invalid_registry_output_and_registry_errors_stop_before_inference() {
    let cases = [
        (vec![tool("", json!({}))], "invalid tool name"),
        (vec![tool(" x", json!({}))], "invalid tool name"),
        (
            vec![tool("same", json!({})), tool("same", json!({}))],
            "duplicate tool name",
        ),
        (
            vec![tool("bad-schema", json!([]))],
            "tool input_schema must be an object",
        ),
        (
            (0..129)
                .map(|i| tool(&format!("t{i}"), json!({})))
                .collect(),
            "too many tool definitions",
        ),
    ];
    for (definitions, expected_message) in cases {
        let observed = Rc::new(RefCell::new(None));
        let provider_calls = Rc::new(Cell::new(0));
        let (mut agent, counts) = capture_agent(vec![], observed, provider_calls.clone());
        agent.set_tool_registry(Box::new(TestRegistry {
            calls: Rc::new(Cell::new(0)),
            result: Ok(definitions),
        }));
        let err = agent.run_turn(input(), &Flag(false)).unwrap_err();
        assert_eq!(err, component_error(AgentPhase::Tools, expected_message));
        assert_eq!(provider_calls.get(), 0);
        assert_eq!(counts.append.get(), 0);
    }

    let observed = Rc::new(RefCell::new(None));
    let provider_calls = Rc::new(Cell::new(0));
    let (mut agent, counts) = capture_agent(vec![], observed, provider_calls.clone());
    agent.set_tool_registry(Box::new(TestRegistry {
        calls: Rc::new(Cell::new(0)),
        result: Err(component_error(AgentPhase::Cancelled, "registry cancelled")),
    }));
    assert_eq!(
        agent.run_turn(input(), &Flag(false)).unwrap_err(),
        component_error(AgentPhase::Tools, "registry cancelled")
    );
    assert_eq!(provider_calls.get(), 0);
    assert_eq!(counts.append.get(), 0);
}

#[test]
fn cancellation_sampled_after_registry_prevents_inference() {
    let observed = Rc::new(RefCell::new(None));
    let provider_calls = Rc::new(Cell::new(0));
    let (mut agent, counts) = capture_agent(vec![], observed, provider_calls.clone());
    let registry_calls = Rc::new(Cell::new(0));
    agent.set_tool_registry(Box::new(TestRegistry {
        calls: registry_calls.clone(),
        result: Ok(vec![tool("safe", json!({}))]),
    }));
    let cancel = CancelAt {
        checks: Cell::new(0),
        cancel_at: 3,
    };
    assert_eq!(
        agent.run_turn(input(), &cancel).unwrap_err().phase,
        AgentPhase::Cancelled
    );
    assert_eq!(registry_calls.get(), 1);
    assert_eq!(provider_calls.get(), 0);
    assert_eq!(counts.append.get(), 0);
}

#[test]
fn constructor_does_not_call_components_and_keeps_prompt_bytes() {
    let counts = Rc::new(Counts::default());
    let agent = scripted_agent(counts.clone(), None, None, None, None, false, false);
    assert_eq!(agent.system_prompt, "  keep prompt bytes  ");
    assert_eq!(counts.prepare.get(), 0);
    assert_eq!(counts.infer.get(), 0);
    assert_eq!(counts.finish.get(), 0);
    assert_eq!(counts.append.get(), 0);
    let err = Agent::new(
        ScriptProvider {
            counts: counts.clone(),
            error: None,
        },
        ScriptManager {
            counts: counts.clone(),
            prepare_error: None,
            finish_error: None,
            bad_request: false,
            bad_record: false,
        },
        ScriptStorage {
            counts: counts.clone(),
            error: None,
        },
        " \t\n ".into(),
    )
    .err()
    .unwrap();
    assert_eq!(err.phase, AgentPhase::Validation);
    assert_eq!(counts.prepare.get(), 0);
    assert_eq!(counts.infer.get(), 0);
}

#[test]
fn default_manager_and_agent_preserve_native_payload_and_valid_identity_bytes() {
    let input_value = native_input();
    let mut manager = DefaultChatTurnManager;
    let request = manager
        .prepare(
            TurnInput {
                turn_id: " id with bytes ".into(),
                input: input_value.clone(),
            },
            " prompt bytes ",
        )
        .unwrap();
    assert_eq!(request.turn_id, " id with bytes ");
    assert_eq!(request.system_prompt, " prompt bytes ");
    assert_eq!(request.input, input_value);
    let response = native_response();
    let record = manager
        .finish(
            " id with bytes ",
            LlmResponse {
                payload: response.clone(),
            },
        )
        .unwrap();
    assert_eq!(record.response, response);

    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(counts.clone(), None, None, None, None, false, false);
    let outcome = agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(outcome.response, native_response());
    assert_eq!(outcome.receipt.reference, "test-receipt");
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn monocle_wrapper_is_lazy_transparent_and_calls_once_with_same_cancel_reference() {
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let expected = Flag(false);
    let expected_ptr = &expected as *const Flag as *const dyn Cancellation;
    let mut provider =
        MonocleLlmProvider::new(move |request: LlmRequest, cancel: &dyn Cancellation| {
            observed.set(observed.get() + 1);
            assert_eq!(request.turn_id, " t ");
            assert_eq!(request.system_prompt, " p ");
            assert_eq!(request.input, native_input());
            assert_eq!(cancel as *const dyn Cancellation, expected_ptr);
            Ok(LlmResponse {
                payload: native_response(),
            })
        });
    assert_eq!(calls.get(), 0);
    let response = provider
        .infer(
            LlmRequest {
                turn_id: " t ".into(),
                system_prompt: " p ".into(),
                input: native_input(),
                tools: vec![],
            },
            &expected,
        )
        .unwrap();
    assert_eq!(response.payload, native_response());
    assert_eq!(calls.get(), 1);
    let mut err_provider = MonocleLlmProvider::new(|_: LlmRequest, _: &dyn Cancellation| {
        Err(component_error(
            AgentPhase::Validation,
            "native callback failure",
        ))
    });
    assert_eq!(
        err_provider
            .infer(
                LlmRequest {
                    turn_id: "t".into(),
                    system_prompt: "p".into(),
                    input: json!({}),
                    tools: vec![],
                },
                &expected
            )
            .unwrap_err(),
        component_error(AgentPhase::Validation, "native callback failure")
    );
}

#[test]
fn pre_cancel_runs_no_component_and_invalid_id_precedes_cancellation() {
    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(counts.clone(), None, None, None, None, false, false);
    assert_eq!(
        agent.run_turn(input(), &Flag(true)).unwrap_err().phase,
        AgentPhase::Cancelled
    );
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (0, 0, 0, 0)
    );
    let invalid = TurnInput {
        turn_id: " \n".into(),
        input: json!(null),
    };
    assert_eq!(
        agent.run_turn(invalid, &Flag(true)).unwrap_err().phase,
        AgentPhase::Validation
    );
    assert_eq!(counts.prepare.get(), 0);
}

#[test]
fn cancellation_at_each_stage_boundary_has_exact_call_counts() {
    let expected = [
        (1, (0, 0, 0, 0)),
        (2, (1, 0, 0, 0)),
        (3, (1, 1, 0, 0)),
        (4, (1, 1, 1, 0)),
    ];
    for (cancel_at, counts_expected) in expected {
        let counts = Rc::new(Counts::default());
        let mut agent = scripted_agent(counts.clone(), None, None, None, None, false, false);
        let cancel = CancelAt {
            checks: Cell::new(0),
            cancel_at,
        };
        assert_eq!(
            agent.run_turn(input(), &cancel).unwrap_err().phase,
            AgentPhase::Cancelled
        );
        assert_eq!(
            (
                counts.prepare.get(),
                counts.infer.get(),
                counts.finish.get(),
                counts.append.get()
            ),
            counts_expected
        );
    }
}

#[test]
fn manager_identity_validation_precedes_next_cancellation_check() {
    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(counts.clone(), None, None, None, None, true, false);
    let cancel = CancelAt {
        checks: Cell::new(0),
        cancel_at: 2,
    };
    let err = agent.run_turn(input(), &cancel).unwrap_err();
    assert_eq!(err.phase, AgentPhase::Manager);
    assert_eq!(err.message, "request identity changed");
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (1, 0, 0, 0)
    );

    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(counts.clone(), None, None, None, None, false, true);
    let cancel = CancelAt {
        checks: Cell::new(0),
        cancel_at: 4,
    };
    let err = agent.run_turn(input(), &cancel).unwrap_err();
    assert_eq!(err.phase, AgentPhase::Manager);
    assert_eq!(err.message, "record identity changed");
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (1, 1, 1, 0)
    );
}

#[test]
fn component_failures_are_normalized_to_invoked_stage_without_retry() {
    let cases = [
        (
            Some(component_error(AgentPhase::Validation, "prepare failed")),
            None,
            None,
            None,
            AgentPhase::Manager,
            "prepare failed",
            (1, 0, 0, 0),
        ),
        (
            None,
            Some(component_error(AgentPhase::Storage, "provider failed")),
            None,
            None,
            AgentPhase::Provider,
            "provider failed",
            (1, 1, 0, 0),
        ),
        (
            None,
            None,
            Some(component_error(AgentPhase::Provider, "finish failed")),
            None,
            AgentPhase::Manager,
            "finish failed",
            (1, 1, 1, 0),
        ),
        (
            None,
            None,
            None,
            Some(component_error(AgentPhase::Provider, "append failed")),
            AgentPhase::Storage,
            "append failed",
            (1, 1, 1, 1),
        ),
    ];
    for (prepare, provider, finish, storage, phase, message, counts_expected) in cases {
        let counts = Rc::new(Counts::default());
        let mut agent = scripted_agent(
            counts.clone(),
            prepare,
            provider,
            finish,
            storage,
            false,
            false,
        );
        let err = agent.run_turn(input(), &Flag(false)).unwrap_err();
        assert_eq!(err.phase, phase);
        assert_eq!(err.message, message);
        assert_eq!(
            (
                counts.prepare.get(),
                counts.infer.get(),
                counts.finish.get(),
                counts.append.get()
            ),
            counts_expected
        );
    }
}

#[test]
fn explicit_component_cancellation_is_preserved_only_from_provider() {
    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(
        counts.clone(),
        None,
        Some(component_error(AgentPhase::Cancelled, "provider cancelled")),
        None,
        None,
        false,
        false,
    );
    let err = agent.run_turn(input(), &Flag(false)).unwrap_err();
    assert_eq!(
        err,
        component_error(AgentPhase::Cancelled, "provider cancelled")
    );
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (1, 1, 0, 0)
    );

    let cases = [
        (
            Some(component_error(AgentPhase::Cancelled, "prepare cancelled")),
            None,
            None,
            AgentPhase::Manager,
            "prepare cancelled",
            (1, 0, 0, 0),
        ),
        (
            None,
            Some(component_error(AgentPhase::Cancelled, "finish cancelled")),
            None,
            AgentPhase::Manager,
            "finish cancelled",
            (1, 1, 1, 0),
        ),
        (
            None,
            None,
            Some(component_error(AgentPhase::Cancelled, "storage cancelled")),
            AgentPhase::Storage,
            "storage cancelled",
            (1, 1, 1, 1),
        ),
    ];
    for (prepare_error, finish_error, storage_error, phase, message, expected_counts) in cases {
        let counts = Rc::new(Counts::default());
        let mut agent = scripted_agent(
            counts.clone(),
            prepare_error,
            None,
            finish_error,
            storage_error,
            false,
            false,
        );
        let err = agent.run_turn(input(), &Flag(false)).unwrap_err();
        assert_eq!(err.phase, phase);
        assert_eq!(err.message, message);
        assert_eq!(
            (
                counts.prepare.get(),
                counts.infer.get(),
                counts.finish.get(),
                counts.append.get()
            ),
            expected_counts
        );
    }
}

#[test]
fn storage_error_does_not_retry_inference_and_response_is_not_reported_successful() {
    let counts = Rc::new(Counts::default());
    let mut agent = scripted_agent(
        counts.clone(),
        None,
        None,
        None,
        Some(component_error(
            AgentPhase::Provider,
            "write outcome unknown",
        )),
        false,
        false,
    );
    let err = agent.run_turn(input(), &Flag(false)).unwrap_err();
    assert_eq!(
        err,
        component_error(AgentPhase::Storage, "write outcome unknown")
    );
    assert_eq!(
        (
            counts.prepare.get(),
            counts.infer.get(),
            counts.finish.get(),
            counts.append.get()
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn db_storage_forwards_full_record_once_and_preserves_receipt_or_error() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = seen.clone();
    let receipt = StorageReceipt {
        reference: "db:row-7/native".into(),
    };
    let returned = receipt.clone();
    let mut storage = DbAgentStorage::new(move |record: &TurnRecord| {
        captured.borrow_mut().push(record.clone());
        Ok(returned.clone())
    });
    let record = TurnRecord {
        turn_id: "id".into(),
        response: native_response(),
    };
    assert_eq!(seen.borrow().len(), 0);
    assert_eq!(storage.append(&record).unwrap(), receipt);
    assert_eq!(seen.borrow().as_slice(), &[record]);
    assert_eq!(seen.borrow().len(), 1);

    let expected = component_error(AgentPhase::Validation, "host transaction failed");
    let returned_error = expected.clone();
    let mut failing = DbAgentStorage::new(move |_: &TurnRecord| Err(returned_error.clone()));
    assert_eq!(
        failing
            .append(&TurnRecord {
                turn_id: "x".into(),
                response: json!(null)
            })
            .unwrap_err(),
        expected
    );
}

#[test]
fn agent_integrates_with_db_and_file_storage_without_retries() {
    let provider_calls = Rc::new(Cell::new(0));
    let provider_seen = provider_calls.clone();
    let records = Rc::new(RefCell::new(Vec::new()));
    let records_seen = records.clone();
    let db_receipt = StorageReceipt {
        reference: "host-tx:ack-1".into(),
    };
    let callback_receipt = db_receipt.clone();
    let mut db_agent = Agent::new(
        MonocleLlmProvider::new(move |request: LlmRequest, _: &dyn Cancellation| {
            provider_seen.set(provider_seen.get() + 1);
            assert_eq!(request.input, native_input());
            assert_eq!(request.system_prompt, "configured system");
            Ok(LlmResponse {
                payload: native_response(),
            })
        }),
        DefaultChatTurnManager,
        DbAgentStorage::new(move |record: &TurnRecord| {
            records_seen.borrow_mut().push(record.clone());
            Ok(callback_receipt.clone())
        }),
        "configured system".into(),
    )
    .unwrap();
    let db_outcome = db_agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(db_outcome.response, native_response());
    assert_eq!(db_outcome.receipt, db_receipt);
    assert_eq!(provider_calls.get(), 1);
    assert_eq!(
        records.borrow().as_slice(),
        &[TurnRecord {
            turn_id: " turn-1 ".into(),
            response: native_response()
        }]
    );

    let path = temp_path("agent");
    let file_provider_calls = Rc::new(Cell::new(0));
    let file_provider_seen = file_provider_calls.clone();
    let mut file_agent = Agent::new(
        MonocleLlmProvider::new(move |_: LlmRequest, _: &dyn Cancellation| {
            file_provider_seen.set(file_provider_seen.get() + 1);
            Ok(LlmResponse {
                payload: native_response(),
            })
        }),
        DefaultChatTurnManager,
        FileAgentStorage::new(&path),
        "configured system".into(),
    )
    .unwrap();
    let file_outcome = file_agent.run_turn(input(), &Flag(false)).unwrap();
    assert_eq!(file_outcome.response, native_response());
    assert_eq!(file_outcome.receipt.reference, path.to_string_lossy());
    assert_eq!(file_provider_calls.get(), 1);
    let lines = fs::read_to_string(&path).unwrap();
    assert_eq!(lines.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(lines.lines().next().unwrap()).unwrap(),
        json!({
            "turn_id": " turn-1 ", "response": native_response()
        })
    );
    fs::remove_file(path).unwrap();

    let failed_calls = Rc::new(Cell::new(0));
    let failed_provider_calls = failed_calls.clone();
    let db_error = component_error(AgentPhase::Provider, "host transaction outcome unknown");
    let callback_error = db_error.clone();
    let mut failed_db_agent = Agent::new(
        MonocleLlmProvider::new(move |_: LlmRequest, _: &dyn Cancellation| {
            failed_provider_calls.set(failed_provider_calls.get() + 1);
            Ok(LlmResponse {
                payload: native_response(),
            })
        }),
        DefaultChatTurnManager,
        DbAgentStorage::new(move |_: &TurnRecord| Err(callback_error.clone())),
        "configured system".into(),
    )
    .unwrap();
    let err = failed_db_agent.run_turn(input(), &Flag(false)).unwrap_err();
    assert_eq!(
        err,
        component_error(AgentPhase::Storage, "host transaction outcome unknown")
    );
    assert_eq!(failed_calls.get(), 1);
}

static TEMP_ID: AtomicUsize = AtomicUsize::new(0);
fn temp_path(label: &str) -> PathBuf {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-injection-{label}-{}-{nanos}-{id}.jsonl",
        std::process::id()
    ))
}

#[test]
fn file_storage_writes_two_jsonl_records_and_returns_flush_receipt() {
    let path = temp_path("ok");
    let mut storage = FileAgentStorage::new(&path);
    let first = TurnRecord {
        turn_id: "one".into(),
        response: native_response(),
    };
    let second = TurnRecord {
        turn_id: "two".into(),
        response: json!({"unknown": [1, 2]}),
    };
    assert_eq!(
        storage.append(&first).unwrap().reference,
        path.to_string_lossy()
    );
    assert_eq!(
        storage.append(&second).unwrap().reference,
        path.to_string_lossy()
    );
    let contents = fs::read_to_string(&path).unwrap();
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(lines[0]).unwrap(),
        json!({"turn_id":"one", "response":native_response()})
    );
    assert_eq!(
        serde_json::from_str::<Value>(lines[1]).unwrap(),
        json!({"turn_id":"two", "response":{"unknown":[1,2]}})
    );
    assert!(contents.ends_with('\n'));
    fs::remove_file(path).unwrap();
}

#[test]
fn file_storage_path_error_is_storage_phase() {
    let path = temp_path("blocked");
    fs::create_dir(&path).unwrap();
    let mut storage = FileAgentStorage::new(&path);
    let err = storage
        .append(&TurnRecord {
            turn_id: "x".into(),
            response: json!(1),
        })
        .unwrap_err();
    assert_eq!(err.phase, AgentPhase::Storage);
    assert!(!err.message.is_empty());
    fs::remove_dir(path).unwrap();
}
