//! Shared interactive-REPL scaffolding for `monocle chat` and `monocle agent`.
//!
//! Both commands independently built a bracketed-paste rustyline `Editor`,
//! loaded/saved a history file, and drove the same Ctrl-C/Ctrl-D/read-eval
//! loop — this factors that duplication into one place so the two REPLs
//! can't drift on the parts that should behave identically (paste handling,
//! interrupt/EOF semantics, error reporting).

use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, Editor, Helper};
use std::path::PathBuf;

use crate::colors as c;
use crate::error::Result;

/// What the per-line callback wants the loop to do next.
pub enum LoopControl {
    Continue,
    Quit,
}

/// Drives an interactive REPL to completion.
///
/// Builds the `Editor` with `bracketed_paste(true)` (a multi-line paste
/// arrives as one input buffer, submitted only on Enter, instead of each
/// embedded newline being read as a separate accept-line), loads history from
/// `history_path` (best-effort — a missing/unreadable file just means an
/// empty history this session) and saves it back on exit, then loops on
/// `rl.readline(prompt)`.
///
/// `on_line` receives each non-empty trimmed line (already added to history)
/// and returns whether the loop should continue or quit. Ctrl-C at the idle
/// prompt just starts a fresh line (rustyline surfaces it as `Interrupted`
/// without raising SIGINT); Ctrl-D (EOF) quits with a "Bye." message. `prompt`
/// must be plain text with no raw ANSI escapes — see `agent.rs`'s prompt
/// comment for why (rustyline's cursor-column math assumes 0-width escapes,
/// which breaks when a terminal doesn't actually interpret them as color).
pub fn run_repl<H: Helper>(
    helper: H,
    history_path: PathBuf,
    prompt: &str,
    mut on_line: impl FnMut(&str) -> Result<LoopControl>,
) -> Result<()> {
    let config = Config::builder().bracketed_paste(true).build();
    let mut rl: Editor<H, DefaultHistory> =
        Editor::with_config(config).map_err(|e| crate::error::AppError::new(e.to_string()))?;
    rl.set_helper(Some(helper));
    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.load_history(&history_path);

    loop {
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line.as_str());
                match on_line(trimmed)? {
                    LoopControl::Continue => {}
                    LoopControl::Quit => break,
                }
            }
            Err(ReadlineError::Interrupted) => continue,
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
