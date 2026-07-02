//! ACP server-agent surface (Path B, Phase 3) — lets an ACP client (editor,
//! monocle desktop, Craft) spawn `monocle acp` and drive our agent over stdio
//! JSON-RPC (Zed's Agent Client Protocol).
//!
//! **Architecture / migration firewall:** this module is the ONLY place that
//! touches the `agent-client-protocol` crate AND async/tokio. The agent-core
//! loop stays synchronous; each prompt runs it on `spawn_blocking` and bridges
//! streamed updates back to the async ACP connection via a channel. The crate is
//! a swappable adapter — pinning 0.9 vs migrating to 1.0 is a change to this file
//! only (the ACP *wire protocol* is the stable contract, not the crate API).
//!
//! Scope: initialize / new_session / prompt / cancel; tool file I/O is routed
//! through the client when it advertises `fs` capabilities (editor-mediated
//! read/write), else runs on local disk; tool permission is delegated to the
//! client via `session/request_permission`; tool calls stream as correlated
//! `ToolCall`/`ToolCallUpdate` lifecycle updates. Follow-ups: client terminal
//! callbacks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use agent_client_protocol::{self as acp, Agent, Client as _};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::agent::providers::{Message, MonocleProvider};
use crate::agent::runner::{Agent as CoreAgent, Approver, Cancel, Observer, RunStop};
use crate::agent::tools::{FsBackend, ToolContext, ToolOutcome, ToolRegistry};
use crate::agent::{DEFAULT_MAX_STEPS, DEFAULT_MODEL, SYSTEM_PROMPT};
use crate::auth::try_access_token;
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::net::Client;

/// Permission option ids sent to the client on `session/request_permission` and
/// matched back on its answer. One symbol tying construct↔compare, so a rename
/// can't silently break approvals.
const PERM_ALLOW: &str = "allow";
const PERM_REJECT: &str = "reject";

/// A call the (blocking) agent loop needs the async ACP connection to make on
/// its behalf — a fire-and-forget update, a permission round-trip, or a
/// client-mediated filesystem read/write (when the client advertises fs caps).
enum ClientCall {
    Notify(acp::SessionNotification),
    Permission(
        acp::RequestPermissionRequest,
        oneshot::Sender<acp::RequestPermissionOutcome>,
    ),
    ReadFile(
        acp::ReadTextFileRequest,
        oneshot::Sender<std::result::Result<String, String>>,
    ),
    WriteFile(
        acp::WriteTextFileRequest,
        oneshot::Sender<std::result::Result<(), String>>,
    ),
}

/// Per-ACP-session state, keyed by session id string.
struct SessionState {
    convo: Vec<Message>,
    cwd: PathBuf,
    cancel: Cancel,
    model: String,
    /// True while a `prompt()` turn is executing for this session. Guards against
    /// concurrent prompts corrupting the `mem::take`-n conversation (see Fix #8).
    running: bool,
}

/// The model id for a session: the client's `_meta["monocle.model"]` if present
/// (a string), else `DEFAULT_MODEL`. The id is still routed/validated by monocle
/// chat-proxy — we only pass it through. Any fallback (meta absent, key absent,
/// or key not a string) yields `DEFAULT_MODEL`.
fn session_model(meta: &Option<acp::Meta>) -> String {
    meta.as_ref()
        .and_then(|m| m.get("monocle.model"))
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL)
        .to_string()
}

struct MonocleAgent {
    /// Outbound `session/update` notifications → forwarded to the connection.
    updates: mpsc::UnboundedSender<ClientCall>,
    sessions: RefCell<HashMap<String, SessionState>>,
    next_id: AtomicU64,
    /// Capabilities the client advertised at `initialize` — notably whether it
    /// can serve `fs/read_text_file` + `fs/write_text_file`, which gates whether
    /// tool file I/O is routed through the client (editor-mediated) or local disk.
    client_caps: RefCell<acp::ClientCapabilities>,
}

impl MonocleAgent {
    fn new(updates: mpsc::UnboundedSender<ClientCall>) -> Self {
        Self {
            updates,
            sessions: RefCell::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            client_caps: RefCell::new(acp::ClientCapabilities::default()),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl Agent for MonocleAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        // Remember what the client can do (fs read/write) so `prompt()` can route
        // tool file I/O through the client when it advertises those capabilities.
        *self.client_caps.borrow_mut() = args.client_capabilities.clone();
        Ok(acp::InitializeResponse::new(args.protocol_version)
            .agent_capabilities(acp::AgentCapabilities::new()))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        // We authenticate out-of-band (stored monocle credentials), so no ACP auth.
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let n = self.next_id.fetch_add(1, Ordering::SeqCst);
        let id = acp::SessionId::new(format!("monocle-{n}"));
        self.sessions.borrow_mut().insert(
            id.0.to_string(),
            SessionState {
                convo: vec![Message::system(SYSTEM_PROMPT)],
                cwd: args.cwd,
                cancel: Cancel::new(),
                model: session_model(&args.meta),
                running: false,
            },
        );
        Ok(acp::NewSessionResponse::new(id))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let key = args.session_id.0.to_string();

        // Claim the session and MOVE its conversation out for the turn (leaving an
        // empty Vec behind). Reject an overlapping prompt for the same session —
        // two turns sharing the mem::take-n state would corrupt it (Fix #8). Keep
        // this borrow short; it is released before the first await.
        let (convo, cwd, cancel, model) = {
            let mut sessions = self.sessions.borrow_mut();
            let st = sessions
                .get_mut(&key)
                .ok_or_else(acp::Error::invalid_params)?;
            if st.running {
                return Err(acp::Error::invalid_params()); // session busy
            }
            st.running = true;
            st.cancel.reset();
            (
                std::mem::take(&mut st.convo),
                st.cwd.clone(),
                st.cancel.clone(),
                st.model.clone(),
            )
        };

        let user = prompt_text(&args.prompt);
        let updates = self.updates.clone();
        let sid = args.session_id.clone();

        // Route tool file I/O through the client only when it advertises BOTH fs
        // read and write; otherwise fall back to local disk (default LocalFs). Read
        // the caps here (brief borrow) so the blocking closure captures plain data.
        let use_client_fs = {
            let caps = self.client_caps.borrow();
            caps.fs.read_text_file && caps.fs.write_text_file
        };
        let fs_calls = self.updates.clone();
        let fs_sid = args.session_id.clone();

        // Run the SYNC agent-core loop off the async thread; stream updates back,
        // and route side-effecting tool permission to the client (the editor
        // decides via session/request_permission). The user message is pushed
        // INSIDE the closure, only after auth succeeds, so an auth failure leaves
        // the conversation exactly as it was.
        let joined = tokio::task::spawn_blocking(move || {
            let mut convo = convo;
            let mut observer = AcpObserver {
                updates: updates.clone(),
                sid: sid.clone(),
            };
            let mut approver = AcpApprover {
                calls: updates,
                sid,
                cancel: cancel.clone(),
            };
            let session = match try_access_token(&Client::new(), &Credentials::new()) {
                Ok(s) => s,
                Err(e) => return (convo, Err(e)),
            };
            convo.push(Message::user(user));
            let provider = MonocleProvider::from_session(session);
            let tools = ToolRegistry::with_defaults();
            let mut ctx = ToolContext::new(cwd).with_cancel(cancel.clone());
            if use_client_fs {
                ctx = ctx.with_fs(Arc::new(AcpClientFs {
                    calls: fs_calls,
                    sid: fs_sid,
                }));
            }
            let agent = CoreAgent::with_max_steps(&provider, &tools, ctx, model, DEFAULT_MAX_STEPS);
            let result = agent.run(&mut convo, &mut approver, &mut observer, &cancel);
            (convo, result)
        })
        .await
        .map_err(|_| acp::Error::internal_error())?;

        // ALWAYS write the (possibly partial) conversation back and release the
        // running guard — for BOTH Ok and Err — so tools already executed this
        // turn are not replayed on the client's retry (Fix #1). Then propagate.
        let (updated_convo, result) = joined;
        if let Some(st) = self.sessions.borrow_mut().get_mut(&key) {
            st.convo = updated_convo;
            st.running = false;
        }

        // The loop reports the real stop reason — a cancel landing on the final
        // (no-tool) step is Cancelled, not a spuriously "completed" EndTurn.
        let run_stop = result.map_err(acp::Error::into_internal_error)?;
        let stop = match run_stop {
            RunStop::Cancelled => acp::StopReason::Cancelled,
            RunStop::EndTurn => acp::StopReason::EndTurn,
            RunStop::MaxSteps => acp::StopReason::MaxTurnRequests,
        };
        Ok(acp::PromptResponse::new(stop))
    }

    async fn cancel(&self, args: acp::CancelNotification) -> acp::Result<()> {
        if let Some(st) = self.sessions.borrow().get(&args.session_id.0.to_string()) {
            st.cancel.cancel();
        }
        Ok(())
    }
}

/// Map a tool name to the ACP `ToolKind` the client renders it as. The default
/// (the shell tool, `bash`/`powershell`) is `Execute`.
fn tool_kind(name: &str) -> acp::ToolKind {
    match name {
        "read_file" => acp::ToolKind::Read,
        "write_file" | "edit_file" => acp::ToolKind::Edit,
        _ => acp::ToolKind::Execute,
    }
}

/// Bridges the sync loop's `Observer` callbacks to ACP `session/update`
/// notifications (agent message text + tool-call lifecycle updates). The LLM
/// tool-call id ties a call's `ToolCall` (in-progress) to its `ToolCallUpdate`
/// (completed/failed) — the same id the client sees in the permission request.
struct AcpObserver {
    updates: mpsc::UnboundedSender<ClientCall>,
    sid: acp::SessionId,
}

impl AcpObserver {
    fn send(&self, update: acp::SessionUpdate) {
        let _ = self
            .updates
            .send(ClientCall::Notify(acp::SessionNotification::new(
                self.sid.clone(),
                update,
            )));
    }
}

impl Observer for AcpObserver {
    fn on_text_delta(&mut self, delta: &str) {
        self.send(acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(delta.to_string())),
        ));
    }
    fn on_tool_call(&mut self, id: &str, name: &str, args: &serde_json::Value) {
        self.send(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(acp::ToolCallId::new(id.to_string()), name)
                .kind(tool_kind(name))
                .status(acp::ToolCallStatus::InProgress)
                .raw_input(args.clone()),
        ));
    }
    fn on_tool_result(&mut self, id: &str, _name: &str, outcome: &ToolOutcome) {
        let status = if outcome.is_error {
            acp::ToolCallStatus::Failed
        } else {
            acp::ToolCallStatus::Completed
        };
        self.send(acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(id.to_string()),
                acp::ToolCallUpdateFields::new()
                    .status(status)
                    .content(vec![acp::ToolCallContent::from(
                        outcome.ui_text().to_string(),
                    )]),
            ),
        ));
    }
    fn on_notice(&mut self, msg: &str) {
        self.send(acp::SessionUpdate::AgentThoughtChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(msg.to_string())),
        ));
    }
}

/// Bridges the sync loop's `Approver` to ACP `session/request_permission`: the
/// client (editor) decides whether a side-effecting tool may run. Read-only
/// tools are never gated (the loop doesn't consult the approver for them).
struct AcpApprover {
    calls: mpsc::UnboundedSender<ClientCall>,
    sid: acp::SessionId,
    /// The session's cancel flag, so a permission wait breaks (denies) when the
    /// client cancels the turn instead of hanging forever on the oneshot (Fix #5).
    cancel: Cancel,
}

impl Approver for AcpApprover {
    fn approve(&mut self, id: &str, tool_name: &str, args: &serde_json::Value) -> bool {
        // Reuse the streamed ToolCall's id so the client correlates the permission
        // request with the in-progress tool call it already rendered.
        let tool_call = acp::ToolCallUpdate::new(
            acp::ToolCallId::new(id.to_string()),
            acp::ToolCallUpdateFields::new().title(format!("{tool_name} {args}")),
        );
        let options = vec![
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(PERM_ALLOW),
                "Allow",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                acp::PermissionOptionId::new(PERM_REJECT),
                "Reject",
                acp::PermissionOptionKind::RejectOnce,
            ),
        ];
        let req = acp::RequestPermissionRequest::new(self.sid.clone(), tool_call, options);

        let (tx, mut rx) = oneshot::channel();
        if self.calls.send(ClientCall::Permission(req, tx)).is_err() {
            return false; // connection gone → deny
        }
        // Wait for the client's answer, but honor cancel so a never-answered
        // permission can't hang this blocking-pool thread forever. Polling (not a
        // single blocking_recv) lets us observe cancel between checks; the 20ms
        // sleep is fine here — this runs on a spawn_blocking thread.
        loop {
            match rx.try_recv() {
                Ok(acp::RequestPermissionOutcome::Selected(sel)) => {
                    return sel.option_id.0.as_ref() == PERM_ALLOW;
                }
                Ok(_) => return false, // Cancelled outcome → deny
                Err(oneshot::error::TryRecvError::Empty) => {
                    if self.cancel.is_cancelled() {
                        return false; // turn cancelled → deny, don't hang
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(oneshot::error::TryRecvError::Closed) => return false, // sender gone → deny
            }
        }
    }
}

/// Client-mediated [`FsBackend`]: the read/write/edit tools' file I/O is routed
/// through the ACP client (`fs/read_text_file`, `fs/write_text_file`) instead of
/// local disk, so the editor can serve unsaved buffers and track changes. Wired
/// in only when the client advertises both fs capabilities (see `prompt()`).
///
/// These methods run on the loop's `spawn_blocking` thread, so they block on the
/// paired oneshot; the async forwarding task (owning the connection) performs the
/// actual RPC. Paths passed in are already absolute (`ToolContext::resolve`).
struct AcpClientFs {
    calls: mpsc::UnboundedSender<ClientCall>,
    sid: acp::SessionId,
}

impl AcpClientFs {
    /// Send a queued call and block for its ack. Any send/recv failure (the
    /// forwarding task or connection is gone) collapses to a single unavailable
    /// message the tool surfaces as an error.
    fn round_trip<T>(
        &self,
        call: ClientCall,
        rx: oneshot::Receiver<std::result::Result<T, String>>,
    ) -> std::result::Result<T, String> {
        if self.calls.send(call).is_err() {
            return Err("acp client fs unavailable".to_string());
        }
        match rx.blocking_recv() {
            Ok(res) => res,
            Err(_) => Err("acp client fs unavailable".to_string()),
        }
    }
}

impl FsBackend for AcpClientFs {
    fn read(&self, path: &std::path::Path) -> std::result::Result<String, String> {
        let (tx, rx) = oneshot::channel();
        let req = acp::ReadTextFileRequest::new(self.sid.clone(), path.to_path_buf());
        self.round_trip(ClientCall::ReadFile(req, tx), rx)
    }

    fn write(&self, path: &std::path::Path, content: &str) -> std::result::Result<(), String> {
        let (tx, rx) = oneshot::channel();
        let req = acp::WriteTextFileRequest::new(self.sid.clone(), path.to_path_buf(), content);
        self.round_trip(ClientCall::WriteFile(req, tx), rx)
    }
}

/// Extract the plain-text portion of an ACP prompt (Text blocks joined).
fn prompt_text(blocks: &[acp::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            acp::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Entry point for `monocle acp`: run the ACP agent over stdio until the client
/// closes the connection. Sets up a current-thread runtime + `LocalSet` because
/// the crate's futures are `!Send`.
pub fn serve() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| AppError::new(e.to_string()))?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run());
    Ok(())
}

async fn run() {
    let outgoing = tokio::io::stdout().compat_write();
    let incoming = tokio::io::stdin().compat();

    let (tx, mut rx) = mpsc::unbounded_channel::<ClientCall>();
    let agent = std::rc::Rc::new(MonocleAgent::new(tx));
    // Hold a second handle so we can cancel in-flight sessions on shutdown (Fix #6).
    let shutdown = std::rc::Rc::clone(&agent);

    let (conn, io_task) = acp::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });

    // Make the loop's queued calls to the client over the connection: fire-and-
    // forget updates, and permission round-trips whose outcome is returned to the
    // (blocking) loop via the paired oneshot.
    let conn = std::rc::Rc::new(conn);
    tokio::task::spawn_local(async move {
        while let Some(call) = rx.recv().await {
            match call {
                ClientCall::Notify(note) => {
                    let _ = conn.session_notification(note).await;
                }
                ClientCall::Permission(req, ack) => {
                    // Spawn the round-trip so an unanswered permission doesn't
                    // head-of-line-block queued updates for other sessions (Fix #7).
                    let conn = std::rc::Rc::clone(&conn);
                    tokio::task::spawn_local(async move {
                        let outcome = match conn.request_permission(req).await {
                            Ok(resp) => resp.outcome,
                            Err(_) => acp::RequestPermissionOutcome::Cancelled,
                        };
                        let _ = ack.send(outcome);
                    });
                }
                ClientCall::ReadFile(req, ack) => {
                    // Client-mediated read; spawn so it can't head-of-line-block
                    // other queued calls (same rationale as Permission).
                    let conn = std::rc::Rc::clone(&conn);
                    tokio::task::spawn_local(async move {
                        let res = conn
                            .read_text_file(req)
                            .await
                            .map(|r| r.content)
                            .map_err(|e| e.to_string());
                        let _ = ack.send(res);
                    });
                }
                ClientCall::WriteFile(req, ack) => {
                    let conn = std::rc::Rc::clone(&conn);
                    tokio::task::spawn_local(async move {
                        let res = conn
                            .write_text_file(req)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string());
                        let _ = ack.send(res);
                    });
                }
            }
        }
    });

    let _ = io_task.await;

    // Client disconnected: signal every in-flight session to stop so the detached
    // spawn_blocking loops halt at their next step boundary and the runtime can
    // shut down instead of blocking on LLM calls / tools (Fix #6).
    for st in shutdown.sessions.borrow().values() {
        st.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `acp::Meta` (a `serde_json::Map`) from a JSON object literal.
    fn meta(value: serde_json::Value) -> acp::Meta {
        serde_json::from_value(value).expect("meta must be a JSON object")
    }

    #[test]
    fn session_model_defaults_when_meta_absent() {
        assert_eq!(session_model(&None), DEFAULT_MODEL);
    }

    #[test]
    fn session_model_defaults_when_key_absent() {
        let m = meta(serde_json::json!({ "other.key": "x" }));
        assert_eq!(session_model(&Some(m)), DEFAULT_MODEL);
    }

    #[test]
    fn session_model_defaults_when_key_not_a_string() {
        let m = meta(serde_json::json!({ "monocle.model": 42 }));
        assert_eq!(session_model(&Some(m)), DEFAULT_MODEL);
    }

    #[test]
    fn session_model_uses_meta_key_when_present() {
        let m = meta(serde_json::json!({ "monocle.model": "claude-haiku-4-5" }));
        assert_eq!(session_model(&Some(m)), "claude-haiku-4-5");
    }

    #[test]
    fn prompt_text_joins_text_blocks_and_ignores_non_text() {
        let blocks = vec![
            acp::ContentBlock::from("hello".to_string()),
            acp::ContentBlock::from("world".to_string()),
        ];
        assert_eq!(prompt_text(&blocks), "hello\nworld");
        assert_eq!(prompt_text(&[]), "");
    }

    /// Drive `AcpApprover` against a scripted client that answers the permission
    /// request with the given option id, off-thread (approve() blocks on it).
    fn approver_with_response(option: &'static str) -> bool {
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientCall>();
        let responder = std::thread::spawn(move || {
            if let Some(ClientCall::Permission(_req, ack)) = rx.blocking_recv() {
                let _ = ack.send(acp::RequestPermissionOutcome::Selected(
                    acp::SelectedPermissionOutcome::new(option),
                ));
            }
        });
        let mut approver = AcpApprover {
            calls: tx,
            sid: acp::SessionId::new("s"),
            cancel: Cancel::new(),
        };
        let granted = approver.approve("call_1", "write_file", &serde_json::json!({"path": "x"}));
        responder.join().unwrap();
        granted
    }

    #[test]
    fn approver_grants_only_when_client_selects_allow() {
        assert!(approver_with_response("allow"));
        assert!(!approver_with_response("reject"));
    }

    #[test]
    fn approver_denies_without_hanging_when_cancelled_and_no_answer() {
        // The client never answers the permission (rx kept, never drained), but the
        // turn is cancelled — approve() must break the wait and deny, not hang.
        let (tx, _rx) = mpsc::unbounded_channel::<ClientCall>();
        let cancel = Cancel::new();
        cancel.cancel();
        let mut approver = AcpApprover {
            calls: tx,
            sid: acp::SessionId::new("s"),
            cancel,
        };
        let start = std::time::Instant::now();
        assert!(!approver.approve("call_1", "write_file", &serde_json::json!({"path": "x"})));
        // The 20ms poll means it returns almost immediately; bound it generously.
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    /// Build an `AcpObserver` wired to an in-memory channel (no ACP stack), so a
    /// callback's emitted `SessionUpdate` can be read back synchronously.
    fn observer_channel() -> (AcpObserver, mpsc::UnboundedReceiver<ClientCall>) {
        let (tx, rx) = mpsc::unbounded_channel::<ClientCall>();
        let observer = AcpObserver {
            updates: tx,
            sid: acp::SessionId::new("s"),
        };
        (observer, rx)
    }

    /// The single `SessionUpdate` the observer just sent (panics otherwise).
    fn next_update(rx: &mut mpsc::UnboundedReceiver<ClientCall>) -> acp::SessionUpdate {
        match rx.try_recv().expect("expected one queued ClientCall") {
            ClientCall::Notify(n) => n.update,
            _ => panic!("expected Notify, got another ClientCall variant"),
        }
    }

    #[test]
    fn on_tool_call_emits_in_progress_tool_call_with_id_and_kind() {
        let (mut observer, mut rx) = observer_channel();
        observer.on_tool_call("call_9", "write_file", &serde_json::json!({"path": "x"}));
        match next_update(&mut rx) {
            acp::SessionUpdate::ToolCall(tc) => {
                assert_eq!(tc.tool_call_id.0.as_ref(), "call_9");
                assert_eq!(tc.status, acp::ToolCallStatus::InProgress);
                assert_eq!(tc.kind, acp::ToolKind::Edit);
            }
            _ => panic!("expected a ToolCall update"),
        }
    }

    #[test]
    fn on_tool_result_ok_emits_completed_update_with_same_id() {
        let (mut observer, mut rx) = observer_channel();
        observer.on_tool_result("call_9", "write_file", &ToolOutcome::ok("hi"));
        match next_update(&mut rx) {
            acp::SessionUpdate::ToolCallUpdate(tcu) => {
                assert_eq!(tcu.tool_call_id.0.as_ref(), "call_9");
                assert_eq!(tcu.fields.status, Some(acp::ToolCallStatus::Completed));
            }
            _ => panic!("expected a ToolCallUpdate update"),
        }
    }

    #[test]
    fn on_tool_result_error_emits_failed_update() {
        let (mut observer, mut rx) = observer_channel();
        observer.on_tool_result("call_9", "write_file", &ToolOutcome::error("boom"));
        match next_update(&mut rx) {
            acp::SessionUpdate::ToolCallUpdate(tcu) => {
                assert_eq!(tcu.fields.status, Some(acp::ToolCallStatus::Failed));
            }
            _ => panic!("expected a ToolCallUpdate update"),
        }
    }

    #[test]
    fn approver_denies_when_connection_is_gone() {
        let (tx, rx) = mpsc::unbounded_channel::<ClientCall>();
        drop(rx); // client/connection gone → the send fails → deny, never block.
        let mut approver = AcpApprover {
            calls: tx,
            sid: acp::SessionId::new("s"),
            cancel: Cancel::new(),
        };
        assert!(!approver.approve("call_1", "write_file", &serde_json::json!({})));
    }

    #[test]
    fn client_fs_read_returns_client_content() {
        // Scripted client: receive the ReadFile call off-thread and reply with
        // content, mirroring `approver_with_response` (read() blocks on the ack).
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientCall>();
        let responder = std::thread::spawn(move || {
            if let Some(ClientCall::ReadFile(req, ack)) = rx.blocking_recv() {
                assert_eq!(req.path, std::path::PathBuf::from("/abs/file.txt"));
                let _ = ack.send(Ok("hello from client".to_string()));
            }
        });
        let fs = AcpClientFs {
            calls: tx,
            sid: acp::SessionId::new("s"),
        };
        let got = fs.read(std::path::Path::new("/abs/file.txt"));
        responder.join().unwrap();
        assert_eq!(got.unwrap(), "hello from client");
    }

    #[test]
    fn client_fs_write_acks_ok() {
        let (tx, mut rx) = mpsc::unbounded_channel::<ClientCall>();
        let responder = std::thread::spawn(move || {
            if let Some(ClientCall::WriteFile(req, ack)) = rx.blocking_recv() {
                assert_eq!(req.path, std::path::PathBuf::from("/abs/out.txt"));
                assert_eq!(req.content, "payload");
                let _ = ack.send(Ok(()));
            }
        });
        let fs = AcpClientFs {
            calls: tx,
            sid: acp::SessionId::new("s"),
        };
        let got = fs.write(std::path::Path::new("/abs/out.txt"), "payload");
        responder.join().unwrap();
        assert!(got.is_ok());
    }

    #[test]
    fn client_fs_read_errors_when_forwarder_gone() {
        // No receiver → the queued call can't be delivered → surface an error
        // instead of blocking the tool forever.
        let (tx, rx) = mpsc::unbounded_channel::<ClientCall>();
        drop(rx);
        let fs = AcpClientFs {
            calls: tx,
            sid: acp::SessionId::new("s"),
        };
        assert!(fs.read(std::path::Path::new("/abs/file.txt")).is_err());
    }
}
