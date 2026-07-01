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
//! Scope: initialize / new_session / prompt / cancel; tools run locally; tool
//! permission is delegated to the client via `session/request_permission`
//! (allow/reject). Follow-ups: client fs/terminal callbacks, richer ToolCall
//! updates, per-session model config.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_client_protocol::{self as acp, Agent, Client as _};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::agent::providers::{Message, MonocleProvider};
use crate::agent::runner::{Agent as CoreAgent, AgentConfig, Approver, Cancel, Observer};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::net::Client;

const SYSTEM_PROMPT: &str = "You are Monocle's headless agent. Use the provided tools \
(read_file, write_file, edit_file, and the shell) to accomplish the user's task within the \
session's working directory. Take minimal, verified steps. When finished, give a brief summary.";

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_MAX_STEPS: usize = 20;

/// A call the (blocking) agent loop needs the async ACP connection to make on
/// its behalf — either a fire-and-forget update or a permission round-trip.
enum ClientCall {
    Notify(acp::SessionNotification),
    Permission(
        acp::RequestPermissionRequest,
        oneshot::Sender<acp::RequestPermissionOutcome>,
    ),
}

/// Per-ACP-session state, keyed by session id string.
struct SessionState {
    convo: Vec<Message>,
    cwd: PathBuf,
    cancel: Cancel,
}

struct MonocleAgent {
    /// Outbound `session/update` notifications → forwarded to the connection.
    updates: mpsc::UnboundedSender<ClientCall>,
    sessions: RefCell<HashMap<String, SessionState>>,
    next_id: AtomicU64,
}

impl MonocleAgent {
    fn new(updates: mpsc::UnboundedSender<ClientCall>) -> Self {
        Self {
            updates,
            sessions: RefCell::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl Agent for MonocleAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
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
            },
        );
        Ok(acp::NewSessionResponse::new(id))
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let key = args.session_id.0.to_string();

        // Snapshot the session state (release the RefCell borrow before awaiting).
        let (mut convo, cwd, cancel) = {
            let mut sessions = self.sessions.borrow_mut();
            let st = sessions
                .get_mut(&key)
                .ok_or_else(acp::Error::invalid_params)?;
            st.cancel.reset();
            (st.convo.clone(), st.cwd.clone(), st.cancel.clone())
        };

        let user = prompt_text(&args.prompt);
        convo.push(Message::user(user));

        let updates = self.updates.clone();
        let sid = args.session_id.clone();

        // Run the SYNC agent-core loop off the async thread; stream updates back,
        // and route side-effecting tool permission to the client (the editor
        // decides via session/request_permission).
        let joined = tokio::task::spawn_blocking(move || {
            let mut observer = AcpObserver {
                updates: updates.clone(),
                sid: sid.clone(),
            };
            let mut approver = AcpApprover {
                calls: updates,
                sid,
                tool_counter: 0,
            };
            let session = get_access_token(&Client::new(), &Credentials::new());
            let provider = MonocleProvider::from_session(session);
            let tools = ToolRegistry::with_defaults();
            let mut config = AgentConfig::new(DEFAULT_MODEL);
            config.max_steps = DEFAULT_MAX_STEPS;
            let agent = CoreAgent::new(&provider, &tools, ToolContext::new(cwd), config);
            let result = agent.run(&mut convo, &mut approver, &mut observer, &cancel);
            (convo, cancel.is_cancelled(), result.map(|_| ()))
        })
        .await
        .map_err(|_| acp::Error::internal_error())?;

        let (updated_convo, cancelled, result) = joined;
        result.map_err(acp::Error::into_internal_error)?;

        // Persist the updated conversation for follow-up prompts.
        if let Some(st) = self.sessions.borrow_mut().get_mut(&key) {
            st.convo = updated_convo;
        }

        let stop = if cancelled {
            acp::StopReason::Cancelled
        } else {
            acp::StopReason::EndTurn
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

/// Bridges the sync loop's `Observer` callbacks to ACP `session/update`
/// notifications (agent message text + tool progress as thought chunks).
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
    fn on_tool_call(&mut self, name: &str, args: &serde_json::Value) {
        let text = format!("⏵ {name} {args}");
        self.send(acp::SessionUpdate::AgentThoughtChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(text)),
        ));
    }
    fn on_tool_result(&mut self, _name: &str, outcome: &ToolOutcome) {
        let text = format!("  {}", outcome.ui_text());
        self.send(acp::SessionUpdate::AgentThoughtChunk(
            acp::ContentChunk::new(acp::ContentBlock::from(text)),
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
    tool_counter: u64,
}

impl Approver for AcpApprover {
    fn approve(&mut self, tool_name: &str, args: &serde_json::Value) -> bool {
        self.tool_counter += 1;
        let tool_call = acp::ToolCallUpdate::new(
            acp::ToolCallId::new(format!("tc-{}", self.tool_counter)),
            acp::ToolCallUpdateFields::new().title(format!("{tool_name} {args}")),
        );
        let options = vec![
            acp::PermissionOption::new(
                acp::PermissionOptionId::new("allow"),
                "Allow",
                acp::PermissionOptionKind::AllowOnce,
            ),
            acp::PermissionOption::new(
                acp::PermissionOptionId::new("reject"),
                "Reject",
                acp::PermissionOptionKind::RejectOnce,
            ),
        ];
        let req = acp::RequestPermissionRequest::new(self.sid.clone(), tool_call, options);

        let (tx, rx) = oneshot::channel();
        if self.calls.send(ClientCall::Permission(req, tx)).is_err() {
            return false; // connection gone → deny
        }
        // Block this off-thread loop until the client answers (or the turn ends).
        matches!(
            rx.blocking_recv(),
            Ok(acp::RequestPermissionOutcome::Selected(sel)) if sel.option_id.0.as_ref() == "allow"
        )
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

    let (conn, io_task) = acp::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });

    // Make the loop's queued calls to the client over the connection: fire-and-
    // forget updates, and permission round-trips whose outcome is returned to the
    // (blocking) loop via the paired oneshot.
    tokio::task::spawn_local(async move {
        while let Some(call) = rx.recv().await {
            match call {
                ClientCall::Notify(note) => {
                    let _ = conn.session_notification(note).await;
                }
                ClientCall::Permission(req, ack) => {
                    let outcome = match conn.request_permission(req).await {
                        Ok(resp) => resp.outcome,
                        Err(_) => acp::RequestPermissionOutcome::Cancelled,
                    };
                    let _ = ack.send(outcome);
                }
            }
        }
    });

    let _ = io_task.await;
}

#[cfg(test)]
mod tests {
    use super::*;

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
            tool_counter: 0,
        };
        let granted = approver.approve("write_file", &serde_json::json!({"path": "x"}));
        responder.join().unwrap();
        granted
    }

    #[test]
    fn approver_grants_only_when_client_selects_allow() {
        assert!(approver_with_response("allow"));
        assert!(!approver_with_response("reject"));
    }

    #[test]
    fn approver_denies_when_connection_is_gone() {
        let (tx, rx) = mpsc::unbounded_channel::<ClientCall>();
        drop(rx); // client/connection gone → the send fails → deny, never block.
        let mut approver = AcpApprover {
            calls: tx,
            sid: acp::SessionId::new("s"),
            tool_counter: 0,
        };
        assert!(!approver.approve("write_file", &serde_json::json!({})));
    }
}
