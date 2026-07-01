mod common;

use serde_json::{json, Value};

use common::stub;
use monocle_cli::agent::providers::{Message, MonocleProvider};
use monocle_cli::agent::runner::{Agent, AgentConfig, AllowAll, Approver, Observer, Silent};
use monocle_cli::agent::tools::{ToolContext, ToolRegistry};

/// Stateful stub LLM: first turn requests a `write_file` tool call; once it sees
/// a tool-result message in the conversation, it returns a final answer.
fn write_then_finish_stub() -> common::Stub {
    stub(|_addr, _method, url, body| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        let req: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
        let seen_tool_result = req["messages"]
            .as_array()
            .map(|ms| ms.iter().any(|m| m["role"] == "tool"))
            .unwrap_or(false);

        let resp = if seen_tool_result {
            json!({"choices":[{"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]})
        } else {
            let args = serde_json::to_string(&json!({"path":"out.txt","content":"hi"})).unwrap();
            json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[
                {"id":"call_1","type":"function","function":{"name":"write_file","arguments": args}}
            ]},"finish_reason":"tool_calls"}]})
        };
        (200, resp.to_string())
    })
}

#[test]
fn loop_executes_tool_then_returns_final_answer() {
    let s = write_then_finish_stub();
    let provider = MonocleProvider::new("tok", s.router_url());
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        &provider,
        &tools,
        ToolContext::new(dir.path()),
        AgentConfig::new("any"),
    );

    // Observer records which tools were called.
    #[derive(Default)]
    struct Rec {
        calls: Vec<String>,
    }
    impl Observer for Rec {
        fn on_tool_call(&mut self, name: &str, _args: &Value) {
            self.calls.push(name.to_string());
        }
    }
    let mut rec = Rec::default();

    let answer = agent
        .run(
            &mut vec![
                Message::system("be helpful"),
                Message::user("write out.txt"),
            ],
            &mut AllowAll,
            &mut rec,
        )
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
    let s = write_then_finish_stub();
    let provider = MonocleProvider::new("tok", s.router_url());
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        &provider,
        &tools,
        ToolContext::new(dir.path()),
        AgentConfig::new("any"),
    );

    struct DenyAll;
    impl Approver for DenyAll {
        fn approve(&mut self, _name: &str, _args: &Value) -> bool {
            false
        }
    }

    let answer = agent
        .run(
            &mut vec![Message::user("write out.txt")],
            &mut DenyAll,
            &mut Silent,
        )
        .unwrap();

    // The model still finishes (it sees the denial as a tool result), but nothing
    // was written.
    assert_eq!(answer, "done");
    assert!(
        !dir.path().join("out.txt").exists(),
        "denied write must not touch disk"
    );
}

#[test]
fn loop_stops_at_max_steps() {
    // Stub that ALWAYS asks for another (read-only) tool call → never finishes.
    let s = stub(|_a, _m, url, _b| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        let args = serde_json::to_string(&json!({"path":"whatever"})).unwrap();
        (
            200,
            json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[
                {"id":"c","type":"function","function":{"name":"read_file","arguments": args}}
            ]},"finish_reason":"tool_calls"}]})
            .to_string(),
        )
    });
    let provider = MonocleProvider::new("tok", s.router_url());
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let mut config = AgentConfig::new("any");
    config.max_steps = 3;
    let agent = Agent::new(&provider, &tools, ToolContext::new(dir.path()), config);

    // Graceful stop: returns a notice (not a hard error) so partial work isn't lost.
    let answer = agent
        .run(
            &mut vec![Message::user("loop forever")],
            &mut AllowAll,
            &mut Silent,
        )
        .unwrap();
    assert!(answer.contains("stopped after 3 steps"), "got: {answer}");
}

#[test]
fn conversation_persists_across_turns() {
    // No-tool stub whose answer reports how many `user` messages it saw — proof
    // the conversation carries forward between `run` calls (multi-turn).
    let s = stub(|_a, _m, url, body| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        let req: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
        let users = req["messages"]
            .as_array()
            .map(|ms| ms.iter().filter(|m| m["role"] == "user").count())
            .unwrap_or(0);
        (
            200,
            json!({"choices":[{"message":{"role":"assistant","content":format!("seen {users}")},"finish_reason":"stop"}]}).to_string(),
        )
    });
    let provider = MonocleProvider::new("tok", s.router_url());
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(&provider, &tools, ToolContext::new(dir.path()), AgentConfig::new("any"));

    let mut convo = vec![Message::system("s"), Message::user("first")];
    let a1 = agent.run(&mut convo, &mut AllowAll, &mut Silent).unwrap();
    assert_eq!(a1, "seen 1");

    // `run` appended the assistant answer; a follow-up turn sees both users.
    convo.push(Message::user("second"));
    let a2 = agent.run(&mut convo, &mut AllowAll, &mut Silent).unwrap();
    assert_eq!(a2, "seen 2");

    // system, user, assistant, user, assistant
    assert_eq!(convo.len(), 5);
}

#[test]
fn malformed_tool_args_are_reported_to_model_not_swallowed() {
    // First turn: a tool call whose `arguments` is not valid JSON. Once the model
    // sees the error tool-result, it recovers with a final answer.
    let s = stub(|_a, _m, url, body| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        let req: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
        let seen_tool = req["messages"]
            .as_array()
            .map(|ms| ms.iter().any(|m| m["role"] == "tool"))
            .unwrap_or(false);
        if seen_tool {
            (200, json!({"choices":[{"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}]}).to_string())
        } else {
            (200, json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[
                {"id":"c","type":"function","function":{"name":"write_file","arguments":"{not json"}}
            ]},"finish_reason":"tool_calls"}]}).to_string())
        }
    });
    let provider = MonocleProvider::new("tok", s.router_url());
    let tools = ToolRegistry::with_defaults();
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        &provider,
        &tools,
        ToolContext::new(dir.path()),
        AgentConfig::new("any"),
    );

    let answer = agent
        .run(
            &mut vec![Message::user("do it")],
            &mut AllowAll,
            &mut Silent,
        )
        .unwrap();
    assert_eq!(answer, "recovered");
}
