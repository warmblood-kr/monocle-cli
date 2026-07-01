//! `monocle agent` — experimental headless agent loop (Path B, monocle-cli#44).
//!
//! Wires the real provider (monocle routing) + the default toolset (read/write/
//! edit + cross-platform shell) into the agent-core loop, prints progress to
//! stderr, and the final answer to stdout.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use serde_json::Value;

use crate::agent::providers::{LlmProvider, Message, MonocleProvider};
use crate::agent::runner::{Agent, AgentConfig, AllowAll, Cancel, Observer};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::auth::get_access_token;
use crate::colors as c;
use crate::credentials::Credentials;
use crate::error::Result;
use crate::net::Client;

const SYSTEM_PROMPT: &str = "You are Monocle's headless agent. Use the provided tools \
(read_file, write_file, edit_file, and the shell) to accomplish the user's task within the \
working directory. Take minimal, verified steps. When finished, give a brief summary.";

pub struct AgentOptions {
    pub prompt: Option<String>,
    pub workdir: Option<String>,
    pub model: String,
    pub max_steps: usize,
}

struct CliObserver;
impl Observer for CliObserver {
    fn on_text_delta(&mut self, delta: &str) {
        // Stream assistant text to stdout as it arrives (tool progress goes to stderr).
        let mut out = std::io::stdout();
        let _ = out.write_all(delta.as_bytes());
        let _ = out.flush();
    }
    fn on_tool_call(&mut self, name: &str, args: &Value) {
        eprintln!(
            "\n{} {} {}",
            c::cyan("⏵"),
            c::bold(name),
            c::dim(&one_line(&args.to_string(), 120))
        );
    }
    fn on_tool_result(&mut self, _name: &str, outcome: &ToolOutcome) {
        let tag = if outcome.is_error {
            c::red("✗")
        } else {
            c::green("✓")
        };
        eprintln!("  {} {}", tag, c::dim(&one_line(outcome.ui_text(), 200)));
    }
}

pub fn agent_command(client: &Client, creds: &Credentials, opts: AgentOptions) -> Result<()> {
    let session = get_access_token(client, creds);
    let provider = MonocleProvider::from_session(session);
    let tools = ToolRegistry::with_defaults();

    let workdir = opts
        .workdir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let mut config = AgentConfig::new(opts.model);
    config.max_steps = opts.max_steps;
    let agent = Agent::new(&provider, &tools, ToolContext::new(workdir.clone()), config);

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

    let interactive = std::io::stdin().is_terminal();
    let mut convo = vec![Message::system(SYSTEM_PROMPT)];

    // Seed the first turn from the prompt arg, or (non-interactive) piped stdin.
    if let Some(p) = opts.prompt {
        run_turn(&agent, &mut convo, p, &cancel)?;
    } else if !interactive {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        if input.trim().is_empty() {
            eprintln!("No input provided.");
            std::process::exit(1);
        }
        run_turn(&agent, &mut convo, input.trim().to_string(), &cancel)?;
    }

    // Piped / one-shot: done. Interactive: keep taking follow-ups in the session.
    if !interactive {
        return Ok(());
    }

    eprintln!(
        "{}",
        c::dim("Interactive — Ctrl-C aborts the turn; Ctrl-D or /exit to quit.")
    );
    let stdin = std::io::stdin();
    loop {
        eprint!("\n{} ", c::cyan("»"));
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            eprintln!("\nBye.");
            break;
        }
        let msg = line.trim();
        if msg.is_empty() {
            continue;
        }
        if msg == "/exit" || msg == "/quit" {
            eprintln!("Bye.");
            break;
        }
        run_turn(&agent, &mut convo, msg.to_string(), &cancel)?;
    }
    Ok(())
}

/// Run one user turn to completion: append the message, stream the agent's
/// answer to stdout (via the observer), and terminate the streamed line.
fn run_turn<P: LlmProvider>(
    agent: &Agent<'_, P>,
    convo: &mut Vec<Message>,
    user: String,
    cancel: &Cancel,
) -> Result<()> {
    // Clear any cancel from an idle Ctrl-C press before starting this turn.
    cancel.reset();
    convo.push(Message::user(user));
    agent.run(convo, &mut AllowAll, &mut CliObserver, cancel)?;
    let mut out = std::io::stdout();
    out.write_all(b"\n")?;
    out.flush()?;
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
