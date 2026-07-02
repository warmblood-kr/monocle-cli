//! `monocle agent` — experimental headless agent loop (Path B, monocle-cli#44).
//!
//! Wires the real provider (monocle routing) + the default toolset (read/write/
//! edit + cross-platform shell) into the agent-core loop, prints progress to
//! stderr, and the final answer to stdout.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::Value;

use crate::agent::commands::{config_text, help_text, login_view, status_text};
use crate::agent::providers::{Message, MonocleProvider};
use crate::agent::runner::{Agent, AllowAll, Cancel, Observer};
use crate::agent::session::{session_path, SessionStore};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::agent::SYSTEM_PROMPT;
use crate::auth::get_access_token;
use crate::colors as c;
use crate::credentials::Credentials;
use crate::error::Result;
use crate::net::Client;
use crate::util::home_dir;

pub struct AgentOptions {
    pub prompt: Option<String>,
    pub workdir: Option<String>,
    pub model: String,
    pub max_steps: usize,
    pub session: Option<String>,
}

struct CliObserver;
impl Observer for CliObserver {
    fn on_text_delta(&mut self, delta: &str) {
        // Stream assistant text to stdout as it arrives (tool progress goes to stderr).
        let mut out = std::io::stdout();
        let _ = out.write_all(delta.as_bytes());
        let _ = out.flush();
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
        // Fresh token each turn (get_access_token refreshes if near expiry).
        let auth = get_access_token(self.client, self.creds);
        let provider = MonocleProvider::from_session(auth);
        let agent = Agent::with_max_steps(
            &provider,
            &self.tools,
            ToolContext::new(self.workdir.clone()).with_cancel(self.cancel.clone()),
            self.model.as_str(),
            self.max_steps,
        );

        // Clear any cancel from an idle Ctrl-C press before starting this turn.
        self.cancel.reset();
        let mark = self.convo.len();
        self.convo.push(Message::user(user));
        if let Err(e) = agent.run(
            &mut self.convo,
            &mut AllowAll,
            &mut CliObserver,
            &self.cancel,
        ) {
            // Roll back this failed turn's (partial) messages so a retry — and the
            // persisted session — stay well-formed.
            self.convo.truncate(mark);
            return Err(e);
        }

        let mut out = std::io::stdout();
        out.write_all(b"\n")?;
        out.flush()?;

        // Persist this turn's new messages (append-only).
        if let Some(store) = &self.session {
            store.append(&self.convo[self.persisted..])?;
            self.persisted = self.convo.len();
        }
        Ok(())
    }
}

/// A slash command typed into the interactive REPL. Management commands are
/// handled locally (never sent to the agent/LLM); anything else is a turn.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Config,
    Status,
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
        "/exit" | "/quit" => Command::Quit,
        other => Command::Unknown(other.to_string()),
    }
}

pub fn agent_command(client: &Client, creds: &Credentials, opts: AgentOptions) -> Result<()> {
    let workdir = opts
        .workdir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    eprintln!(
        "{} {}",
        c::dim("agent · workdir"),
        c::dim(&workdir.display().to_string())
    );
    eprintln!(
        "{}",
        c::yellow("⚠ experimental: tools auto-approved (read/write/edit + shell). Run only in a directory you trust.")
    );

    // Ctrl-C cancels the *current turn* (not the process); Ctrl-D / /exit quits.
    let cancel = Cancel::new();
    let _ = ctrlc::set_handler({
        let c = cancel.clone();
        move || c.cancel()
    });

    // Optional named session: resume by replaying the persisted conversation.
    let session: Option<SessionStore> = opts
        .session
        .as_ref()
        .map(|name| SessionStore::new(session_path(&home_dir(), name)));
    let (convo, persisted) = match &session {
        Some(store) => {
            let loaded = store.load()?;
            if loaded.is_empty() {
                (vec![Message::system(SYSTEM_PROMPT)], 0)
            } else {
                eprintln!(
                    "{}",
                    c::dim(&format!("resumed session ({} messages)", loaded.len()))
                );
                let n = loaded.len();
                (loaded, n)
            }
        }
        None => (vec![Message::system(SYSTEM_PROMPT)], 0),
    };

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
    };

    let interactive = std::io::stdin().is_terminal();

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
            "Interactive — ↑/↓ history, ←/→ & Ctrl-A/E/K/Y editing; /help for commands; Ctrl-C aborts the turn; Ctrl-D or /exit to quit."
        )
    );

    // rustyline gives the input line proper editing (←/→, ↑/↓ history, Emacs
    // Ctrl-A/E/K/Y) with its default Emacs keymap + file history. It is used
    // ONLY here, on the interactive TTY path — the headless / piped / ACP paths
    // above never touch it, keeping the engine UI-free (CLAUDE.md 설계 원칙:
    // rich UI is the host's job via `monocle acp`). This is input-line editing
    // only; it does not take over scrollback/paging.
    let mut rl = DefaultEditor::new().map_err(|e| crate::error::AppError::new(e.to_string()))?;
    let history_path = home_dir().join(".monocle").join("agent_history");
    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Best-effort history persistence: a missing/unreadable file just means an
    // empty history this session.
    let _ = rl.load_history(&history_path);

    // rustyline reads the prompt in raw mode, where Ctrl-C surfaces as
    // `Interrupted` WITHOUT raising SIGINT — so the `ctrlc` turn-cancel handler
    // is not triggered at the idle prompt (we just start a fresh line). During a
    // turn the terminal is back in cooked mode, so Ctrl-C raises SIGINT and that
    // handler cancels the running turn, exactly as before.
    let prompt = format!("{} ", c::cyan("»"));
    loop {
        match rl.readline(&prompt) {
            Ok(line) => {
                let msg = line.trim();
                if msg.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line.as_str());
                // Slash commands are handled locally (never sent to the agent/LLM),
                // and printed to stderr so stdout stays the clean answer channel.
                match dispatch_command(msg) {
                    Command::Quit => {
                        eprintln!("Bye.");
                        break;
                    }
                    Command::Help => eprintln!("{}", help_text()),
                    Command::Config => eprintln!(
                        "{}",
                        config_text(
                            &repl.model,
                            repl.max_steps,
                            &repl.workdir,
                            repl.session_name()
                        )
                    ),
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
                    }
                    Command::Unknown(cmd) => {
                        eprintln!("unknown command: {cmd} (try /help)");
                    }
                    Command::Turn => {
                        // A transient per-turn error must not tear down the session.
                        if let Err(e) = repl.run_turn(msg.to_string()) {
                            eprintln!("{} {e}", c::red("Error:"));
                        }
                    }
                }
            }
            // Ctrl-C at the idle prompt: don't quit — just start a fresh prompt.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D (EOF): quit, matching the previous read_line behavior.
            Err(ReadlineError::Eof) => {
                eprintln!("Bye.");
                break;
            }
            Err(e) => {
                eprintln!("{} {e}", c::red("Error:"));
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
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
}
