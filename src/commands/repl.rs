//! Shared interactive-REPL scaffolding for `monocle chat` and `monocle agent`.
//!
//! Both commands independently built a bracketed-paste rustyline `Editor`,
//! loaded/saved a history file, and drove the same Ctrl-C/Ctrl-D/read-eval
//! loop — this factors that duplication into one place so the two REPLs
//! can't drift on the parts that should behave identically (paste handling,
//! interrupt/EOF semantics, error reporting).

use rustyline::completion::Pair;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{CompletionType, Config, Editor, Helper};
use std::borrow::Cow;
use std::path::PathBuf;

use crate::colors as c;
use crate::error::Result;

/// What the per-line callback wants the loop to do next.
pub enum LoopControl {
    Continue,
    Quit,
}

/// Ghost-text hint shared by both REPL helpers' `Hinter` impls: given the
/// already-typed line, the cursor position, and the same `(start, candidates)`
/// pair their `Completer::complete` computed for that line/pos, returns the
/// untyped remainder of the best (first-ranked) candidate — or `None` if the
/// cursor isn't at the end of the line (mid-line edits shouldn't grow a hint
/// past the cursor) or there's nothing left to complete.
///
/// The comparison is case-insensitive (matching `fuzzy_model_candidates`'
/// `ignore_case` matcher), but the hint text itself is always the
/// candidate's own casing — so typing `/model CLA` against a cached
/// `claude-...` id still ghosts `ude-...`, not a re-cased echo of what was
/// typed. When the top candidate isn't a prefix of what's typed at all (a
/// genuine subsequence match, e.g. `/model sonnet` matching
/// `claude-sonnet-4-6`), there's no sensible ghost suffix and this returns
/// `None`.
pub fn hint_from_candidates(
    line: &str,
    start: usize,
    pos: usize,
    candidates: &[Pair],
) -> Option<String> {
    if pos != line.len() {
        return None;
    }
    let typed = &line[start..pos];
    let best = candidates.first()?;
    let candidate_head = best.replacement.get(..typed.len())?;
    if candidate_head.eq_ignore_ascii_case(typed) {
        Some(best.replacement[typed.len()..].to_string())
    } else {
        None
    }
}

/// Dim an inline hint with ANSI, so both REPL helpers' `Highlighter::
/// highlight_hint` render candidates the same washed-out gray instead of the
/// default (undimmed) hint color.
pub fn dim_hint(hint: &str) -> Cow<'_, str> {
    Cow::Owned(c::dim(hint))
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
    // `CompletionType::List` shows every matching candidate at once on Tab
    // (a dropdown), instead of the default `Circular`'s cycle-one-at-a-time —
    // the actual "dropdown" experience slash-command/model completion is
    // meant to have.
    let config = Config::builder()
        .bracketed_paste(true)
        .completion_type(CompletionType::List)
        .build();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(id: &str) -> Pair {
        Pair {
            display: id.to_string(),
            replacement: id.to_string(),
        }
    }

    /// FIX #1: `fuzzy_model_candidates` matches case-insensitively, so the
    /// top candidate for an all-caps query can differ in case from what was
    /// typed — the hint must still appear, using the candidate's own
    /// casing for the ghost suffix (not a case-sensitive `strip_prefix`,
    /// which would silently return `None` here).
    #[test]
    fn hint_shows_case_insensitive_prefix_match_with_candidate_casing() {
        let candidates = vec![pair("claude-sonnet-4-6")];
        let line = "/model CLA";
        let start = line.len() - "CLA".len();
        assert_eq!(
            hint_from_candidates(line, start, line.len(), &candidates),
            Some("ude-sonnet-4-6".to_string())
        );
    }

    /// A genuine subsequence (non-prefix) fuzzy match has no sensible ghost
    /// suffix — `None` is correct, not a stripped/garbled remainder.
    #[test]
    fn hint_is_none_for_non_prefix_subsequence_match() {
        let candidates = vec![pair("claude-sonnet-4-6")];
        let line = "/model sonnet";
        let start = line.len() - "sonnet".len();
        assert_eq!(
            hint_from_candidates(line, start, line.len(), &candidates),
            None
        );
    }
}
