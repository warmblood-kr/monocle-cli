//! Agent subsystem (Path B — native Rust headless agent loop).
//!
//! Design: warmblood-kr/monocle#158 (SDD) · impl: warmblood-kr/monocle-cli#44.
//! This is the §9 step-1 spike: the `providers` abstraction only. The agent loop,
//! tools, permission/sandboxing, session, and ACP surface come later.

pub mod providers;
pub mod runner;
pub mod session;
pub mod tools;

/// Shared agent defaults, so the CLI (`monocle agent`), the chat command, and the
/// ACP surface stay in lock-step instead of each carrying (and drifting) its own
/// copy.
///
/// System prompt for the headless agent loop. Referenced by both
/// `commands::agent` and `acp`.
pub const SYSTEM_PROMPT: &str = "You are Monocle's headless agent. Use the provided tools \
(read_file, write_file, edit_file, and the shell) to accomplish the user's task within the \
session's working directory. Take minimal, verified steps. When finished, give a brief summary.";

/// Default model id (routed/validated by monocle chat-proxy — we only pass it through).
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Default agent-loop step budget before giving up.
pub const DEFAULT_MAX_STEPS: usize = 20;
