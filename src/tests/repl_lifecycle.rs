use super::{run_selected_turn_with_session, run_turn_lifecycle, Repl, TurnDiagnostics};
use crate::agent::permission::PermissionStore;
use crate::agent::providers::{
    ChatRequest, ChatResponse, FunctionCall, LlmProvider, Message, ToolCall,
};
use crate::agent::runner::{Cancel, RunStop};
use crate::agent::session::SessionStore;
use crate::agent::tools::{Tool, ToolContext, ToolOutcome, ToolRegistry};
use crate::credentials::Credentials;
use crate::error::AppError;
use crate::net::Client;
use serde_json::{json, Value};
use std::cell::Cell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;
use std::{cell::RefCell, io};

// These synthetic cases exercise the production lifecycle helper. The separate
// integration subprocess exercises the actual CLI auth/exit boundary; the
// synthetic success case below additionally calls the real selected-turn path.

#[derive(Clone)]
struct SharedOutput(Rc<RefCell<Vec<u8>>>);

impl Write for SharedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn diagnostics() -> Option<TurnDiagnostics> {
    Some(TurnDiagnostics::for_agent(
        "model".into(),
        None,
        "https://router/v1/chat/completions".into(),
        None,
        0,
        None,
        0,
    ))
}

struct ReplFakeProvider {
    chat_calls: Rc<Cell<usize>>,
    stream_calls: Rc<Cell<usize>>,
}

struct FailingReplProvider {
    calls: Rc<Cell<usize>>,
}

impl LlmProvider for FailingReplProvider {
    fn chat(&self, _request: &ChatRequest) -> crate::error::Result<ChatResponse> {
        panic!("native Repl failure test must use streaming")
    }

    fn chat_stream(
        &self,
        _request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> crate::error::Result<ChatResponse> {
        self.calls.set(self.calls.get() + 1);
        on_delta("partial before error");
        Err(AppError::new("synthetic provider failure"))
    }
}

impl LlmProvider for ReplFakeProvider {
    fn chat(&self, _request: &ChatRequest) -> crate::error::Result<ChatResponse> {
        self.chat_calls.set(self.chat_calls.get() + 1);
        Ok(ChatResponse {
            content: "model-only answer".into(),
            tool_calls: Vec::new(),
            model: Some("synthetic-model".into()),
            finish_reason: Some("stop".into()),
            truncated: false,
            usage: Some(crate::agent::providers::TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
            }),
        })
    }

    fn chat_stream(
        &self,
        _request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> crate::error::Result<ChatResponse> {
        self.stream_calls.set(self.stream_calls.get() + 1);
        on_delta("native answer");
        Ok(ChatResponse {
            content: "native answer".into(),
            tool_calls: Vec::new(),
            model: Some("synthetic-model".into()),
            finish_reason: Some("stop".into()),
            truncated: false,
            usage: None,
        })
    }
}

fn repl_for_test<'a>(
    client: &'a Client,
    creds: &'a Credentials,
    workdir: &Path,
    session: Option<SessionStore>,
    output: &'a mut dyn Write,
    provider: Box<dyn LlmProvider>,
    model_only: bool,
) -> Repl<'a> {
    Repl {
        client,
        creds,
        tools: ToolRegistry::new(),
        workdir: workdir.to_path_buf(),
        model: "synthetic-model".into(),
        max_steps: 2,
        cancel: Cancel::new(),
        session,
        convo: vec![Message::system("system")],
        persisted: 1,
        selected_session_poisoned: false,
        auto_approve: false,
        experimental_core_model_only: model_only,
        experimental_core_tool_workflow: false,
        interactive: false,
        permissions: PermissionStore::load(workdir),
        diagnostics: None,
        test_provider: Some((provider, "synthetic://chat".into())),
        test_output: Some(output),
    }
}

struct WorkflowThenProviderError {
    calls: Rc<Cell<usize>>,
}
impl LlmProvider for WorkflowThenProviderError {
    fn chat(&self, request: &ChatRequest) -> crate::error::Result<ChatResponse> {
        let call = self.calls.get() + 1;
        self.calls.set(call);
        if call == 1 {
            assert!(request
                .tools
                .iter()
                .any(|tool| tool.function.name == "effect"));
            Ok(ChatResponse {
                content: "checking before effect".into(),
                tool_calls: vec![ToolCall {
                    id: "call-effect-1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "effect".into(),
                        arguments: "{}".into(),
                    },
                }],
                model: Some("workflow-model".into()),
                finish_reason: Some("tool_calls".into()),
                truncated: false,
                usage: None,
            })
        } else {
            Err(AppError::new("provider failed after tool effect"))
        }
    }
}

struct EffectTool {
    calls: Rc<Cell<usize>>,
    cancel_after_effect: Option<Cancel>,
}
impl Tool for EffectTool {
    fn name(&self) -> &str {
        "effect"
    }
    fn description(&self) -> &str {
        "test effect"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object"})
    }
    fn is_side_effecting(&self) -> bool {
        false
    }
    fn run(&self, _context: &ToolContext, _args: &Value) -> ToolOutcome {
        self.calls.set(self.calls.get() + 1);
        if let Some(cancel) = &self.cancel_after_effect {
            cancel.cancel();
        }
        ToolOutcome::ok("effect completed")
    }
}

fn session_with_system(dir: &Path) -> SessionStore {
    let store = SessionStore::new(dir.join("session.jsonl"));
    store.append(&[Message::system("system")]).unwrap();
    store
}

#[test]
fn tool_effect_then_provider_error_poison_preserves_failure_transcript_and_blocks_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session = session_with_system(dir.path());
    let before = std::fs::read(session.path()).unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let provider_calls = Rc::new(Cell::new(0));
    let effect_calls = Rc::new(Cell::new(0));
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(WorkflowThenProviderError {
            calls: provider_calls.clone(),
        }),
        false,
    );
    repl.experimental_core_tool_workflow = true;
    repl.tools.add(Box::new(EffectTool {
        calls: effect_calls.clone(),
        cancel_after_effect: None,
    }));

    let error = repl.run_turn("perform effect".into()).unwrap_err();
    assert!(error
        .to_string()
        .contains("provider failed after tool effect"));
    assert_eq!(provider_calls.get(), 2);
    assert_eq!(effect_calls.get(), 1);
    assert!(
        repl.selected_session_poisoned,
        "effect uncertainty must poison this selected session"
    );
    assert!(
        repl.convo.iter().any(|message| message.role == "tool"
            && message.content.as_deref() == Some("effect completed")),
        "failure transcript remains visible in memory"
    );
    assert_eq!(repl.persisted, 1);
    assert_eq!(
        std::fs::read(repl.session.as_ref().unwrap().path()).unwrap(),
        before,
        "failed workflow is not saved as success"
    );
    assert!(
        output_bytes.borrow().is_empty(),
        "no successful assistant output"
    );

    let next = repl.run_turn("retry?".into()).unwrap_err();
    assert!(next.to_string().contains("reconciliation"));
    assert_eq!(
        provider_calls.get(),
        2,
        "poison blocks another provider call"
    );
    assert_eq!(effect_calls.get(), 1, "poison blocks another effect");
}

#[test]
fn post_effect_cancel_leaves_reopen_marker_and_reopened_repl_refuses_before_provider() {
    let dir = tempfile::tempdir().unwrap();
    let session = session_with_system(dir.path());
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let provider_calls = Rc::new(Cell::new(0));
    let effect_calls = Rc::new(Cell::new(0));
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut first = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(WorkflowThenProviderError {
            calls: provider_calls.clone(),
        }),
        false,
    );
    first.experimental_core_tool_workflow = true;
    first.tools.add(Box::new(EffectTool {
        calls: effect_calls.clone(),
        cancel_after_effect: Some(first.cancel.clone()),
    }));

    assert!(first.run_turn("cancel after effect".into()).is_err());
    assert_eq!(
        provider_calls.get(),
        1,
        "post-effect cancel stops before continuation inference"
    );
    assert_eq!(effect_calls.get(), 1);
    assert!(first.selected_session_poisoned);
    let marker = std::path::PathBuf::from(format!(
        "{}.core-tool-workflow-pending",
        first.session.as_ref().unwrap().path().display()
    ));
    assert!(marker.is_dir(), "uncertain dispatch is durable across exit");

    let reopened_provider_calls = Rc::new(Cell::new(0));
    let mut reopened_output = SharedOutput(Rc::new(RefCell::new(Vec::new())));
    let mut reopened = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(SessionStore::new(dir.path().join("session.jsonl"))),
        &mut reopened_output,
        Box::new(ReopenedProvider {
            calls: reopened_provider_calls.clone(),
        }),
        false,
    );
    reopened.experimental_core_tool_workflow = true;
    let error = reopened
        .run_turn("must not silently replay".into())
        .unwrap_err();
    assert!(error.to_string().contains("reconcile"));
    assert_eq!(
        reopened_provider_calls.get(),
        0,
        "reopen marker rejects before provider use"
    );
    assert!(
        reopened.test_provider.is_some(),
        "preflight leaves provider unused"
    );
    assert!(reopened_output.0.borrow().is_empty());
}

#[test]
fn pending_workflow_journal_fences_default_model_only_and_workflow_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let mut observations = Vec::new();

    for (name, model_only, workflow) in [
        ("default", false, false),
        ("model-only", true, false),
        ("workflow", false, true),
    ] {
        let session = SessionStore::new(dir.path().join(format!("{name}.jsonl")));
        session.append(&[Message::system("system")]).unwrap();
        let marker = std::path::PathBuf::from(format!(
            "{}.core-tool-workflow-pending",
            session.path().display()
        ));
        std::fs::create_dir(&marker).unwrap();
        std::fs::write(
            marker.join("journal.json"),
            br#"{"version":1,"turn_id":"uncertain","state":"uncertain"}"#,
        )
        .unwrap();
        let chat_calls = Rc::new(Cell::new(0));
        let stream_calls = Rc::new(Cell::new(0));
        let output_bytes = Rc::new(RefCell::new(Vec::new()));
        let mut output = SharedOutput(output_bytes.clone());
        let provider = ReplFakeProvider {
            chat_calls: chat_calls.clone(),
            stream_calls: stream_calls.clone(),
        };
        let mut repl = repl_for_test(
            &client,
            &creds,
            dir.path(),
            Some(session),
            &mut output,
            Box::new(provider),
            model_only,
        );
        repl.experimental_core_tool_workflow = workflow;
        let result = repl.run_turn("must stay fenced".into());
        observations.push((
            name,
            result.is_err(),
            chat_calls.get() + stream_calls.get(),
            repl.test_provider.is_some(),
            output_bytes.borrow().is_empty(),
        ));
    }

    assert!(
        observations.iter().all(|(_, rejected, calls, provider_unused, output_empty)| {
            *rejected && *calls == 0 && *provider_unused && *output_empty
        }),
        "every selected/default route must refuse before provider/output; observed {observations:?}"
    );
}

#[test]
fn pending_journal_metadata_error_fails_closed_before_default_provider() {
    let dir = tempfile::tempdir().unwrap();
    let blocked_parent = dir.path().join("not-a-directory");
    std::fs::write(&blocked_parent, b"file").unwrap();
    let session = SessionStore::new(blocked_parent.join("session.jsonl"));
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(provider),
        false,
    );

    let result = repl.run_turn("metadata must fail closed".into());

    assert!(result.is_err());
    assert_eq!(chat_calls.get() + stream_calls.get(), 0);
    assert!(repl.test_provider.is_some());
    assert!(output_bytes.borrow().is_empty());
}

struct ReopenedProvider {
    calls: Rc<Cell<usize>>,
}
impl LlmProvider for ReopenedProvider {
    fn chat(&self, _request: &ChatRequest) -> crate::error::Result<ChatResponse> {
        self.calls.set(self.calls.get() + 1);
        Ok(ChatResponse {
            content: "should not run".into(),
            tool_calls: vec![],
            model: Some("reopen-model".into()),
            finish_reason: Some("stop".into()),
            truncated: false,
            usage: None,
        })
    }
}

#[test]
fn preflight_rejects_before_auth_provider_output_or_append() {
    let dir = tempfile::tempdir().unwrap();
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    let mut conversation = vec![Message::system("system"), Message::tool("call", "result")];
    let mut persisted = 1;
    let mut diag = diagnostics();
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let auth_calls = Cell::new(0);
    let provider_calls = Cell::new(0);
    let append_calls = Cell::new(0);

    let result = run_turn_lifecycle::<(), _, _, _>(
        true,
        false,
        &mut conversation,
        &mut persisted,
        &mut false,
        &mut diag,
        Some(&session),
        &Cancel::new(),
        "model",
        "new user turn".into(),
        &mut output,
        || auth_calls.set(auth_calls.get() + 1),
        |(), _, _, _, _, _, _, _| {
            provider_calls.set(provider_calls.get() + 1);
            Ok("unused".into())
        },
        |_, _| {
            append_calls.set(append_calls.get() + 1);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(auth_calls.get(), 0);
    assert_eq!(provider_calls.get(), 0);
    assert_eq!(append_calls.get(), 0);
    assert!(output_bytes.borrow().is_empty());
    assert_eq!(conversation.len(), 2);
    assert_eq!(persisted, 1);
    assert!(diag.is_none());
}

#[test]
fn provider_error_rolls_back_turn_and_skips_append() {
    let dir = tempfile::tempdir().unwrap();
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    let mut conversation = vec![Message::system("system")];
    let mut persisted = 1;
    let mut diag = diagnostics();
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let provider_calls = Cell::new(0);
    let append_calls = Cell::new(0);

    let result = run_turn_lifecycle(
        false,
        false,
        &mut conversation,
        &mut persisted,
        &mut false,
        &mut diag,
        Some(&session),
        &Cancel::new(),
        "model",
        "new user turn".into(),
        &mut output,
        || (),
        |(), _, messages, observer, _, _, _, _| {
            provider_calls.set(provider_calls.get() + 1);
            messages.push(Message::assistant("partial"));
            observer.on_text_delta("partial");
            Err(AppError::new("synthetic provider failure"))
        },
        |_, _| {
            append_calls.set(append_calls.get() + 1);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(provider_calls.get(), 1);
    assert_eq!(append_calls.get(), 0);
    assert_eq!(conversation.len(), 1);
    assert_eq!(persisted, 1);
    assert!(diag.is_none());
    // Streaming already exposed before an error remains observable; rollback is
    // of conversation state only, matching current host behavior.
    assert_eq!(*output_bytes.borrow(), b"partial");
}

#[test]
fn append_failure_happens_after_answer_output_and_keeps_success_state() {
    let dir = tempfile::tempdir().unwrap();
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    let mut conversation = vec![Message::system("system")];
    let mut persisted = 1;
    let mut diag = None;
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let append_calls = Cell::new(0);

    let result = run_turn_lifecycle(
        false,
        false,
        &mut conversation,
        &mut persisted,
        &mut false,
        &mut diag,
        Some(&session),
        &Cancel::new(),
        "model",
        "new user turn".into(),
        &mut output,
        || (),
        |(), _, messages, observer, _, _, _, _| {
            messages.push(Message::assistant("answer"));
            observer.on_text_delta("answer");
            Ok("https://router/v1/chat/completions".into())
        },
        |_, messages| {
            append_calls.set(append_calls.get() + 1);
            assert_eq!(messages.len(), 2);
            assert_eq!(
                String::from_utf8_lossy(output_bytes.borrow().as_slice()),
                "answer\n"
            );
            Err(AppError::new("synthetic append failure"))
        },
    );

    assert!(result.is_err());
    assert_eq!(append_calls.get(), 1);
    assert_eq!(conversation.len(), 3);
    assert_eq!(persisted, 1);
    assert!(diag.is_some());
    assert_eq!(*output_bytes.borrow(), b"answer\n");
}

#[test]
fn successful_turn_without_session_does_not_advance_persisted_cursor() {
    let mut conversation = vec![Message::system("system")];
    let mut persisted = 1;
    let mut diag = None;
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes);
    let append_calls = Cell::new(0);

    let result = run_turn_lifecycle(
        true,
        false,
        &mut conversation,
        &mut persisted,
        &mut false,
        &mut diag,
        None,
        &Cancel::new(),
        "model",
        "new user turn".into(),
        &mut output,
        || (),
        |(), _, messages, observer, _, _, _, _| {
            messages.push(Message::assistant("answer"));
            observer.on_text_delta("answer");
            Ok("synthetic://endpoint".into())
        },
        |_, _| {
            append_calls.set(append_calls.get() + 1);
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(append_calls.get(), 0);
    assert_eq!(persisted, 1);
    assert_eq!(conversation.len(), 1);
    assert!(diag.is_none());
}

#[test]
fn lifecycle_can_run_the_real_selected_turn_with_a_synthetic_provider() {
    struct FakeProvider(Cell<usize>);
    impl LlmProvider for FakeProvider {
        fn chat(&self, _request: &ChatRequest) -> crate::error::Result<ChatResponse> {
            self.0.set(self.0.get() + 1);
            Ok(ChatResponse {
                content: "selected answer".into(),
                tool_calls: Vec::new(),
                model: Some("synthetic".into()),
                finish_reason: Some("stop".into()),
                truncated: false,
                usage: Some(crate::agent::providers::TokenUsage {
                    prompt_tokens: 11,
                    completion_tokens: 7,
                    total_tokens: 18,
                }),
            })
        }

        fn chat_stream(
            &self,
            _request: &ChatRequest,
            _on_delta: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("model-only path must use the single non-stream call")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    let provider = FakeProvider(Cell::new(0));
    let mut conversation = vec![Message::system("system")];
    let mut persisted = 1;
    let mut diag = None;
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let append_calls = Cell::new(0);

    let result = run_turn_lifecycle(
        true,
        false,
        &mut conversation,
        &mut persisted,
        &mut false,
        &mut diag,
        Some(&session),
        &Cancel::new(),
        "model",
        "new user turn".into(),
        &mut output,
        || (),
        |(), request, messages, observer, cancel, persisted, session, poisoned| {
            let stop = run_selected_turn_with_session(
                true,
                &provider,
                request,
                messages,
                observer,
                cancel,
                persisted,
                session,
                poisoned,
                |_, _| panic!("native path must not be selected"),
            )?;
            assert_eq!(stop, RunStop::EndTurn);
            Ok("synthetic://endpoint".into())
        },
        |_, messages| {
            append_calls.set(append_calls.get() + 1);
            assert_eq!(messages.len(), 2);
            Ok(())
        },
    );

    assert!(result.is_ok());
    assert_eq!(provider.0.get(), 1);
    assert_eq!(append_calls.get(), 0);
    assert_eq!(persisted, conversation.len());
    assert_eq!(conversation.len(), 3);
    assert_eq!(conversation[2].content.as_deref(), Some("selected answer"));
    assert_eq!(*output_bytes.borrow(), b"selected answer\n");
}

#[test]
fn actual_repl_run_turn_uses_native_default_once_and_persists_output() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    session.append(&[Message::system("system")]).unwrap();
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(provider),
        false,
    );
    // An idle Ctrl-C is cleared before the turn starts.
    repl.cancel.cancel();

    let result = repl.run_turn("native request".into());

    assert!(result.is_ok());
    assert_eq!(chat_calls.get(), 0);
    assert_eq!(stream_calls.get(), 1);
    assert!(!repl.cancel.is_cancelled());
    assert_eq!(repl.convo.len(), 3);
    assert_eq!(repl.convo[2].content.as_deref(), Some("native answer"));
    assert_eq!(repl.persisted, repl.convo.len());
    assert_eq!(*output_bytes.borrow(), b"native answer\n");
    let persisted = repl.session.as_ref().unwrap().load().unwrap();
    assert_eq!(persisted.len(), repl.convo.len());
}

#[test]
fn actual_repl_run_turn_uses_model_only_path_once_when_selected() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        None,
        &mut output,
        Box::new(provider),
        true,
    );

    let result = repl.run_turn("model-only request".into());

    assert!(result.is_err());
    assert_eq!(chat_calls.get(), 0);
    assert_eq!(stream_calls.get(), 0);
    assert_eq!(repl.convo.len(), 1);
    assert_eq!(repl.persisted, 1);
    assert!(output_bytes.borrow().is_empty());
}

#[test]
fn actual_repl_valid_history_without_session_is_pure_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        None,
        &mut output,
        Box::new(provider),
        true,
    );
    let original_history = repl.convo.clone();
    repl.diagnostics = diagnostics();

    let result = repl.run_turn("must not run".into());

    assert!(result.is_err());
    assert_eq!(chat_calls.get(), 0);
    assert_eq!(stream_calls.get(), 0);
    assert_eq!(
        serde_json::to_value(&repl.convo).unwrap(),
        serde_json::to_value(&original_history).unwrap()
    );
    assert_eq!(repl.persisted, 1);
    assert!(repl.diagnostics.is_none());
    assert!(repl.test_provider.is_some());
    assert!(output_bytes.borrow().is_empty());
}

#[test]
fn actual_repl_first_named_session_turn_archives_system_and_answer_before_output() {
    struct EmptyArchiveAwareOutput {
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        assistant_was_saved: bool,
    }
    impl Write for EmptyArchiveAwareOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let archived = SessionStore::new(&self.path).load().unwrap_or_default();
            self.assistant_was_saved |= archived.last().is_some_and(|message| {
                message.role == "assistant"
                    && message.content.as_deref() == Some("model-only answer")
            });
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let path = dir.path().join("first-turn.jsonl");
    let session = SessionStore::new(&path);
    assert!(session.load().unwrap().is_empty());
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let mut output = EmptyArchiveAwareOutput {
        path: path.clone(),
        bytes: Vec::new(),
        assistant_was_saved: false,
    };
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(provider),
        true,
    );
    repl.persisted = 0;

    let result = repl.run_turn("first user".into());

    assert!(result.is_ok());
    assert_eq!(chat_calls.get(), 1);
    assert_eq!(stream_calls.get(), 0);
    assert_eq!(repl.persisted, 3);
    assert_eq!(repl.convo.len(), 3);
    let archived = SessionStore::new(&path).load().unwrap();
    assert_eq!(archived.len(), 3);
    assert_eq!(archived[0].role, "system");
    assert_eq!(archived[0].content.as_deref(), Some("system"));
    assert_eq!(archived[1].role, "user");
    assert_eq!(archived[1].content.as_deref(), Some("first user"));
    assert_eq!(archived[2].role, "assistant");
    assert_eq!(archived[2].content.as_deref(), Some("model-only answer"));
    let expected_jsonl = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&Message::system("system")).unwrap(),
        serde_json::to_string(&Message::user("first user")).unwrap(),
        serde_json::to_string(&Message::assistant("model-only answer")).unwrap(),
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected_jsonl);
    drop(repl);
    assert!(output.assistant_was_saved);
    assert_eq!(output.bytes, b"model-only answer\n");
}

#[test]
fn actual_repl_selected_named_session_appends_before_output() {
    struct SessionAwareOutput {
        path: std::path::PathBuf,
        bytes: Vec<u8>,
        wrote_before_saved_assistant: bool,
    }
    impl Write for SessionAwareOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let saved = std::fs::read_to_string(&self.path).unwrap_or_default();
            if !saved.contains("model-only answer") {
                self.wrote_before_saved_assistant = true;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let path = dir.path().join("session.jsonl");
    let session = SessionStore::new(&path);
    session.append(&[Message::system("system")]).unwrap();
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let mut output = SessionAwareOutput {
        path: path.clone(),
        bytes: Vec::new(),
        wrote_before_saved_assistant: false,
    };
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(provider),
        true,
    );

    let result = repl.run_turn("model-only request".into());

    assert!(result.is_ok());
    assert_eq!(chat_calls.get(), 1);
    assert_eq!(stream_calls.get(), 0);
    let diagnostics = repl.diagnostics.as_ref().unwrap();
    assert_eq!(diagnostics.served_model.as_deref(), Some("synthetic-model"));
    assert_eq!(diagnostics.steps, Some(1));
    assert_eq!(diagnostics.usage.as_ref().unwrap().prompt_tokens, 11);
    assert_eq!(diagnostics.usage.as_ref().unwrap().completion_tokens, 7);
    assert_eq!(diagnostics.usage.as_ref().unwrap().total_tokens, 18);
    drop(repl);
    assert!(!output.wrote_before_saved_assistant);
    assert_eq!(output.bytes, b"model-only answer\n");
    let persisted = SessionStore::new(path).load().unwrap();
    assert_eq!(persisted.len(), 3);
    assert_eq!(persisted[0].content.as_deref(), Some("system"));
    assert_eq!(persisted[1].content.as_deref(), Some("model-only request"));
    assert_eq!(persisted[2].content.as_deref(), Some("model-only answer"));
}

#[test]
fn actual_repl_preflight_rejects_without_using_synthetic_provider() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let chat_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: chat_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        None,
        &mut output,
        Box::new(provider),
        true,
    );
    repl.convo.push(Message::tool("call", "result"));

    let result = repl.run_turn("rejected request".into());

    assert!(result.is_err());
    assert_eq!(chat_calls.get(), 0);
    assert_eq!(stream_calls.get(), 0);
    assert_eq!(repl.convo.len(), 2);
    assert_eq!(repl.persisted, 1);
    assert!(repl.diagnostics.is_none());
    assert!(output_bytes.borrow().is_empty());
}

#[test]
fn actual_repl_preflight_rejects_empty_or_structurally_unsupported_history() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let session = SessionStore::new(dir.path().join("history.jsonl"));
    session.append(&[Message::system("system")]).unwrap();
    let mut invalid_histories = Vec::new();
    invalid_histories.push(vec![Message::system("   ")]);
    invalid_histories.push(vec![Message::user("missing system")]);
    invalid_histories.push(vec![
        Message::system("system"),
        Message::tool("call", "output"),
    ]);
    invalid_histories.push(vec![
        Message::system("system"),
        Message::assistant_with_tool_calls(
            "",
            vec![crate::agent::providers::ToolCall {
                id: "call-1".into(),
                kind: "function".into(),
                function: crate::agent::providers::FunctionCall {
                    name: "x".into(),
                    arguments: "{}".into(),
                },
            }],
        ),
    ]);

    for history in invalid_histories {
        let chat_calls = Rc::new(Cell::new(0));
        let stream_calls = Rc::new(Cell::new(0));
        let provider = ReplFakeProvider {
            chat_calls: chat_calls.clone(),
            stream_calls: stream_calls.clone(),
        };
        let mut output = Vec::new();
        let mut repl = repl_for_test(
            &client,
            &creds,
            dir.path(),
            Some(SessionStore::new(session.path())),
            &mut output,
            Box::new(provider),
            true,
        );
        repl.convo = history;
        repl.persisted = 0;
        assert!(repl.run_turn("must not run".into()).is_err());
        assert_eq!(chat_calls.get(), 0);
        assert_eq!(stream_calls.get(), 0);
        assert_eq!(repl.persisted, 0);
        assert!(repl.diagnostics.is_none());
        drop(repl);
        assert!(output.is_empty());
        assert_eq!(session.load().unwrap().len(), 1);
    }
}

#[test]
fn actual_repl_provider_error_rolls_back_without_session_append() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let session = SessionStore::new(dir.path().join("session.jsonl"));
    session.append(&[Message::system("system")]).unwrap();
    let provider_calls = Rc::new(Cell::new(0));
    let provider = FailingReplProvider {
        calls: provider_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(provider),
        false,
    );

    let result = repl.run_turn("failing request".into());

    assert!(result.is_err());
    assert_eq!(provider_calls.get(), 1);
    assert_eq!(*output_bytes.borrow(), b"partial before error");
    assert_eq!(repl.convo.len(), 1);
    assert_eq!(repl.persisted, 1);
    assert!(repl.diagnostics.is_none());
    let persisted = repl.session.as_ref().unwrap().load().unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].role, "system");
}

#[test]
fn actual_repl_append_failure_keeps_answer_and_cursor_after_output() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"leave this file untouched").unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let provider_calls = Rc::new(Cell::new(0));
    let stream_calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: provider_calls.clone(),
        stream_calls: stream_calls.clone(),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(SessionStore::new(blocker.join("session.jsonl"))),
        &mut output,
        Box::new(provider),
        false,
    );

    let result = repl.run_turn("answer before append".into());

    assert!(result.is_err());
    assert_eq!(provider_calls.get(), 0);
    assert_eq!(stream_calls.get(), 1);
    assert_eq!(*output_bytes.borrow(), b"native answer\n");
    assert_eq!(repl.convo.len(), 3);
    assert_eq!(repl.convo[2].content.as_deref(), Some("native answer"));
    assert_eq!(repl.persisted, 1);
    assert!(repl.diagnostics.is_some());
    assert_eq!(
        std::fs::read(&blocker).unwrap(),
        b"leave this file untouched"
    );
    assert!(!blocker.join("session.jsonl").exists());
}

#[test]
fn actual_repl_resume_appends_only_the_new_turn_and_preserves_history_bytes() {
    struct CaptureProvider(Rc<RefCell<Option<ChatRequest>>>);
    impl LlmProvider for CaptureProvider {
        fn chat(&self, request: &ChatRequest) -> crate::error::Result<ChatResponse> {
            *self.0.borrow_mut() = Some(request.clone());
            Ok(ChatResponse {
                content: "resumed answer".into(),
                tool_calls: Vec::new(),
                model: Some("served-v2".into()),
                finish_reason: Some("stop".into()),
                truncated: false,
                usage: None,
            })
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
            _: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("selected route must use one non-stream request")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let path = dir.path().join("resume.jsonl");
    let session = SessionStore::new(&path);
    session
        .append(&[
            Message::system("  retained system  "),
            Message::user("old user"),
            Message::assistant("old answer"),
        ])
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let captured = Rc::new(RefCell::new(None));
    let mut output = Vec::new();
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(CaptureProvider(captured.clone())),
        true,
    );
    repl.convo = repl.session.as_ref().unwrap().load().unwrap();
    repl.persisted = repl.convo.len();

    repl.run_turn("new user".into()).unwrap();

    let seen = captured.borrow().clone().unwrap();
    assert_eq!(seen.model, "synthetic-model");
    assert_eq!(seen.messages.len(), 4);
    assert_eq!(
        seen.messages[0].content.as_deref(),
        Some("  retained system  ")
    );
    assert_eq!(seen.messages[1].content.as_deref(), Some("old user"));
    assert_eq!(seen.messages[2].content.as_deref(), Some("old answer"));
    assert_eq!(seen.messages[3].content.as_deref(), Some("new user"));
    let after = std::fs::read(&path).unwrap();
    assert!(after.starts_with(&before));
    let archived = SessionStore::new(&path).load().unwrap();
    assert_eq!(archived.len(), 5);
    assert_eq!(archived[3].content.as_deref(), Some("new user"));
    assert_eq!(archived[4].content.as_deref(), Some("resumed answer"));
    assert_eq!(repl.persisted, 5);
    drop(repl);
    assert_eq!(output, b"resumed answer\n");
}

#[test]
fn actual_repl_selected_append_error_poisons_until_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"blocker").unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let calls = Rc::new(Cell::new(0));
    let provider = ReplFakeProvider {
        chat_calls: calls.clone(),
        stream_calls: Rc::new(Cell::new(0)),
    };
    let output_bytes = Rc::new(RefCell::new(Vec::new()));
    let mut output = SharedOutput(output_bytes.clone());
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(SessionStore::new(blocker.join("session.jsonl"))),
        &mut output,
        Box::new(provider),
        true,
    );
    repl.persisted = 0;

    let first = repl.run_turn("will not commit".into());
    assert!(first.is_err());
    assert_eq!(calls.get(), 1);
    assert_eq!(repl.persisted, 0);
    assert_eq!(repl.convo.len(), 1);
    assert!(repl.selected_session_poisoned);
    assert!(repl.diagnostics.is_none());
    assert!(output_bytes.borrow().is_empty());

    let second = repl.run_turn("must be rejected".into());
    assert!(second.is_err());
    assert_eq!(calls.get(), 1);
    assert!(output_bytes.borrow().is_empty());
}

#[test]
fn selected_output_failure_keeps_the_saved_and_committed_turn() {
    struct FailingOutput {
        path: std::path::PathBuf,
        saw_saved: Rc<Cell<bool>>,
    }
    impl Write for FailingOutput {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            let saved = std::fs::read_to_string(&self.path).unwrap_or_default();
            self.saw_saved.set(saved.contains("model-only answer"));
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test writer failed",
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let path = dir.path().join("saved.jsonl");
    let session = SessionStore::new(&path);
    session.append(&[Message::system("system")]).unwrap();
    let saw_saved = Rc::new(Cell::new(false));
    let mut output = FailingOutput {
        path: path.clone(),
        saw_saved: saw_saved.clone(),
    };
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(session),
        &mut output,
        Box::new(ReplFakeProvider {
            chat_calls: Rc::new(Cell::new(0)),
            stream_calls: Rc::new(Cell::new(0)),
        }),
        true,
    );

    let result = repl.run_turn("saved before output".into());
    assert!(result.is_err());
    assert!(saw_saved.get());
    assert_eq!(repl.persisted, 3);
    assert_eq!(repl.convo.len(), 3);
    assert_eq!(repl.convo[2].content.as_deref(), Some("model-only answer"));
    assert!(repl.diagnostics.is_none());
    assert_eq!(SessionStore::new(path).load().unwrap().len(), 3);
}

#[test]
fn selected_provider_error_is_preserved_and_post_chat_cancel_skips_append() {
    struct ErrorProvider(Rc<Cell<usize>>);
    impl LlmProvider for ErrorProvider {
        fn chat(&self, _: &ChatRequest) -> crate::error::Result<ChatResponse> {
            self.0.set(self.0.get() + 1);
            Err(AppError::new("native provider sentinel"))
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
            _: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("selected route must not stream")
        }
    }
    struct CancelAfterChat {
        cancel: Cancel,
        calls: Rc<Cell<usize>>,
    }
    impl LlmProvider for CancelAfterChat {
        fn chat(&self, _: &ChatRequest) -> crate::error::Result<ChatResponse> {
            self.calls.set(self.calls.get() + 1);
            self.cancel.cancel();
            Ok(ChatResponse {
                content: "must not be saved".into(),
                tool_calls: Vec::new(),
                model: Some("synthetic-model".into()),
                finish_reason: Some("stop".into()),
                truncated: false,
                usage: None,
            })
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
            _: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("selected route must not stream")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new();
    let creds = Credentials::with_home(dir.path());
    let path = dir.path().join("provider-errors.jsonl");
    let session = SessionStore::new(&path);
    session.append(&[Message::system("system")]).unwrap();
    let calls = Rc::new(Cell::new(0));
    let mut output_bytes = Vec::new();
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(SessionStore::new(&path)),
        &mut output_bytes,
        Box::new(ErrorProvider(calls.clone())),
        true,
    );
    let error = repl.run_turn("provider fails".into()).unwrap_err();
    assert_eq!(error.to_string(), "native provider sentinel");
    assert_eq!(calls.get(), 1);
    assert_eq!(repl.persisted, 1);
    assert_eq!(SessionStore::new(&path).load().unwrap().len(), 1);
    drop(repl);
    assert!(output_bytes.is_empty());

    let cancel = Cancel::new();
    let cancel_calls = Rc::new(Cell::new(0));
    let mut output = Vec::new();
    let mut repl = repl_for_test(
        &client,
        &creds,
        dir.path(),
        Some(SessionStore::new(&path)),
        &mut output,
        Box::new(CancelAfterChat {
            cancel: cancel.clone(),
            calls: cancel_calls.clone(),
        }),
        true,
    );
    repl.cancel = cancel;
    let result = repl.run_turn("cancel after response".into());
    assert!(result.is_err());
    assert_eq!(cancel_calls.get(), 1);
    assert_eq!(repl.persisted, 1);
    assert_eq!(SessionStore::new(&path).load().unwrap().len(), 1);
    drop(repl);
    assert!(output.is_empty());
}

#[test]
fn native_adapter_preserves_model_history_max_tokens_and_response_metadata() {
    struct Capture {
        seen: Rc<RefCell<Option<(String, Vec<Message>, Option<i64>)>>>,
        calls: Rc<Cell<usize>>,
    }
    impl LlmProvider for Capture {
        fn chat(&self, request: &ChatRequest) -> crate::error::Result<ChatResponse> {
            self.calls.set(self.calls.get() + 1);
            *self.seen.borrow_mut() = Some((
                request.model.clone(),
                request.messages.clone(),
                request.max_tokens,
            ));
            Ok(ChatResponse {
                content: "answer with metadata".into(),
                tool_calls: Vec::new(),
                model: Some("provider-selected-model".into()),
                finish_reason: Some("length".into()),
                truncated: false,
                usage: Some(crate::agent::providers::TokenUsage {
                    prompt_tokens: 13,
                    completion_tokens: 17,
                    total_tokens: 30,
                }),
            })
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
            _: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("native adapter must make one non-stream call")
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let session = SessionStore::new(dir.path().join("metadata.jsonl"));
    let mut conversation = vec![
        Message::system("untrimmed guide "),
        Message::user("question"),
    ];
    let mut persisted = 0;
    let mut poisoned = false;
    let seen = Rc::new(RefCell::new(None));
    let calls = Rc::new(Cell::new(0));
    let request = ChatRequest {
        model: "requested-model".into(),
        messages: conversation.clone(),
        max_tokens: Some(77),
        ..Default::default()
    };

    let response = crate::agent::core_model_only::run_model_only(
        &Capture {
            seen: seen.clone(),
            calls: calls.clone(),
        },
        &request,
        &mut conversation,
        &mut persisted,
        Some(&session),
        &mut poisoned,
        &Cancel::new(),
    )
    .unwrap();

    assert_eq!(calls.get(), 1);
    let (model, history, max_tokens) = seen.borrow().clone().unwrap();
    assert_eq!(model, "requested-model");
    assert_eq!(max_tokens, Some(77));
    assert_eq!(history[0].content.as_deref(), Some("untrimmed guide "));
    assert_eq!(history[1].content.as_deref(), Some("question"));
    assert_eq!(response.model.as_deref(), Some("provider-selected-model"));
    assert_eq!(response.finish_reason.as_deref(), Some("length"));
    assert_eq!(response.usage.as_ref().unwrap().prompt_tokens, 13);
    assert_eq!(response.usage.as_ref().unwrap().completion_tokens, 17);
    assert_eq!(response.usage.as_ref().unwrap().total_tokens, 30);
    assert_eq!(persisted, 3);
    assert_eq!(SessionStore::new(session.path()).load().unwrap().len(), 3);
    assert!(!poisoned);
}

#[test]
fn selected_unsupported_provider_responses_are_not_saved_or_emitted() {
    struct Unsupported(bool);
    impl LlmProvider for Unsupported {
        fn chat(&self, _: &ChatRequest) -> crate::error::Result<ChatResponse> {
            Ok(ChatResponse {
                content: "partial".into(),
                tool_calls: if self.0 {
                    vec![crate::agent::providers::ToolCall {
                        id: "call".into(),
                        kind: "function".into(),
                        function: crate::agent::providers::FunctionCall {
                            name: "shell".into(),
                            arguments: "{}".into(),
                        },
                    }]
                } else {
                    Vec::new()
                },
                model: Some("synthetic-model".into()),
                finish_reason: Some("stop".into()),
                truncated: !self.0,
                usage: None,
            })
        }
        fn chat_stream(
            &self,
            _: &ChatRequest,
            _: &mut dyn FnMut(&str),
        ) -> crate::error::Result<ChatResponse> {
            panic!("selected route must not stream")
        }
    }

    for tool_calls in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new();
        let creds = Credentials::with_home(dir.path());
        let path = dir.path().join("unsupported.jsonl");
        let session = SessionStore::new(&path);
        session.append(&[Message::system("system")]).unwrap();
        let mut output = Vec::new();
        let mut repl = repl_for_test(
            &client,
            &creds,
            dir.path(),
            Some(session),
            &mut output,
            Box::new(Unsupported(tool_calls)),
            true,
        );
        assert!(repl.run_turn("unsupported response".into()).is_err());
        assert_eq!(repl.persisted, 1);
        assert_eq!(repl.convo.len(), 1);
        assert!(repl.diagnostics.is_none());
        assert_eq!(SessionStore::new(&path).load().unwrap().len(), 1);
        drop(repl);
        assert!(output.is_empty());
    }
}
