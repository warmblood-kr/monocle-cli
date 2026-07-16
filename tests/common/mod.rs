//! A tiny synchronous stub HTTP server for exercising the `net` facade end to
//! end without mocking reqwest. Mirrors the `deps.fetch` injection the TS tests
//! used, but with a real loopback server (keeps everything sync).
//!
//! (Each test binary compiles its own copy and uses a different subset, so
//! unused helpers here are expected.)
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;
use tiny_http::{Response, Server};

use monocle_cli::agent::providers::{
    ChatRequest, ChatResponse, FunctionCall, LlmProvider, ToolCall,
};
use monocle_cli::agent::runner::{Cancel, Observer};
use monocle_cli::agent::tools::{FsBackend, ShellExec, ShellResult, ToolOutcome};
use monocle_cli::error::Result;

// ── mock LLM (isolated loop testing — no network) ───────────────────────────

/// A scripted `LlmProvider` for isolated loop/orchestration tests. The closure
/// inspects the request (e.g. whether a tool result is already present) and
/// returns the next response — no HTTP, deterministic.
pub struct FakeProvider {
    responder: Box<dyn Fn(&ChatRequest) -> ChatResponse + Send + Sync>,
}

impl FakeProvider {
    pub fn new(f: impl Fn(&ChatRequest) -> ChatResponse + Send + Sync + 'static) -> Self {
        Self {
            responder: Box::new(f),
        }
    }
}

impl LlmProvider for FakeProvider {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        Ok((self.responder)(req))
    }
    // chat_stream uses the trait default (delegates to chat, emits content once),
    // which is exactly what isolated loop tests want.
}

/// A final text answer (no tool calls).
pub fn text_response(content: &str) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        tool_calls: vec![],
        model: None,
        finish_reason: Some("stop".to_string()),
        truncated: false,
        usage: None,
    }
}

/// A response that requests one tool call with the given JSON-encoded args.
pub fn tool_call_response(id: &str, name: &str, args: Value) -> ChatResponse {
    tool_call_response_raw(id, name, &args.to_string())
}

/// Like [`tool_call_response`] but with a raw `arguments` string (e.g. to script
/// malformed JSON).
pub fn tool_call_response_raw(id: &str, name: &str, arguments: &str) -> ChatResponse {
    ChatResponse {
        content: String::new(),
        tool_calls: vec![ToolCall {
            id: id.to_string(),
            kind: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }],
        model: None,
        finish_reason: Some("tool_calls".to_string()),
        truncated: false,
        usage: None,
    }
}

/// Whether the conversation already contains a tool-result message (a common
/// signal for "second turn" in stateful fakes).
pub fn has_tool_result(req: &ChatRequest) -> bool {
    req.messages.iter().any(|m| m.role == "tool")
}

pub struct Stub {
    /// `host:port` — feed as a tenant domain (resolves to http on 127.0.0.1) or
    /// prefix with `http://` for a router URL.
    pub addr: String,
}

impl Stub {
    pub fn router_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Spawn a stub server. `handler(addr, method, url, body) -> (status, body)`,
/// where `addr` is this server's own `host:port` (for building absolute
/// endpoint URLs in discovery docs). The thread runs until the process exits.
pub fn stub<F>(handler: F) -> Stub
where
    F: Fn(&str, &str, &str, &str) -> (u16, String) + Send + 'static,
{
    let server = Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let addr_for_thread = addr.clone();
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let method = req.method().as_str().to_string();
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, resp_body) = handler(&addr_for_thread, &method, &url, &body);
            let _ = req.respond(Response::from_string(resp_body).with_status_code(status));
        }
    });
    Stub { addr }
}

/// Like [`stub`] but responds with `Content-Type: text/event-stream` so the
/// provider takes its SSE-parsing path. Handler returns the full SSE body.
pub fn stub_sse<F>(handler: F) -> Stub
where
    F: Fn(&str, &str, &str, &str) -> (u16, String) + Send + 'static,
{
    let server = Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let addr_for_thread = addr.clone();
    thread::spawn(move || {
        let ct =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap();
        for mut req in server.incoming_requests() {
            let method = req.method().as_str().to_string();
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, resp_body) = handler(&addr_for_thread, &method, &url, &body);
            let _ = req.respond(
                Response::from_string(resp_body)
                    .with_status_code(status)
                    .with_header(ct.clone()),
            );
        }
    });
    Stub { addr }
}

// ── tool-layer virtualization testkit ────────────────────────────────────────
//
// The same seam the ACP surface uses to inject a client-mediated shell/fs (see
// `tools::ToolContext::with_shell`/`with_fs`) lets a test swap in *mock* backends:
// capture the exact commands / file writes the agent makes and assert on them,
// with NO real process spawn and NO disk I/O. Paired with `FakeProvider` (scripts
// the model) and `RecordingObserver` (captures the loop's callbacks), a whole
// tool-calling loop runs deterministically. Mirrors `FakeProvider`'s Arc/closure
// style. See `tests/agent_virtual_tools.rs` for the end-to-end demonstrator.

/// A mock [`ShellExec`] that records every command it's asked to run and serves a
/// canned [`ShellResult`] — never spawns a real process. Wrap in `Arc` and keep a
/// clone (`Arc<RecordingShell>` coerces to `Arc<dyn ShellExec>` at `with_shell`)
/// to read [`commands`](RecordingShell::commands) back after the run.
pub struct RecordingShell {
    commands: Arc<Mutex<Vec<String>>>,
    stdout: String,
    exit_code: Option<i32>,
}

impl RecordingShell {
    /// Records commands; every `exec` returns success with empty output.
    pub fn new() -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            stdout: String::new(),
            exit_code: Some(0),
        }
    }

    /// Like [`new`](RecordingShell::new) but serves canned stdout + exit code.
    pub fn with_output(stdout: impl Into<String>, exit_code: i32) -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            stdout: stdout.into(),
            exit_code: Some(exit_code),
        }
    }

    /// The commands passed to `exec`, in order — for assertions.
    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }
}

impl Default for RecordingShell {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellExec for RecordingShell {
    fn exec(&self, command: &str, _cwd: &Path, _cancel: &Cancel) -> ShellResult {
        self.commands.lock().unwrap().push(command.to_string());
        ShellResult {
            output: self.stdout.clone(),
            exit_code: self.exit_code,
            cancelled: false,
        }
    }
}

/// A mock [`FsBackend`] backed by an in-memory `HashMap<PathBuf, String>` — the
/// read/write/edit tools hit this instead of disk. Files are keyed by the tool's
/// *resolved* absolute path (`workdir.join(path)`), so seed / assert with the same
/// resolved path. Wrap in `Arc` and keep a clone to inspect it after the run.
pub struct InMemoryFs {
    files: Arc<Mutex<HashMap<PathBuf, String>>>,
    writes: Arc<Mutex<Vec<PathBuf>>>,
}

impl InMemoryFs {
    /// Empty filesystem.
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
            writes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Pre-seed files (each `(resolved-path, contents)`).
    pub fn seeded(entries: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        let fs = Self::new();
        {
            let mut files = fs.files.lock().unwrap();
            for (path, content) in entries {
                files.insert(path, content);
            }
        }
        fs
    }

    /// Current contents of `path` (the resolved absolute path), if present.
    pub fn get(&self, path: &Path) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }

    /// The paths written via `write`, in order — for assertions.
    pub fn writes(&self) -> Vec<PathBuf> {
        self.writes.lock().unwrap().clone()
    }
}

impl Default for InMemoryFs {
    fn default() -> Self {
        Self::new()
    }
}

impl FsBackend for InMemoryFs {
    fn read(&self, path: &Path) -> std::result::Result<String, String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            // Same shape the trait expects for a missing file: a human-readable
            // String error the tool drops into `ToolOutcome::error`.
            .ok_or_else(|| format!("{}: no such file (in-memory)", path.display()))
    }

    fn write(&self, path: &Path, content: &str) -> std::result::Result<(), String> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), content.to_string());
        self.writes.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// An [`Observer`] that records a compact string for each loop callback, so a test
/// can assert exactly which tool calls the agent made and in what order:
/// - `call:<name>:<args-compact-json>`
/// - `result:<name>:<is_error>`
/// - `text:<delta>` · `notice:<msg>` · `step` (one per `on_turn_step` call — once
///   per LLM call the turn makes)
#[derive(Default)]
pub struct RecordingObserver {
    events: Vec<String>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded events, in order — for assertions.
    pub fn events(&self) -> Vec<String> {
        self.events.clone()
    }
}

impl Observer for RecordingObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.events.push(format!("text:{delta}"));
    }
    fn on_tool_call(&mut self, _id: &str, name: &str, args: &Value) {
        self.events.push(format!("call:{name}:{args}"));
    }
    fn on_tool_result(&mut self, _id: &str, name: &str, outcome: &ToolOutcome) {
        self.events
            .push(format!("result:{name}:{}", outcome.is_error));
    }
    fn on_notice(&mut self, msg: &str) {
        self.events.push(format!("notice:{msg}"));
    }
    fn on_turn_step(&mut self, _resp: &ChatResponse) {
        self.events.push("step".to_string());
    }
}
