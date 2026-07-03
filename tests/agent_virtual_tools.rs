//! Tool-layer virtualization — the whole agent tool-calling loop, deterministic
//! and side-effect-free.
//!
//! The pattern (three seams, one harness):
//! - **`FakeProvider` scripts the model.** Each turn the closure inspects the
//!   conversation and returns the next `ChatResponse` — a tool call, then another,
//!   then a final text answer — with no HTTP.
//! - **Mock backends capture/serve tool I/O.** `ToolRegistry::with_defaults()` is
//!   the REAL shell + read/write/edit tools; only their *backends* are swapped via
//!   the same `ToolContext::with_shell`/`with_fs` seam the ACP surface uses. A
//!   `RecordingShell` records the exact command the LLM asked to run (no process
//!   spawns); an `InMemoryFs` serves/captures file I/O (no disk touches).
//! - **`RecordingObserver` asserts the calls.** It records `call:`/`result:` for
//!   every tool the loop drove, so we can assert *what tool calls the agent made*
//!   and in what order.
//!
//! Swap `FakeProvider` for a real `MonocleProvider` and the SAME harness becomes a
//! MODEL tool-calling eval: the mock backends still capture what the model chose to
//! do (which commands, which writes) and the observer still records the call
//! sequence — you just assert on the model's behavior instead of a scripted one.
//! (No network test is added here; this is the documented extension point.)

mod common;

use serde_json::json;

use common::{
    has_tool_result, text_response, tool_call_response, FakeProvider, InMemoryFs,
    RecordingObserver, RecordingShell,
};
use monocle_cli::agent::providers::Message;
use monocle_cli::agent::runner::{Agent, AgentConfig, AllowAll, Cancel, RunStop};
use monocle_cli::agent::tools::{Shell, Tool, ToolContext, ToolRegistry};
use std::sync::Arc;

/// The shell tool's platform-specific name (`bash` on unix, `powershell` on
/// Windows) — the model must call it by this name for the call to route.
fn shell_tool_name() -> String {
    Shell.name().to_string()
}

#[test]
fn virtualized_loop_routes_tool_calls_to_mock_backends() {
    let shell_name = shell_tool_name();

    // ── the model: shell `echo hi` → write_file out.txt → final answer ──────
    let shell_name_for_provider = shell_name.clone();
    let provider = FakeProvider::new(move |req| {
        // Count how many tool results the conversation already carries to decide
        // which scripted step we're on (0 → shell, 1 → write, 2 → done).
        let done = req.messages.iter().filter(|m| m.role == "tool").count();
        match done {
            0 => tool_call_response(
                "c1",
                &shell_name_for_provider,
                json!({"command": "echo hi"}),
            ),
            1 => tool_call_response(
                "c2",
                "write_file",
                json!({"path": "out.txt", "content": "data"}),
            ),
            _ => text_response("all done"),
        }
    });

    // ── mock backends (kept as Arc clones for post-run assertions) ──────────
    let shell = Arc::new(RecordingShell::new());
    let fs = Arc::new(InMemoryFs::new());
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path())
        .with_shell(shell.clone())
        .with_fs(fs.clone());

    let tools = ToolRegistry::with_defaults();
    let agent = Agent::new(&provider, &tools, ctx, AgentConfig::new("any"));

    let mut observer = RecordingObserver::new();
    let stop = agent
        .run(
            &mut vec![Message::user("do the thing")],
            &mut AllowAll,
            &mut observer,
            &Cancel::new(),
        )
        .unwrap();

    // (d) normal completion.
    assert!(matches!(stop, RunStop::EndTurn), "expected EndTurn");

    // (a) the LLM's shell call reached the MOCK shell — not real bash.
    assert_eq!(
        shell.commands(),
        vec!["echo hi".to_string()],
        "the exact command the model asked for must reach the mock shell"
    );

    // (b) the write landed in the in-memory fs — no real file on disk. The tool
    // resolves `out.txt` against the workdir, so key/assert on the resolved path.
    let resolved = dir.path().join("out.txt");
    assert_eq!(fs.get(&resolved).as_deref(), Some("data"));
    assert_eq!(fs.writes(), vec![resolved.clone()]);
    assert!(
        !resolved.exists(),
        "virtualized write must NOT touch the real disk"
    );

    // (c) the observer saw the tool calls in order, each followed by its result.
    let events = observer.events();
    let calls_and_results: Vec<String> = events
        .into_iter()
        .filter(|e| e.starts_with("call:") || e.starts_with("result:"))
        .collect();
    assert_eq!(
        calls_and_results,
        vec![
            format!("call:{shell_name}:{{\"command\":\"echo hi\"}}"),
            format!("result:{shell_name}:false"),
            "call:write_file:{\"path\":\"out.txt\",\"content\":\"data\"}".to_string(),
            "result:write_file:false".to_string(),
        ],
        "the agent's tool calls must appear in order, each with its result"
    );
}

#[test]
fn virtualized_read_of_missing_file_surfaces_a_tool_error() {
    // Error path: the model reads a file the in-memory fs doesn't have. The tool
    // must surface an error (is_error) to the observer AND feed it back to the
    // model, which then answers — the failure is reported, not swallowed.
    let provider = FakeProvider::new(|req| {
        if has_tool_result(req) {
            text_response("could not read it")
        } else {
            tool_call_response("r1", "read_file", json!({"path": "missing.txt"}))
        }
    });

    let fs = Arc::new(InMemoryFs::new()); // empty — the read must miss
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path()).with_fs(fs.clone());

    let tools = ToolRegistry::with_defaults();
    let agent = Agent::new(&provider, &tools, ctx, AgentConfig::new("any"));

    let mut observer = RecordingObserver::new();
    let stop = agent
        .run(
            &mut vec![Message::user("read missing.txt")],
            &mut AllowAll,
            &mut observer,
            &Cancel::new(),
        )
        .unwrap();

    assert!(matches!(stop, RunStop::EndTurn));

    // The tool result was flagged as an error to the observer.
    let events = observer.events();
    assert!(
        events.contains(&"result:read_file:true".to_string()),
        "missing-file read must surface as a tool error: {events:?}"
    );
    // And the model got to answer after the error was fed back.
    assert!(events.iter().any(|e| e == "text:could not read it"));
}
