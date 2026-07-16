//! Shared interactive-REPL scaffolding for `monocle chat` and `monocle agent`.
//!
//! Both commands independently built a bracketed-paste rustyline `Editor`,
//! loaded/saved a history file, and drove the same Ctrl-C/Ctrl-D/read-eval
//! loop — this factors that duplication into one place so the two REPLs
//! can't drift on the parts that should behave identically (paste handling,
//! interrupt/EOF semantics, error reporting).

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::{CompletionType, Config, Context, Editor, Helper, Validator};
use std::borrow::Cow;
use std::path::PathBuf;

use crate::colors as c;
use crate::commands::model_list::fuzzy_model_candidates;
use crate::error::Result;

/// Shared REPL prompt for both `monocle chat` and `monocle agent` — unifies
/// what were previously two different carets ("> " and "» "). Plain ASCII-
/// safe text only: rustyline computes prompt width by skipping recognized
/// ANSI escapes, but on Windows consoles without VT processing enabled those
/// escapes print as literal bytes instead, desyncing the cursor column from
/// what rustyline thinks it is. Mode identity is signaled by a colored line
/// printed BEFORE the REPL starts (see chat.rs/agent.rs), not by coloring
/// this prompt.
pub const PROMPT: &str = "\u{276F} "; // ❯ — the modern CLI prompt glyph (Starship/Spaceship convention), replacing the old "$"/">" default.

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

/// rustyline line helper shared by `monocle chat`'s and `monocle agent`'s
/// REPLs (`agent.rs`'s former `ReplHelper` and `chat.rs`'s former
/// `ChatReplHelper` were near-verbatim duplicates, differing only in which
/// slash commands they offer): Tab-completes slash commands as a real
/// dropdown (see `run_repl`'s `CompletionType::List`), and (for `/model`)
/// fuzzy-completes the argument against a model id list fetched once at REPL
/// startup. Also hints the best-ranked candidate as dim inline ghost text via
/// `Hinter`/`Highlighter`; validation stays the derived no-op default
/// (multi-line handling comes from `bracketed_paste`).
#[derive(Helper, Validator)]
pub struct ModelReplHelper {
    /// The slash commands this REPL understands — `agent`'s richer set vs.
    /// `chat`'s `/model`, `/exit`, `/quit` — offered by command-name
    /// completion and shown by `/help` where applicable.
    pub commands: &'static [&'static str],
    /// Model ids fetched once before the loop starts. Empty when not logged
    /// in / the fetch failed — `/model` completion then just offers no
    /// candidates, same defensive posture as the rest of the interactive
    /// path. Plain `Vec` (not `Rc`): each helper is constructed exactly once
    /// and moved by value into `Editor::set_helper`, never cloned or shared.
    pub models: Vec<String>,
}

impl ModelReplHelper {
    /// Shared by `Completer::complete` and `Hinter::hint` so the two can
    /// never disagree on what's being completed.
    fn candidates(&self, line: &str, pos: usize) -> (usize, Vec<Pair>) {
        // Only offer completion for a slash-command being typed at the line start.
        let head = &line[..pos];
        if !head.starts_with('/') {
            return (0, Vec::new());
        }
        // `/model <partial>` — fuzzy-match the argument against the cached
        // model ids instead of the flat command-name list, so `/model
        // cla<TAB>` narrows to matching ids and a bare `/model <TAB>` shows
        // the full dropdown. Guarded (not a bare `head.strip_prefix("/model
        // ")`) so a double space (`/model  cla`) still trims down to the
        // right query instead of leaving a leading space that breaks the
        // fuzzy match, while `/models` (no separating space) still falls
        // through to plain command-name completion instead of being
        // misread as `/model` with argument `s` — the same boundary the
        // real dispatcher (`parse_model_command`) draws.
        if let Some(rest) = head.strip_prefix("/model") {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                let query = rest.trim_start();
                let start = head.len() - query.len();
                return (start, fuzzy_model_candidates(&self.models, query));
            }
        }
        let candidates = self
            .commands
            .iter()
            .filter(|cmd| cmd.starts_with(head))
            .map(|cmd| Pair {
                display: cmd.to_string(),
                replacement: cmd.to_string(),
            })
            .collect();
        // Replace from column 0 (the whole slash token starts there).
        (0, candidates)
    }
}

impl Completer for ModelReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        Ok(self.candidates(line, pos))
    }
}

impl Hinter for ModelReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        let (start, candidates) = self.candidates(line, pos);
        hint_from_candidates(line, start, pos, &candidates)
    }
}

impl Highlighter for ModelReplHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        dim_hint(hint)
    }
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
    use rustyline::history::DefaultHistory;

    /// A representative command list — not either real `SLASH_COMMANDS`
    /// (`agent.rs`) or `CHAT_SLASH_COMMANDS` (`chat.rs`), just enough
    /// variety to exercise `ModelReplHelper` generically.
    const TEST_COMMANDS: &[&str] = &["/help", "/model", "/exit", "/quit"];

    fn model_ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn helper(models: &[&str]) -> ModelReplHelper {
        ModelReplHelper {
            commands: TEST_COMMANDS,
            models: model_ids(models),
        }
    }

    #[test]
    fn complete_slash_command_narrows_to_matching_prefix() {
        let h = helper(&[]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/qu";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec!["/quit".to_string()]
        );
    }

    #[test]
    fn complete_bare_slash_offers_all_commands() {
        let h = helper(&[]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec![
                "/help".to_string(),
                "/model".to_string(),
                "/exit".to_string(),
                "/quit".to_string(),
            ]
        );
    }

    #[test]
    fn complete_non_slash_text_offers_nothing() {
        let h = helper(&[]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "hello";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert!(candidates.is_empty());
    }

    /// The completion `start` offset is what rustyline uses to splice the
    /// chosen candidate back into the line — get it wrong and accepting a
    /// completion corrupts the line. For `/model <partial>` it must point
    /// just past `"/model "`, not column 0 (unlike the command-name path),
    /// so accepting a candidate replaces only the partial id.
    #[test]
    fn complete_model_argument_uses_offset_after_prefix() {
        let h = helper(&["claude-sonnet-4-6", "gpt-4o"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/model cla";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, "/model ".len());
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec!["claude-sonnet-4-6".to_string()]
        );

        // Bare "/model " (empty partial) offers the full dropdown, still
        // anchored right after the prefix.
        let line = "/model ";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, "/model ".len());
        assert_eq!(candidates.len(), 2);
    }

    /// Command-name completion (`/mo<TAB>` → `/model`) must be unaffected by
    /// the `/model` argument branch.
    #[test]
    fn complete_command_name_unaffected_by_model_argument_branch() {
        let h = helper(&["claude-sonnet-4-6"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/mo";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec!["/model".to_string()]
        );
    }

    /// FIX #2: a double space after `/model` must not leave a leading space
    /// in the extracted query — that would break the fuzzy match even
    /// though `parse_model_command` (the real dispatcher) already tolerates
    /// it via its own final `.trim()`.
    #[test]
    fn complete_model_argument_tolerates_double_space() {
        let h = helper(&["claude-sonnet-4-6", "gpt-4o"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/model  cla";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, line.len() - "cla".len());
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec!["claude-sonnet-4-6".to_string()]
        );
    }

    /// `/models` (no separating space) must not be misread as `/model` with
    /// argument `s` — same boundary `parse_model_command` draws.
    #[test]
    fn complete_does_not_treat_slash_models_as_model_argument() {
        let h = helper(&["claude-sonnet-4-6"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/models";
        let (start, candidates) = h.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert!(candidates.is_empty());
    }

    /// FIX #1: `fuzzy_model_candidates` matches case-insensitively, so the
    /// top candidate for an all-caps query can differ in case from what was
    /// typed — the hint must still appear, using the candidate's own
    /// casing for the ghost suffix.
    #[test]
    fn hint_shows_case_insensitive_prefix_match_with_candidate_casing() {
        let h = helper(&["claude-sonnet-4-6", "gpt-4o"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/model CLA";
        assert_eq!(
            h.hint(line, line.len(), &ctx),
            Some("ude-sonnet-4-6".to_string())
        );
    }

    /// A genuine subsequence (non-prefix) fuzzy match has no sensible ghost
    /// suffix — `None` is correct, not a stripped/garbled remainder.
    #[test]
    fn hint_is_none_for_non_prefix_subsequence_match() {
        let h = helper(&["claude-sonnet-4-6"]);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/model sonnet";
        assert_eq!(h.hint(line, line.len(), &ctx), None);
    }
}
