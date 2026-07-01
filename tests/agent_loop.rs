mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use common::{
    has_tool_result, text_response, tool_call_response, tool_call_response_raw, FakeProvider,
};
use monocle_cli::agent::providers::Message;
use monocle_cli::agent::runner::{
    Agent, AgentConfig, AllowAll, Approver, Cancel, Observer, Silent,
};
use monocle_cli::agent::tools::{ToolContext, ToolRegistry};

// These tests exercise the loop IN ISOLATION via a scripted `FakeProvider` — no
// HTTP, no wire format. The wire (Chat Completions / SSE) is covered separately
// by `agent_providers` / `agent_streaming`.

fn agent<'a>(
    provider: &'a FakeProvider,
    tools: &'a ToolRegistry,
    dir: &std::path::Path,
    max_steps: usize,
) -> Agent<'a, FakeProvider> {
    let mut config = AgentConfig::new("any");
    config.max_steps = max_steps;
    Agent::new(provider, tools, ToolContext::new(dir), config)
}

#[test]
fn loop_executes_tool_then_returns_final_answer() {
    let provider = FakeProvider::new(|req| {
        if has_tool_result(req) {
            text_response("done")
        } else {
            tool_call_response(
                "call_1",
                "write_file",
                json!({"path":"out.txt","content":"hi"}),
            )
        }
    });
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 20);

    #[derive(Default)]
    struct Rec {
        calls: Vec<String>,
    }
    impl Observer for Rec {
        fn on_tool_call(&mut self, _id: &str, name: &str, _args: &Value) {
            self.calls.push(name.to_string());
        }
    }
    let mut rec = Rec::default();

    let mut convo = vec![
        Message::system("be helpful"),
        Message::user("write out.txt"),
    ];
    let answer = agent
        .run(&mut convo, &mut AllowAll, &mut rec, &Cancel::new())
        .unwrap();

    assert_eq!(answer, "done");
    assert_eq!(rec.calls, vec!["write_file"]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "hi"
    );
}

#[test]
fn denied_side_effecting_tool_is_not_executed() {
    let provider = FakeProvider::new(|req| {
        if has_tool_result(req) {
            text_response("done")
        } else {
            tool_call_response("c", "write_file", json!({"path":"out.txt","content":"hi"}))
        }
    });
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 20);

    struct DenyAll;
    impl Approver for DenyAll {
        fn approve(&mut self, _id: &str, _name: &str, _args: &Value) -> bool {
            false
        }
    }

    let answer = agent
        .run(
            &mut vec![Message::user("write out.txt")],
            &mut DenyAll,
            &mut Silent,
            &Cancel::new(),
        )
        .unwrap();

    assert_eq!(answer, "done");
    assert!(
        !dir.path().join("out.txt").exists(),
        "denied write must not touch disk"
    );
}

#[test]
fn loop_stops_at_max_steps() {
    // Always asks for another (read-only) tool call → never finishes.
    let provider =
        FakeProvider::new(|_| tool_call_response("c", "read_file", json!({"path":"whatever"})));
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 3);

    let answer = agent
        .run(
            &mut vec![Message::user("loop forever")],
            &mut AllowAll,
            &mut Silent,
            &Cancel::new(),
        )
        .unwrap();
    assert!(answer.contains("stopped after 3 steps"), "got: {answer}");
}

#[test]
fn conversation_persists_across_turns() {
    // Answer reports how many `user` messages it saw — proof the conversation
    // carries forward between `run` calls (multi-turn).
    let provider = FakeProvider::new(|req| {
        let users = req.messages.iter().filter(|m| m.role == "user").count();
        text_response(&format!("seen {users}"))
    });
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 20);

    let mut convo = vec![Message::system("s"), Message::user("first")];
    let a1 = agent
        .run(&mut convo, &mut AllowAll, &mut Silent, &Cancel::new())
        .unwrap();
    assert_eq!(a1, "seen 1");

    convo.push(Message::user("second"));
    let a2 = agent
        .run(&mut convo, &mut AllowAll, &mut Silent, &Cancel::new())
        .unwrap();
    assert_eq!(a2, "seen 2");

    // system, user, assistant, user, assistant
    assert_eq!(convo.len(), 5);
}

#[test]
fn malformed_tool_args_are_reported_to_model_not_swallowed() {
    let provider = FakeProvider::new(|req| {
        if has_tool_result(req) {
            text_response("recovered")
        } else {
            tool_call_response_raw("c", "write_file", "{not json")
        }
    });
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 20);

    let answer = agent
        .run(
            &mut vec![Message::user("do it")],
            &mut AllowAll,
            &mut Silent,
            &Cancel::new(),
        )
        .unwrap();
    assert_eq!(answer, "recovered");
}

#[test]
fn loop_can_be_cancelled_mid_run() {
    // The fake cancels after its first response; the loop must stop at the next
    // step boundary rather than looping forever.
    let cancel = Cancel::new();
    let trigger = cancel.clone();
    let calls = AtomicUsize::new(0);
    let provider = FakeProvider::new(move |_| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            trigger.cancel();
        }
        tool_call_response("c", "read_file", json!({"path":"x"}))
    });
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = agent(&provider, &tools, dir.path(), 100);

    let answer = agent
        .run(
            &mut vec![Message::user("go")],
            &mut AllowAll,
            &mut Silent,
            &cancel,
        )
        .unwrap();
    assert!(answer.contains("cancelled"), "got: {answer}");
}
