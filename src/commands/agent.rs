//! `monocle agent` — experimental headless agent loop (Path B, monocle-cli#44).
//!
//! Wires the real provider (monocle routing) + the default toolset (read/write/
//! edit + cross-platform shell) into the agent-core loop, prints progress to
//! stderr, and the final answer to stdout.

use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;

use crate::agent::providers::{Message, MonocleProvider};
use crate::agent::runner::{Agent, AgentConfig, AllowAll, Observer};
use crate::agent::tools::{ToolContext, ToolOutcome, ToolRegistry};
use crate::auth::get_access_token;
use crate::colors as c;
use crate::credentials::Credentials;
use crate::error::Result;
use crate::net::Client;

const SYSTEM_PROMPT: &str = "You are Monocle's headless coding agent. Use the provided tools \
(read_file, write_file, edit_file, and the shell) to accomplish the user's task within the \
working directory. Take minimal, verified steps. When finished, give a brief summary.";

pub struct AgentOptions {
    pub prompt: String,
    pub workdir: Option<String>,
    pub model: String,
    pub max_steps: usize,
}

struct CliObserver;
impl Observer for CliObserver {
    fn on_text(&mut self, text: &str) {
        if !text.trim().is_empty() {
            eprintln!("{}", c::dim(text));
        }
    }
    fn on_tool_call(&mut self, name: &str, args: &Value) {
        eprintln!(
            "{} {} {}",
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
        eprintln!("  {} {}", tag, c::dim(&one_line(&outcome.content, 200)));
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

    let answer = agent.run(
        vec![Message::system(SYSTEM_PROMPT), Message::user(opts.prompt)],
        &mut AllowAll,
        &mut CliObserver,
    )?;

    let mut out = std::io::stdout();
    out.write_all(answer.as_bytes())?;
    out.write_all(b"\n")?;
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
