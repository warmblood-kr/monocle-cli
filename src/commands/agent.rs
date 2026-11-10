//! `monocle agent` — experimental headless agent loop (Path B, monocle-cli#44).
//!
//! Wires the real provider (monocle routing) + the default toolset (read/write/
//! edit + cross-platform shell) into the agent-core loop, prints progress to
//! stderr, and the final answer to stdout.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use serde_json::Value;

use crate::agent::commands::{config_text, help_text, login_view, status_text};
use crate::agent::permission::PermissionStore;
use crate::agent::providers::{
    ChatRequest, ChatResponse, LlmProvider, Message, MonocleProvider, TokenUsage,
};
use crate::agent::runner::{Agent, AllowAll, Approver, Cancel, Observer, RunStop};
use crate::agent::session::{session_path, SessionStore};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::agent::SYSTEM_PROMPT;
use crate::auth::get_access_token;
use crate::colors as c;
use crate::commands::model_list::{fetch_model_ids, handle_model_command};
use crate::commands::repl::{
    diag_output, mode_banner, run_repl, LoopControl, ModelReplHelper, TurnDiagnostics, PROMPT,
};
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::util::home_dir;

pub struct AgentOptions {
    pub prompt: Option<String>,
    pub workdir: Option<String>,
    pub model: String,
    pub max_steps: usize,
    pub session: Option<String>,
    /// Skip the per-tool approval prompt (dangerous) — run side-effecting tools
    /// unattended, as scripts/one-shot already do.
    pub auto_approve: bool,
    /// Explicit buffered, text-only shared-core lane; native remains the default.
    pub experimental_core_model_only: bool,
    /// Explicit bounded tool-workflow lane using the existing native provider and tools.
    pub experimental_core_tool_workflow: bool,
}

/// Pure preflight runs before auth refresh or any provider call.
fn validate_core_history(messages: &[Message]) -> Result<()> {
    if messages.is_empty()
        || !messages.iter().any(|message| message.role == "system")
        || messages
            .iter()
            .any(|message| !matches!(message.role.as_str(), "system" | "user" | "assistant"))
    {
        return Err(AppError::new("core model-only: unsupported request"));
    }
    if messages.iter().any(|message| {
        message.role == "tool"
            || message.tool_call_id.is_some()
            || !message.tool_calls.is_empty()
            || message.role == "system"
                && message
                    .content
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
    }) {
        return Err(AppError::new("core model-only: unsupported request"));
    }
    Ok(())
}

/// The tool-workflow lane can resume existing assistant/tool exchanges, but
/// still requires one valid system prompt and only native chat roles.
fn validate_core_tool_history(messages: &[Message]) -> Result<()> {
    if messages.is_empty()
        || !messages.iter().any(|message| message.role == "system")
        || messages.iter().any(|message| {
            !matches!(
                message.role.as_str(),
                "system" | "user" | "assistant" | "tool"
            ) || message.role == "tool" && message.tool_call_id.is_none()
                || message.role == "system"
                    && message
                        .content
                        .as_deref()
                        .is_none_or(|content| content.trim().is_empty())
        })
    {
        return Err(AppError::new("core tool workflow: unsupported request"));
    }
    Ok(())
}

/// The actual Repl dispatch seam. The native closure is the unchanged Agent run.
fn run_selected_turn_with_session<P, N>(
    experimental_core_model_only: bool,
    provider: &P,
    request: &ChatRequest,
    conversation: &mut Vec<Message>,
    observer: &mut dyn Observer,
    cancel: &Cancel,
    persisted: &mut usize,
    session: Option<&SessionStore>,
    poisoned: &mut bool,
    native: N,
) -> Result<RunStop>
where
    P: LlmProvider + ?Sized,
    N: FnOnce(&mut Vec<Message>, &mut dyn Observer) -> Result<RunStop>,
{
    if !experimental_core_model_only {
        return native(conversation, observer);
    }
    let response = crate::agent::core_model_only::run_model_only(
        provider,
        request,
        conversation,
        persisted,
        session,
        poisoned,
        cancel,
    )?;
    observer.on_turn_step(&response);
    if !response.content.is_empty() {
        observer.on_text_delta(&response.content);
    }
    Ok(RunStop::EndTurn)
}

/// Unit-test seam for the old adapter contract. The real Repl route below uses
/// `run_selected_turn_with_session`, which requires the archive transaction.
#[cfg(test)]
fn run_selected_turn<P, N>(
    experimental_core_model_only: bool,
    provider: &P,
    request: &ChatRequest,
    conversation: &mut Vec<Message>,
    observer: &mut dyn Observer,
    cancel: &Cancel,
    native: N,
) -> Result<RunStop>
where
    P: LlmProvider + ?Sized,
    N: FnOnce(&mut Vec<Message>, &mut dyn Observer) -> Result<RunStop>,
{
    if !experimental_core_model_only {
        return native(conversation, observer);
    }
    if cancel.is_cancelled() {
        return Err(AppError::new("core model-only: cancelled"));
    }
    let response = provider.chat(request)?;
    if cancel.is_cancelled() {
        return Err(AppError::new("core model-only: cancelled"));
    }
    if response.truncated || !response.tool_calls.is_empty() {
        return Err(AppError::new("core model-only: unsupported response"));
    }
    observer.on_turn_step(&response);
    if !response.content.is_empty() {
        observer.on_text_delta(&response.content);
    }
    conversation.push(Message::assistant(response.content));
    Ok(RunStop::EndTurn)
}

/// Keep the REPL's production and synthetic providers behind one sized type so
/// `Agent` can retain its borrowed provider without changing agent-core.
struct CliProvider(Box<dyn LlmProvider>);

impl LlmProvider for CliProvider {
    fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        self.0.chat(request)
    }

    fn chat_stream(
        &self,
        request: &ChatRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ChatResponse> {
        self.0.chat_stream(request, on_delta)
    }
}

fn turn_ttfb(
    experimental_core_model_only: bool,
    first_delta_at: Option<std::time::Instant>,
    started: std::time::Instant,
) -> Option<u128> {
    if experimental_core_model_only {
        None
    } else {
        first_delta_at.map(|time| time.duration_since(started).as_millis())
    }
}

/// Turn-scoped accumulator for `/diag`: `Agent::run`'s tool-use loop can call
/// the LLM several times per turn (each a "step"), and this collects the
/// last-known served model plus a running token total across all of them.
struct CliObserver<'a> {
    output: &'a mut dyn Write,
    served_model: Option<String>,
    usage: Option<TokenUsage>,
    steps: u32,
    /// Set on the first `on_text_delta` call this turn — used to compute
    /// `/diag`'s "time to first byte" line (relative to `run_turn`'s `started`).
    first_delta_at: Option<std::time::Instant>,
    buffer_output: bool,
    buffered_text: String,
}
impl<'a> CliObserver<'a> {
    fn new(output: &'a mut dyn Write, buffer_output: bool) -> Self {
        Self {
            output,
            served_model: None,
            usage: None,
            steps: 0,
            first_delta_at: None,
            buffer_output,
            buffered_text: String::new(),
        }
    }
}

impl Observer for CliObserver<'_> {
    fn on_text_delta(&mut self, delta: &str) {
        if self.first_delta_at.is_none() {
            self.first_delta_at = Some(std::time::Instant::now());
        }
        if self.buffer_output {
            self.buffered_text.push_str(delta);
        } else {
            // Stream assistant text to stdout as it arrives (tool progress goes to stderr).
            let _ = self.output.write_all(delta.as_bytes());
            let _ = self.output.flush();
        }
    }
    fn on_tool_call(&mut self, _id: &str, name: &str, args: &Value) {
        eprintln!(
            "\n{} {} {}",
            c::cyan("⏵"),
            c::bold(name),
            c::dim(&one_line(&args.to_string(), 120))
        );
    }
    fn on_tool_result(&mut self, _id: &str, _name: &str, outcome: &ToolOutcome) {
        let tag = if outcome.is_error {
            c::red("✗")
        } else {
            c::green("✓")
        };
        eprintln!("  {} {}", tag, c::dim(&one_line(outcome.ui_text(), 200)));
    }
    fn on_notice(&mut self, msg: &str) {
        // Control notices go to stderr — stdout stays the clean answer channel.
        eprintln!("\n{}", c::dim(msg));
    }
    fn on_turn_step(&mut self, resp: &ChatResponse) {
        self.steps += 1;
        if let Some(model) = &resp.model {
            self.served_model = Some(model.clone());
        }
        if let Some(u) = &resp.usage {
            // Each step's `usage.prompt_tokens` reflects that step's *entire*
            // re-sent conversation, so summing across steps overcounts
            // "distinct prompt content" — but it's still the right number for
            // "total tokens this turn billed across all its LLM calls", the
            // practically useful DX/cost question, so it's summed anyway.
            let acc = self.usage.get_or_insert(TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            });
            acc.prompt_tokens = acc.prompt_tokens.saturating_add(u.prompt_tokens);
            acc.completion_tokens = acc.completion_tokens.saturating_add(u.completion_tokens);
            acc.total_tokens = acc.total_tokens.saturating_add(u.total_tokens);
        }
    }
}

/// Shared post-preflight lifecycle for one Repl turn. Production prepares auth,
/// provider, Agent, and approver before entering so their construction stays
/// outside the latency timer; tests supply synthetic callbacks to assert
/// ordering and rollback without contacting a provider.
fn run_turn_lifecycle<P, A, E, S>(
    experimental_core_model_only: bool,
    experimental_core_tool_workflow: bool,
    conversation: &mut Vec<Message>,
    persisted: &mut usize,
    selected_session_poisoned: &mut bool,
    diagnostics: &mut Option<TurnDiagnostics>,
    session: Option<&SessionStore>,
    cancel: &Cancel,
    model: &str,
    user: String,
    output: &mut dyn Write,
    authenticate_and_prepare: A,
    execute: E,
    append: S,
) -> Result<()>
where
    A: FnOnce() -> P,
    E: FnOnce(
        P,
        &ChatRequest,
        &mut Vec<Message>,
        &mut dyn Observer,
        &Cancel,
        &mut usize,
        Option<&SessionStore>,
        &mut bool,
    ) -> Result<String>,
    S: FnOnce(Option<&SessionStore>, &[Message]) -> Result<()>,
{
    let selected_core = experimental_core_model_only || experimental_core_tool_workflow;
    if experimental_core_model_only {
        if let Err(error) = validate_core_history(conversation) {
            *diagnostics = None;
            return Err(error);
        }
        if session.is_none() {
            *diagnostics = None;
            return Err(AppError::new(
                "core model-only: a named session is required",
            ));
        }
    }
    if experimental_core_tool_workflow {
        if let Err(error) = validate_core_tool_history(conversation) {
            *diagnostics = None;
            return Err(error);
        }
        if session.is_none() {
            *diagnostics = None;
            return Err(AppError::new(
                "core tool workflow: a named session is required",
            ));
        }
    }

    // Keep auth/provider construction after pure preflight and before adding
    // this user's message, matching the existing Repl ordering exactly.
    let prepared = authenticate_and_prepare();
    cancel.reset();
    let mark = conversation.len();
    let mut working = conversation.clone();
    working.push(Message::user(user.clone()));
    if !selected_core {
        conversation.push(Message::user(user));
    }
    let started = std::time::Instant::now();
    let mut observer = CliObserver::new(output, selected_core);
    let request = ChatRequest {
        model: model.to_string(),
        messages: if selected_core {
            working.clone()
        } else {
            conversation.clone()
        },
        ..Default::default()
    };

    let target = if selected_core {
        &mut working
    } else {
        &mut *conversation
    };
    let turn_endpoint = match execute(
        prepared,
        &request,
        target,
        &mut observer,
        cancel,
        persisted,
        session,
        selected_session_poisoned,
    ) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            *diagnostics = None;
            if experimental_core_tool_workflow && *selected_session_poisoned {
                *conversation = working;
            }
            if !selected_core {
                conversation.truncate(mark);
            }
            return Err(error);
        }
    };
    if selected_core {
        // The adapter has appended successfully before it returns; publish its
        // working conversation and committed cursor together at this boundary.
        *conversation = working;
    }

    let served_model = observer.served_model.clone();
    let first_delta_at = observer.first_delta_at;
    let usage = observer.usage.clone();
    let steps = observer.steps;
    let buffered_text = observer.buffered_text.clone();
    drop(observer);
    if selected_core {
        if let Err(error) = output
            .write_all(buffered_text.as_bytes())
            .and_then(|_| output.write_all(b"\n"))
            .and_then(|_| output.flush())
        {
            *diagnostics = None;
            return Err(error.into());
        }
    } else {
        output.write_all(b"\n")?;
        output.flush()?;
    }
    *diagnostics = Some(TurnDiagnostics::for_agent(
        model.to_string(),
        served_model,
        turn_endpoint,
        turn_ttfb(selected_core, first_delta_at, started),
        started.elapsed().as_millis(),
        usage,
        steps,
    ));

    if !selected_core {
        if let Some(store) = session {
            append(Some(store), &conversation[*persisted..])?;
            *persisted = conversation.len();
        }
    }
    Ok(())
}

/// The slash commands the REPL understands, offered by the completer and shown
/// by `/help`. Kept here (next to the dispatcher) so completion never drifts from
/// what `dispatch_command` / `parse_model_command` actually handle.
const SLASH_COMMANDS: &[&str] = &[
    "/help", "/config", "/status", "/model", "/diag", "/exit", "/quit",
];

/// Interactive approver with a memory. Before each side-effecting tool call it
/// consults the session's [`PermissionStore`]; if the call is already authorized
/// (allowed earlier this session, or persisted in `.monocle/settings.json`) it
/// runs with no prompt. Otherwise it prompts on stderr with four choices —
/// **[y]** once, **[s]** this session, **[a]** always (persist), **[N]** deny —
/// and records `s`/`a` in the store. Default (empty / unknown / EOF) is deny.
/// Only ever constructed on an interactive TTY, where the terminal is in cooked
/// mode during a turn so a plain `read_line` works.
struct PromptApprover<'a> {
    store: &'a mut PermissionStore,
}
impl Approver for PromptApprover<'_> {
    fn approve(&mut self, _id: &str, name: &str, args: &Value) -> bool {
        // A prior session/persisted decision skips the prompt entirely.
        if self.store.is_allowed(name, args) {
            return true;
        }
        // For the shell tool the interesting thing is the command; else the JSON.
        let summary = args
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| args.to_string());
        eprint!(
            "{} {} {}: {}  {} ",
            c::yellow("⚠"),
            c::dim("run"),
            c::bold(name),
            c::dim(&one_line(&summary, 120)),
            c::yellow("[y]es once / [s]ession / [a]lways / [N]o"),
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let answer = match std::io::stdin().read_line(&mut line) {
            Ok(0) => ApprovalAnswer::Deny, // EOF → deny
            Ok(_) => approve_answer(&line),
            Err(_) => ApprovalAnswer::Deny,
        };
        match answer {
            ApprovalAnswer::Once => true,
            ApprovalAnswer::Session => {
                self.store.allow_session(name, args);
                true
            }
            ApprovalAnswer::Always => {
                if let Err(e) = self.store.allow_always(name, args) {
                    eprintln!("{} 권한을 저장하지 못했습니다: {e}", c::yellow("⚠"));
                }
                true
            }
            ApprovalAnswer::Deny => false,
        }
    }
}

/// A four-way approval decision. Anything but a leading `y`/`s`/`a` is `Deny` —
/// the safe default (empty, `n`, EOF, a stray word).
#[derive(Debug, PartialEq, Eq)]
enum ApprovalAnswer {
    /// `y` — allow this one call only.
    Once,
    /// `s` — allow for the rest of this session (in-memory).
    Session,
    /// `a` — allow always; persist to `.monocle/settings.json`.
    Always,
    /// `n` / empty / unknown / EOF — deny.
    Deny,
}

/// Pure decision for an approval prompt, keyed on the first non-space character.
fn approve_answer(input: &str) -> ApprovalAnswer {
    match input.trim_start().chars().next() {
        Some('y') | Some('Y') => ApprovalAnswer::Once,
        Some('s') | Some('S') => ApprovalAnswer::Session,
        Some('a') | Some('A') => ApprovalAnswer::Always,
        _ => ApprovalAnswer::Deny,
    }
}

/// Approver-selection policy: use the interactive [`PromptApprover`] only on an
/// interactive TTY without `--auto-approve`. Non-interactive (piped / one-shot)
/// and `--auto-approve` keep today's `AllowAll` behavior (they already print the
/// experimental warning and have no TTY to prompt on).
fn use_prompt_approver(auto_approve: bool, interactive: bool) -> bool {
    interactive && !auto_approve
}

/// Per-session driver. Rebuilds the provider each turn so a fresh (auto-refreshed)
/// token is used — a long-lived interactive session never sends a stale bearer.
struct Repl<'a> {
    client: &'a Client,
    creds: &'a Credentials,
    tools: ToolRegistry,
    workdir: PathBuf,
    model: String,
    max_steps: usize,
    cancel: Cancel,
    session: Option<SessionStore>,
    convo: Vec<Message>,
    persisted: usize,
    /// A failed selected append may have partially written; require reopen/reconciliation.
    selected_session_poisoned: bool,
    /// `--auto-approve`: run side-effecting tools without prompting.
    auto_approve: bool,
    experimental_core_model_only: bool,
    experimental_core_tool_workflow: bool,
    /// Whether this session runs on an interactive TTY (can prompt for approval).
    interactive: bool,
    /// Remembered tool-permission decisions (session + `.monocle/settings.json`),
    /// so an allowed tool isn't re-prompted for the life of the session.
    permissions: PermissionStore,
    /// Last turn's diagnostics, shown on demand by `/diag` — `None` until the
    /// first successful turn, mirroring `monocle chat`'s REPL.
    diagnostics: Option<TurnDiagnostics>,
    /// Synthetic-only injection for exercising the actual `run_turn` entry.
    #[cfg(test)]
    test_provider: Option<(Box<dyn LlmProvider>, String)>,
    #[cfg(test)]
    test_output: Option<&'a mut dyn Write>,
}

impl Repl<'_> {
    /// The `--session` name, if this REPL is persisting a conversation.
    /// Derived from the session file stem (`<name>.jsonl`).
    fn session_name(&self) -> Option<&str> {
        self.session
            .as_ref()
            .and_then(|s| s.path().file_stem())
            .and_then(|s| s.to_str())
    }

    fn run_turn(&mut self, user: String) -> Result<()> {
        if (self.experimental_core_model_only || self.experimental_core_tool_workflow)
            && self.selected_session_poisoned
        {
            self.diagnostics = None;
            return Err(AppError::new(
                "core agent: session requires reopen or reconciliation",
            ));
        }
        // Actual Repl entry preflights before token refresh/provider construction.
        if self.experimental_core_model_only {
            if let Err(error) = validate_core_history(&self.convo) {
                self.diagnostics = None;
                return Err(error);
            }
            if self.session.is_none() {
                self.diagnostics = None;
                return Err(AppError::new(
                    "core model-only: a named session is required",
                ));
            }
        }
        if self.experimental_core_tool_workflow {
            if let Err(error) = validate_core_tool_history(&self.convo) {
                self.diagnostics = None;
                return Err(error);
            }
            if self.session.is_none() {
                self.diagnostics = None;
                return Err(AppError::new(
                    "core tool workflow: a named session is required",
                ));
            }
        }
        if let Some(session) = self.session.as_ref() {
            crate::agent::core_tool_workflow::ensure_no_pending_dispatches(session)?;
        }

        #[cfg(test)]
        let test_provider = self.test_provider.take();
        #[cfg(test)]
        let test_output = self.test_output.take();

        let (provider, turn_endpoint) = {
            #[cfg(test)]
            if let Some((provider, endpoint)) = test_provider {
                (CliProvider(provider), endpoint)
            } else {
                let auth = get_access_token(self.client, self.creds);
                let endpoint = format!("{}{}", auth.router_url, crate::endpoints::CHAT_COMPLETIONS);
                (
                    CliProvider(Box::new(MonocleProvider::from_session(auth))),
                    endpoint,
                )
            }
            #[cfg(not(test))]
            {
                let auth = get_access_token(self.client, self.creds);
                let endpoint = format!("{}{}", auth.router_url, crate::endpoints::CHAT_COMPLETIONS);
                (
                    CliProvider(Box::new(MonocleProvider::from_session(auth))),
                    endpoint,
                )
            }
        };

        let Repl {
            tools,
            workdir,
            model,
            max_steps,
            cancel,
            session,
            convo,
            persisted,
            selected_session_poisoned,
            auto_approve,
            experimental_core_model_only,
            experimental_core_tool_workflow,
            interactive,
            permissions,
            diagnostics,
            ..
        } = self;

        // Preserve the original timing boundary: build the Agent and select its
        // approver before the timer starts in run_turn_lifecycle.
        let agent = Agent::with_max_steps(
            &provider,
            tools,
            ToolContext::new(workdir.clone()).with_cancel(cancel.clone()),
            model.as_str(),
            *max_steps,
        );
        let mut allow = AllowAll;
        let mut prompt = PromptApprover { store: permissions };
        let approver: &mut dyn Approver = if use_prompt_approver(*auto_approve, *interactive) {
            &mut prompt
        } else {
            &mut allow
        };

        let mut stdout = std::io::stdout();
        #[cfg(test)]
        let output: &mut dyn Write = match test_output {
            Some(writer) => writer,
            None => &mut stdout,
        };
        #[cfg(not(test))]
        let output: &mut dyn Write = &mut stdout;

        run_turn_lifecycle(
            *experimental_core_model_only,
            *experimental_core_tool_workflow,
            convo,
            persisted,
            selected_session_poisoned,
            diagnostics,
            session.as_ref(),
            cancel,
            model,
            user,
            output,
            // Auth/provider and Agent/approver are prepared above, before the
            // timer starts. This no-op retains the helper's preflight seam.
            || (),
            |(), request, conversation, observer, cancel, persisted, session, poisoned| {
                if *experimental_core_tool_workflow {
                    crate::agent::core_tool_workflow::run_tool_workflow(
                        &provider,
                        request,
                        conversation,
                        observer,
                        cancel,
                        persisted,
                        session,
                        poisoned,
                        tools,
                        &ToolContext::new(workdir.clone()).with_cancel(cancel.clone()),
                        approver,
                        *max_steps,
                    )?;
                } else {
                    run_selected_turn_with_session(
                        *experimental_core_model_only,
                        &provider,
                        request,
                        conversation,
                        observer,
                        cancel,
                        persisted,
                        session,
                        poisoned,
                        |conversation, observer| {
                            agent.run(conversation, approver, observer, cancel)
                        },
                    )?;
                }
                Ok(turn_endpoint)
            },
            |session, messages| match session {
                Some(store) => store.append(messages),
                None => Ok(()),
            },
        )
    }
}

/// A slash command typed into the interactive REPL. Management commands are
/// handled locally (never sent to the agent/LLM); anything else is a turn.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Config,
    Status,
    Diag,
    Quit,
    Unknown(String),
    /// Not a slash command — send it to the agent as a normal turn.
    Turn,
}

/// Pure classifier for a trimmed input line. Only lines starting with `/` are
/// management commands; everything else is a turn for the agent.
fn dispatch_command(input: &str) -> Command {
    if !input.starts_with('/') {
        return Command::Turn;
    }
    match input {
        "/help" => Command::Help,
        "/config" => Command::Config,
        "/status" => Command::Status,
        "/diag" => Command::Diag,
        "/exit" | "/quit" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    }
}

pub fn agent_command(client: &Client, creds: &Credentials, opts: AgentOptions) -> Result<()> {
    let workdir = opts
        .workdir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let interactive = std::io::stdin().is_terminal();

    eprintln!(
        "{} {}",
        mode_banner("agent", c::orange),
        c::dim(&workdir.display().to_string())
    );
    // Interactive sessions gate side-effecting tools behind a y/N prompt (unless
    // `--auto-approve`); scripts / one-shot / piped runs have no TTY to prompt on
    // and keep running tools unattended, so the warning differs by mode.
    let warning = if use_prompt_approver(opts.auto_approve, interactive) {
        "⚠ experimental: side-effecting tools (write/edit + shell) require approval before each run. Run only in a directory you trust."
    } else {
        "⚠ experimental: tools auto-approved (read/write/edit + shell). Run only in a directory you trust."
    };
    eprintln!("{}", c::yellow(warning));

    // Ctrl-C cancels the *current turn* (not the process); Ctrl-D / /exit quits.
    let cancel = Cancel::new();
    let _ = ctrlc::set_handler({
        let c = cancel.clone();
        move || c.cancel()
    });

    // Layer any guide files (personal ~/.monocle, then the workdir) onto the
    // system prompt so user/project instructions steer the agent. Only seeds a
    // *new* conversation; a resumed session replays its own persisted system turn.
    let guides = crate::agent::guide::load_guides(&home_dir(), &workdir);
    for g in &guides {
        eprintln!("{}", c::dim(&format!("loaded guide — {}", g.label)));
    }
    let system_prompt = crate::agent::guide::augment_system_prompt(SYSTEM_PROMPT, &guides);

    // Optional named session: resume by replaying the persisted conversation.
    let session: Option<SessionStore> = opts
        .session
        .as_ref()
        .map(|name| SessionStore::new(session_path(&home_dir(), name)));
    let (convo, persisted) = match &session {
        Some(store) => {
            crate::agent::core_tool_workflow::ensure_no_pending_dispatches(store)?;
            let loaded = store.load()?;
            if loaded.is_empty() {
                (vec![Message::system(system_prompt)], 0)
            } else {
                eprintln!(
                    "{}",
                    c::dim(&format!("resumed session ({} messages)", loaded.len()))
                );
                let n = loaded.len();
                (loaded, n)
            }
        }
        None => (vec![Message::system(system_prompt)], 0),
    };

    // Load any previously-persisted tool permissions for this working directory
    // so allowed tools run without re-prompting.
    let permissions = PermissionStore::load(&workdir);

    let mut repl = Repl {
        client,
        creds,
        tools: ToolRegistry::with_defaults(),
        workdir,
        model: opts.model,
        max_steps: opts.max_steps,
        cancel,
        session,
        convo,
        persisted,
        selected_session_poisoned: false,
        auto_approve: opts.auto_approve,
        experimental_core_model_only: opts.experimental_core_model_only,
        experimental_core_tool_workflow: opts.experimental_core_tool_workflow,
        interactive,
        permissions,
        diagnostics: None,
        #[cfg(test)]
        test_provider: None,
        #[cfg(test)]
        test_output: None,
    };

    // Seed the first turn from the prompt arg, or (non-interactive) piped stdin.
    if let Some(p) = opts.prompt {
        repl.run_turn(p)?;
    } else if !interactive {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        if input.trim().is_empty() {
            eprintln!("No input provided.");
            std::process::exit(1);
        }
        repl.run_turn(input.trim().to_string())?;
    }

    // Piped / one-shot: done. Interactive: keep taking follow-ups in the session.
    if !interactive {
        return Ok(());
    }

    eprintln!(
        "{}",
        c::dim(
            "Interactive — ↑/↓ history, ←/→ & Ctrl-A/E/K/Y editing, Tab completes /commands; /help for commands; Ctrl-C aborts the turn; Ctrl-D or /exit to quit."
        )
    );

    // Prime `/model` tab-completion with the real model id list — fetched once,
    // up front, never inside the completer itself (a blocking HTTP call on every
    // Tab press would make typing laggy). Best-effort: not logged in, an
    // unreachable router, or any other fetch error just leaves the list empty
    // rather than blocking startup or crashing the REPL — the same defensive
    // posture `login_view` uses for `/status` in a not-logged-in session. The
    // `client`'s own connect/total timeouts (`src/net.rs`) already bound how
    // long this can hang.
    let model_ids = fetch_model_ids(client, creds).unwrap_or_default();

    // rustyline gives the input line proper editing (←/→, ↑/↓ history, Emacs
    // Ctrl-A/E/K/Y) with its default Emacs keymap + file history. It is used
    // ONLY here, on the interactive TTY path — the headless / piped / ACP paths
    // above never touch it, keeping the engine UI-free (CLAUDE.md 설계 원칙:
    // rich UI is the host's job via `monocle acp`). This is input-line editing
    // only; it does not take over scrollback/paging. The Editor build,
    // bracketed-paste config, history load/save, and the Ctrl-C/Ctrl-D/error
    // loop itself are shared with `monocle chat`'s REPL via
    // `commands::repl::run_repl`; only the helper (Tab-completion) and the
    // per-line dispatch below are specific to this REPL.
    //
    // rustyline reads the prompt in raw mode, where Ctrl-C surfaces as
    // `Interrupted` WITHOUT raising SIGINT — so the `ctrlc` turn-cancel handler
    // is not triggered at the idle prompt (we just start a fresh line). During a
    // turn the terminal is back in cooked mode, so Ctrl-C raises SIGINT and that
    // handler cancels the running turn, exactly as before.
    //
    // Plain, uncolored — a prompt string handed to `rl.readline()` must not
    // carry raw ANSI escapes. rustyline computes the prompt's on-screen width
    // by skipping recognized escape sequences (0-width), but on Windows the
    // console only interprets those bytes as color if
    // ENABLE_VIRTUAL_TERMINAL_PROCESSING is active; when that fails to enable,
    // the escapes print as literal characters, so rustyline's cursor-column
    // math (0-width) diverges from the terminal's real cursor position —
    // seen as the cursor sitting ~10 columns in in PowerShell before any input.
    let helper = ModelReplHelper {
        commands: SLASH_COMMANDS,
        models: model_ids,
    };
    let history_path = home_dir().join(".monocle").join("agent_history");

    run_repl(helper, history_path, PROMPT, |msg| {
        // `/model` switches the model for subsequent turns (handled before
        // the general dispatch since it carries an argument).
        if handle_model_command(msg, &mut repl.model) {
            return Ok(LoopControl::Continue);
        }
        // Slash commands are handled locally (never sent to the agent/LLM),
        // and printed to stderr so stdout stays the clean answer channel.
        match dispatch_command(msg) {
            Command::Quit => {
                eprintln!("Bye.");
                Ok(LoopControl::Quit)
            }
            Command::Help => {
                eprintln!("{}", help_text());
                Ok(LoopControl::Continue)
            }
            Command::Config => {
                eprintln!(
                    "{}",
                    config_text(
                        &repl.model,
                        repl.max_steps,
                        &repl.workdir,
                        repl.session_name()
                    )
                );
                Ok(LoopControl::Continue)
            }
            Command::Status => {
                let login = login_view(repl.creds);
                eprintln!(
                    "{}",
                    status_text(
                        login.as_ref(),
                        &repl.model,
                        repl.max_steps,
                        &repl.workdir,
                        repl.session_name(),
                    )
                );
                Ok(LoopControl::Continue)
            }
            Command::Diag => {
                eprintln!("{}", diag_output(&repl.diagnostics));
                Ok(LoopControl::Continue)
            }
            Command::Unknown(cmd) => {
                eprintln!("unknown command: {cmd} (try /help)");
                Ok(LoopControl::Continue)
            }
            Command::Turn => {
                // Reset before this turn's work runs, so the hint below
                // reflects only whether *this* turn logged a network error —
                // not a stale flag left over from an earlier one.
                crate::diag::reset();
                // A transient per-turn error must not tear down the session.
                if let Err(e) = repl.run_turn(msg.to_string()) {
                    eprintln!("{} {e}", c::red("Error:"));
                    if crate::diag::was_logged() {
                        eprintln!(
                            "  {}",
                            c::dim(&format!(
                                "(details logged to {})",
                                crate::diag::display_path()
                            ))
                        );
                    }
                }
                Ok(LoopControl::Continue)
            }
        }
    })
}

/// Collapse whitespace and truncate, for compact one-line progress output.
fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max {
        let head: String = collapsed.chars().take(max).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_recognizes_management_commands() {
        assert_eq!(dispatch_command("/help"), Command::Help);
        assert_eq!(dispatch_command("/config"), Command::Config);
        assert_eq!(dispatch_command("/status"), Command::Status);
        assert_eq!(dispatch_command("/diag"), Command::Diag);
        assert_eq!(dispatch_command("/quit"), Command::Quit);
        assert_eq!(dispatch_command("/exit"), Command::Quit);
    }

    #[test]
    fn dispatch_flags_unknown_slash_command() {
        assert_eq!(
            dispatch_command("/bogus"),
            Command::Unknown("/bogus".to_string())
        );
    }

    #[test]
    fn dispatch_passes_through_plain_text() {
        assert_eq!(dispatch_command("hello world"), Command::Turn);
        assert_eq!(dispatch_command("summarize the /etc dir"), Command::Turn);
    }

    #[test]
    fn approve_answer_treats_y_prefix_as_once() {
        for yes in ["y", "yes", "Y", "YES", "yeah", "  y", "y\n"] {
            assert_eq!(approve_answer(yes), ApprovalAnswer::Once, "for {yes:?}");
        }
    }

    #[test]
    fn approve_answer_maps_session_and_always() {
        for s in ["s", "session", "S", "  s"] {
            assert_eq!(approve_answer(s), ApprovalAnswer::Session, "for {s:?}");
        }
        for a in ["a", "always", "A", "a\n"] {
            assert_eq!(approve_answer(a), ApprovalAnswer::Always, "for {a:?}");
        }
    }

    #[test]
    fn approve_answer_defaults_to_deny() {
        for no in ["", "n", "no", "N", "find", "\n", "  ", "1"] {
            assert_eq!(approve_answer(no), ApprovalAnswer::Deny, "for {no:?}");
        }
    }

    #[test]
    fn approver_policy_prompts_only_on_interactive_non_auto() {
        // Prompt only when interactive AND not --auto-approve.
        assert!(use_prompt_approver(false, true));
        // --auto-approve, non-interactive, or both → AllowAll (no prompt).
        assert!(!use_prompt_approver(true, true));
        assert!(!use_prompt_approver(false, false));
        assert!(!use_prompt_approver(true, false));
    }

    // `parse_model_command`/`fuzzy_model_candidates` themselves are covered
    // by `commands::model_list`'s own tests; the shared `ModelReplHelper`
    // (Completer/Hinter) is now covered by `commands::repl`'s tests, since
    // that's where it lives.
}

#[cfg(test)]
#[path = "../tests/selected_turn.rs"]
mod selected_turn_contract;

#[cfg(test)]
#[path = "../tests/repl_lifecycle.rs"]
mod repl_lifecycle_contract;
