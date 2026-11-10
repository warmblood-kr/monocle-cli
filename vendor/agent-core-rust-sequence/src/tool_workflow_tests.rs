use super::*;
use crate::agent::error;
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}
struct CancelAt {
    checks: Cell<usize>,
    at: usize,
}
impl Cancellation for CancelAt {
    fn is_cancelled(&self) -> bool {
        let next = self.checks.get() + 1;
        self.checks.set(next);
        next >= self.at
    }
}

struct ScriptProvider {
    steps: Vec<ModelStep>,
    observed: Rc<RefCell<Vec<WorkflowRequest>>>,
}
impl ToolWorkflowProvider for ScriptProvider {
    fn infer_step(
        &mut self,
        request: WorkflowRequest,
        _: &dyn Cancellation,
    ) -> Result<ModelStep, AgentError> {
        self.observed.borrow_mut().push(request);
        if self.steps.is_empty() {
            return Err(error(AgentPhase::Provider, "script exhausted"));
        }
        Ok(self.steps.remove(0))
    }
}

struct ActorManager {
    corrupt: bool,
}

struct ScopedRegistry;
impl ToolRegistry for ScopedRegistry {
    fn definitions(&self) -> Result<Vec<ToolDefinition>, AgentError> {
        Ok(vec![ToolDefinition {
            name: "lookup_order".into(),
            description: "read order status".into(),
            input_schema: json!({"type":"object"}),
        }])
    }
}
impl ToolWorkflowManager for ActorManager {
    fn prepare_workflow(
        &mut self,
        input: TurnInput,
        prompt: &str,
    ) -> Result<WorkflowRequest, AgentError> {
        Ok(WorkflowRequest {
            turn_id: input.turn_id,
            system_prompt: prompt.into(),
            input: input.input,
            tools: vec![ToolDefinition {
                name: "lookup_order".into(),
                description: "read order status".into(),
                input_schema: json!({"type":"object"}),
            }],
            transcript: vec![],
        })
    }
    fn apply_tool_results(
        &mut self,
        request: &WorkflowRequest,
        step: &ModelStep,
        results: &[ToolResult],
    ) -> Result<WorkflowRequest, AgentError> {
        let mut next = request.clone();
        next.transcript.push(WorkflowExchange {
            step: step.clone(),
            results: results.to_vec(),
        });
        if self.corrupt {
            next.transcript.clear();
        }
        Ok(next)
    }
}

struct ScriptExecutor {
    calls: Rc<Cell<usize>>,
    statuses: Vec<ToolStatus>,
    wrong_key: bool,
    principal: &'static str,
    tenant: &'static str,
}
impl ToolExecutor for ScriptExecutor {
    fn execute(&mut self, call: &ToolCall, _: &dyn Cancellation) -> ToolResult {
        // A host-owned authorization scope is injected into this fake. It never
        // trusts model-supplied arguments as identity or authority.
        assert_eq!(self.principal, "alice");
        assert_eq!(self.tenant, "tenant-a");
        self.calls.set(self.calls.get() + 1);
        let status = if self.statuses.is_empty() {
            ToolStatus::Completed(json!({"status":"shipped"}))
        } else {
            self.statuses.remove(0)
        };
        let mut key = call.key.clone();
        if self.wrong_key {
            key.call_id = "different".into();
        }
        ToolResult { key, status }
    }
}

struct TestStorage {
    calls: Rc<Cell<usize>>,
    record: Rc<RefCell<Option<TurnRecord>>>,
    fail: bool,
}
impl AgentStorage for TestStorage {
    fn append(&mut self, record: &TurnRecord) -> Result<StorageReceipt, AgentError> {
        self.calls.set(self.calls.get() + 1);
        *self.record.borrow_mut() = Some(record.clone());
        if self.fail {
            return Err(error(AgentPhase::Storage, "append outcome uncertain"));
        }
        Ok(StorageReceipt {
            reference: "receipt-42".into(),
        })
    }
}

type TestAgent = ToolWorkflowAgent<ScriptProvider, ActorManager, ScriptExecutor, TestStorage>;
fn setup(
    steps: Vec<ModelStep>,
    statuses: Vec<ToolStatus>,
    max_model_steps: usize,
    max_tool_calls: usize,
    wrong_key: bool,
    corrupt_manager: bool,
    fail_append: bool,
) -> (
    TestAgent,
    Rc<RefCell<Vec<WorkflowRequest>>>,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
    Rc<RefCell<Option<TurnRecord>>>,
) {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let executions = Rc::new(Cell::new(0));
    let appends = Rc::new(Cell::new(0));
    let record = Rc::new(RefCell::new(None));
    let mut agent = ToolWorkflowAgent::new(
        ScriptProvider {
            steps,
            observed: observed.clone(),
        },
        ActorManager {
            corrupt: corrupt_manager,
        },
        ScriptExecutor {
            calls: executions.clone(),
            statuses,
            wrong_key,
            principal: "alice",
            tenant: "tenant-a",
        },
        TestStorage {
            calls: appends.clone(),
            record: record.clone(),
            fail: fail_append,
        },
        "orders".into(),
        WorkflowLimits {
            max_model_steps,
            max_tool_calls,
            max_payload_bytes: 65536,
            max_transcript_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    agent.set_tool_registry(Box::new(ScopedRegistry));
    (agent, observed, executions, appends, record)
}

#[test]
fn manager_disclosures_are_cleared_without_a_host_registry() {
    let (mut agent, _, executions, appends, _) = setup(
        vec![request(vec![order_call("c1")])],
        vec![],
        4,
        2,
        false,
        false,
        false,
    );
    agent.clear_tool_registry();
    assert!(agent.run_turn(user_input(), &NeverCancel).is_err());
    assert_eq!(executions.get(), 0);
    assert_eq!(appends.get(), 0);
}
fn order_call(id: &str) -> ToolCall {
    ToolCall {
        key: CallKey {
            turn_id: "42".into(),
            call_id: id.into(),
        },
        name: "lookup_order".into(),
        arguments: json!({"order_id":42}),
    }
}
fn user_input() -> TurnInput {
    TurnInput {
        turn_id: "42".into(),
        input: json!({"question":"where is 42?"}),
    }
}
fn request(calls: Vec<ToolCall>) -> ModelStep {
    ModelStep::ToolRequests(calls)
}
fn terminal() -> ModelStep {
    ModelStep::Terminal(json!("Order 42 shipped"))
}

#[test]
fn central_actor_flow_executes_once_continues_and_appends_the_full_record() {
    let (mut agent, observed, executions, appends, record) = setup(
        vec![request(vec![order_call("c1")]), terminal()],
        vec![],
        4,
        2,
        false,
        false,
        false,
    );
    let outcome = agent
        .run_turn(user_input(), &NeverCancel)
        .expect("authorized actor flow");
    assert_eq!(outcome.response, json!("Order 42 shipped"));
    assert_eq!(outcome.receipt.reference, "receipt-42");
    assert_eq!(observed.borrow().len(), 2);
    assert_eq!(
        observed.borrow()[1].transcript,
        vec![WorkflowExchange {
            step: request(vec![order_call("c1")]),
            results: vec![ToolResult {
                key: order_call("c1").key,
                status: ToolStatus::Completed(json!({"status":"shipped"}))
            }]
        }]
    );
    assert_eq!(executions.get(), 1);
    assert_eq!(appends.get(), 1);
    let saved = record.borrow().clone().unwrap();
    assert_eq!(saved.turn_id, "42");
    assert_eq!(saved.response["tool_workflow"]["version"], 1);
    assert_eq!(saved.response["final"], outcome.response);
    assert_eq!(
        saved.response["tool_workflow"]["transcript"],
        json!([
            {"step":{"ToolRequests":[{"key":{"turn_id":"42","call_id":"c1"},"name":"lookup_order","arguments":{"order_id":42}}]},
             "results":[{"key":{"turn_id":"42","call_id":"c1"},"status":{"Completed":{"status":"shipped"}}}]},
            {"step":{"Terminal":"Order 42 shipped"},"results":[]}
        ])
    );
}

#[test]
fn invalid_batches_are_rejected_before_any_effect() {
    let cases = vec![
        vec![],
        vec![ToolCall {
            name: "hidden".into(),
            ..order_call("a")
        }],
        vec![ToolCall {
            key: CallKey {
                turn_id: "other".into(),
                call_id: "a".into(),
            },
            ..order_call("a")
        }],
        vec![order_call("a"), order_call("a")],
    ];
    for calls in cases {
        let (mut agent, _, executions, appends, _) =
            setup(vec![request(calls)], vec![], 4, 2, false, false, false);
        assert!(agent.run_turn(user_input(), &NeverCancel).is_err());
        assert_eq!(executions.get(), 0);
        assert_eq!(appends.get(), 0);
    }
    let (mut agent, _, executions, appends, _) = setup(
        vec![request(vec![order_call("a"), order_call("b")])],
        vec![],
        4,
        1,
        false,
        false,
        false,
    );
    assert!(agent.run_turn(user_input(), &NeverCancel).is_err());
    assert_eq!(executions.get(), 0);
    assert_eq!(appends.get(), 0);
}

#[test]
fn wrong_correlation_is_unresolved_and_never_continues() {
    let (mut agent, observed, executions, appends, _) = setup(
        vec![request(vec![order_call("c1")]), terminal()],
        vec![],
        4,
        2,
        true,
        false,
        false,
    );
    let failure = agent.run_turn(user_input(), &NeverCancel).unwrap_err();
    assert_eq!(failure.unresolved, vec![order_call("c1").key]);
    assert_eq!(failure.transcript[0].results[0].key.call_id, "different");
    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(executions.get(), 1);
    assert_eq!(appends.get(), 0);
}

#[test]
fn partial_batch_failure_retains_completed_effect_and_stops() {
    let calls = vec![order_call("c1"), order_call("c2")];
    let (mut agent, observed, executions, appends, _) = setup(
        vec![request(calls), terminal()],
        vec![
            ToolStatus::Completed(json!({"status":"shipped"})),
            ToolStatus::Denied("revoked".into()),
        ],
        4,
        4,
        false,
        false,
        false,
    );
    let failure = agent.run_turn(user_input(), &NeverCancel).unwrap_err();
    assert_eq!(failure.unresolved, Vec::<CallKey>::new());
    assert_eq!(failure.transcript[0].results.len(), 2);
    assert!(matches!(
        failure.transcript[0].results[0].status,
        ToolStatus::Completed(_)
    ));
    assert!(matches!(
        failure.transcript[0].results[1].status,
        ToolStatus::Denied(_)
    ));
    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(executions.get(), 2);
    assert_eq!(appends.get(), 0);
}

#[test]
fn unknown_result_is_retained_as_unresolved_without_continuation() {
    let (mut agent, observed, executions, appends, _) = setup(
        vec![request(vec![order_call("c1")]), terminal()],
        vec![ToolStatus::Unknown("transport lost".into())],
        4,
        2,
        false,
        false,
        false,
    );
    let failure = agent.run_turn(user_input(), &NeverCancel).unwrap_err();
    assert_eq!(failure.unresolved, vec![order_call("c1").key]);
    assert!(matches!(
        failure.transcript[0].results[0].status,
        ToolStatus::Unknown(_)
    ));
    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(executions.get(), 1);
    assert_eq!(appends.get(), 0);
}

#[test]
fn cancellation_before_dispatch_and_after_completed_dispatch_preserves_evidence() {
    let (mut before, observed, executions, appends, _) = setup(
        vec![request(vec![order_call("c1")])],
        vec![],
        4,
        2,
        false,
        false,
        false,
    );
    let failure = before
        .run_turn(
            user_input(),
            &CancelAt {
                checks: Cell::new(0),
                at: 5,
            },
        )
        .unwrap_err();
    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(executions.get(), 0);
    assert_eq!(appends.get(), 0);
    assert!(failure.transcript[0].results.is_empty());

    let (mut after, observed, executions, appends, _) = setup(
        vec![request(vec![order_call("c1")]), terminal()],
        vec![],
        4,
        2,
        false,
        false,
        false,
    );
    let failure = after
        .run_turn(
            user_input(),
            &CancelAt {
                checks: Cell::new(0),
                at: 6,
            },
        )
        .unwrap_err();
    assert_eq!(observed.borrow().len(), 1);
    assert_eq!(executions.get(), 1);
    assert_eq!(appends.get(), 0);
    assert!(matches!(
        failure.transcript[0].results[0].status,
        ToolStatus::Completed(_)
    ));
}

#[test]
fn bounds_manager_integrity_and_append_uncertainty_prevent_success_and_retry() {
    let steps = vec![
        request(vec![order_call("a")]),
        request(vec![order_call("b")]),
        terminal(),
    ];
    let (mut loop_agent, observed, executions, appends, _) =
        setup(steps, vec![], 2, 4, false, false, false);
    let failure = loop_agent.run_turn(user_input(), &NeverCancel).unwrap_err();
    assert_eq!(failure.error.phase, AgentPhase::Provider);
    assert_eq!(observed.borrow().len(), 2);
    assert_eq!(executions.get(), 2);
    assert_eq!(appends.get(), 0);

    let (mut corrupt, _, executions, appends, _) = setup(
        vec![request(vec![order_call("a")]), terminal()],
        vec![],
        4,
        4,
        false,
        true,
        false,
    );
    assert!(corrupt.run_turn(user_input(), &NeverCancel).is_err());
    assert_eq!(executions.get(), 1);
    assert_eq!(appends.get(), 0);

    let (mut storage, _, executions, appends, _) =
        setup(vec![terminal()], vec![], 4, 4, false, false, true);
    let failure = storage.run_turn(user_input(), &NeverCancel).unwrap_err();
    assert_eq!(failure.error.phase, AgentPhase::Storage);
    assert_eq!(executions.get(), 0);
    assert_eq!(appends.get(), 1);
}
