//! `monocle agent` — experimental headless agent loop (Path B, monocle-cli#44).
//!
//! Wires the real provider (monocle routing) + the default toolset (read/write/
//! edit + cross-platform shell) into the agent-core loop, prints progress to
//! stderr, and the final answer to stdout.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, Context, Editor, Helper, Highlighter, Hinter, Validator};
use serde_json::Value;

use crate::agent::commands::{config_text, help_text, login_view, status_text};
use crate::agent::providers::{Message, MonocleProvider};
use crate::agent::runner::{Agent, AllowAll, Approver, Cancel, Observer};
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
    /// Skip the per-tool approval prompt (dangerous) — run side-effecting tools
    /// unattended, as scripts/one-shot already do.
    pub auto_approve: bool,
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

/// The slash commands the REPL understands, offered by the completer and shown
/// by `/help`. Kept here (next to the dispatcher) so completion never drifts from
/// what `dispatch_command` / `parse_model_command` actually handle.
const SLASH_COMMANDS: &[&str] = &["/help", "/config", "/status", "/model", "/exit", "/quit"];

/// rustyline line helper: Tab-completes slash commands. Hinting/highlighting/
/// validation are the derived no-op defaults — we only customize completion.
#[derive(Helper, Hinter, Highlighter, Validator)]
struct ReplHelper;

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Only offer completion for a slash-command being typed at the line start.
        let head = &line[..pos];
        if !head.starts_with('/') {
            return Ok((0, Vec::new()));
        }
        let candidates = SLASH_COMMANDS
            .iter()
            .filter(|cmd| cmd.starts_with(head))
            .map(|cmd| Pair {
                display: cmd.to_string(),
                replacement: cmd.to_string(),
            })
            .collect();
        // Replace from column 0 (the whole slash token starts there).
        Ok((0, candidates))
    }
}

/// Interactive approver: prompts the user (stderr) before each side-effecting
/// tool call and reads a y/N answer from stdin. Default (empty / non-y / EOF) is
/// deny — the safe choice. Only ever constructed on an interactive TTY, where the
/// terminal is in cooked mode during a turn so a plain `read_line` works.
struct PromptApprover;
impl Approver for PromptApprover {
    fn approve(&mut self, _id: &str, name: &str, args: &Value) -> bool {
        // For `shell`, the interesting thing is the command; else show the JSON.
        let summary = if name == "shell" {
            args.get("command")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| args.to_string())
        } else {
            args.to_string()
        };
        eprint!(
            "{} {} {}: {}  {} ",
            c::yellow("⚠"),
            c::dim("run"),
            c::bold(name),
            c::dim(&one_line(&summary, 120)),
            c::yellow("[y/N]"),
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => false, // EOF → deny
            Ok(_) => approve_answer(&line),
            Err(_) => false,
        }
    }
}

/// Pure y/N decision for an approval prompt: an answer counts as "yes" only if,
/// after leading whitespace, it starts with `y`/`Y`. Everything else (empty,
/// `n`, a stray word) is "no" — the safe default.
fn approve_answer(input: &str) -> bool {
    matches!(input.trim_start().chars().next(), Some('y') | Some('Y'))
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
    /// `--auto-approve`: run side-effecting tools without prompting.
    auto_approve: bool,
    /// Whether this session runs on an interactive TTY (can prompt for approval).
    interactive: bool,
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
        // Gate side-effecting tools behind the user in interactive mode; scripts
        // / one-shot / `--auto-approve` keep the unattended `AllowAll` behavior.
        let mut allow = AllowAll;
        let mut prompt = PromptApprover;
        let approver: &mut dyn Approver =
            if use_prompt_approver(self.auto_approve, self.interactive) {
                &mut prompt
            } else {
                &mut allow
            };
        if let Err(e) = agent.run(&mut self.convo, approver, &mut CliObserver, &self.cancel) {
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

/// Pure parser for the `/model` command. Returns:
/// - `None` — not a `/model` invocation (let normal dispatch handle it);
/// - `Some(None)` — bare `/model` (show the current model);
/// - `Some(Some(id))` — `/model <id>` (switch to `id`).
fn parse_model_command(input: &str) -> Option<Option<String>> {
    let trimmed = input.trim();
    if trimmed == "/model" {
        return Some(None);
    }
    let rest = trimmed.strip_prefix("/model ")?.trim();
    if rest.is_empty() {
        Some(None)
    } else {
        Some(Some(rest.to_string()))
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
        c::dim("agent · workdir"),
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
        auto_approve: opts.auto_approve,
        interactive,
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

    // rustyline gives the input line proper editing (←/→, ↑/↓ history, Emacs
    // Ctrl-A/E/K/Y) with its default Emacs keymap + file history. It is used
    // ONLY here, on the interactive TTY path — the headless / piped / ACP paths
    // above never touch it, keeping the engine UI-free (CLAUDE.md 설계 원칙:
    // rich UI is the host's job via `monocle acp`). This is input-line editing
    // only; it does not take over scrollback/paging.
    //
    // `bracketed_paste(true)` makes a multi-line paste arrive as a SINGLE input
    // buffer (submitted only on Enter) instead of each embedded newline being read
    // as a separate accept-line → separate turn. This is already rustyline 14's
    // default (`Config::default().enable_bracketed_paste == true`), so on a
    // terminal that supports bracketed paste it worked before too; we set it
    // explicitly (via `Editor::with_config`, replacing `DefaultEditor`) to make
    // the guarantee visible and stop line-splitting regressing if the default
    // changes. The `ReplHelper` adds Tab-completion of slash commands.
    let config = Config::builder().bracketed_paste(true).build();
    let mut rl: Editor<ReplHelper, DefaultHistory> =
        Editor::with_config(config).map_err(|e| crate::error::AppError::new(e.to_string()))?;
    rl.set_helper(Some(ReplHelper));
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
                // `/model` switches the model for subsequent turns (handled before
                // the general dispatch since it carries an argument).
                if let Some(model_cmd) = parse_model_command(msg) {
                    match model_cmd {
                        Some(id) => {
                            eprintln!("{}", c::dim(&format!("model → {id}")));
                            repl.model = id;
                        }
                        None => eprintln!("{}", c::dim(&format!("model: {}", repl.model))),
                    }
                    continue;
                }
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
                        // Reset before this turn's work runs, so the hint below
                        // reflects only whether *this* turn logged a network
                        // error — not a stale flag left over from an earlier one.
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

    #[test]
    fn approve_answer_treats_y_prefix_as_yes() {
        for yes in ["y", "yes", "Y", "YES", "yeah", "  y", "y\n"] {
            assert!(approve_answer(yes), "expected yes for {yes:?}");
        }
    }

    #[test]
    fn approve_answer_defaults_to_no() {
        for no in ["", "n", "no", "N", "find", "\n", "  ", "1", "sure"] {
            assert!(!approve_answer(no), "expected no for {no:?}");
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

    #[test]
    fn parse_model_command_variants() {
        // Not a /model command.
        assert_eq!(parse_model_command("hello"), None);
        assert_eq!(parse_model_command("/models"), None);
        assert_eq!(parse_model_command("/help"), None);
        // Bare /model (with or without surrounding / trailing space) shows current.
        assert_eq!(parse_model_command("/model"), Some(None));
        assert_eq!(parse_model_command("  /model  "), Some(None));
        assert_eq!(parse_model_command("/model   "), Some(None));
        // /model <id> switches.
        assert_eq!(
            parse_model_command("/model gpt-4o"),
            Some(Some("gpt-4o".to_string()))
        );
        assert_eq!(
            parse_model_command("/model  claude-x  "),
            Some(Some("claude-x".to_string()))
        );
    }
}
