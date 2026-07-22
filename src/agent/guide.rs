//! Agent guide files — user instructions layered into the system prompt.
//!
//! When `monocle agent` starts it reads a guide file from two locations, in order,
//! and appends them to the system prompt so personal- and project-level
//! instructions steer the agent:
//!
//! 1. **personal** — `~/.monocle/` (applies to every project)
//! 2. **project** — `<workdir>/` (the directory the agent runs in)
//!
//! Loaded in that order so the project guide comes last (more specific — by
//! convention it wins on conflict). Home and workdir are passed in explicitly
//! (never resolved here) so tests can thread temp dirs.
//!
//! ## Which file
//!
//! In each location the first existing, non-blank name in [`GUIDE_FILENAMES`]
//! wins — **at most one file per directory**, matching Codex's rule. `AGENTS.md`
//! is the cross-tool open convention (Codex, Cursor, Amp, Jules, Gemini, …, now
//! stewarded by the Linux Foundation's Agentic AI Foundation); the rest are
//! honored so an existing repo works with no new file. Note there is no
//! `CODEX.md` — Codex reads `AGENTS.md`.
//!
//! ## Imports (`@path`)
//!
//! The AGENTS.md convention itself defines no preprocessing — it is plain
//! Markdown. `@path` imports are an extension that Claude Code, Gemini CLI, and
//! Amp each invented independently; we follow **Claude Code's semantics**, the
//! most precisely specified of the three:
//!
//! - `@path/to/file` anywhere outside code spans and fenced code blocks
//! - relative paths resolve against **the directory of the file containing the
//!   import**, not the working directory; `~/` means home; absolute paths work
//! - imported files may themselves import, to a depth of [`MAX_IMPORT_DEPTH`]
//! - backtick-wrapping escapes it: `` `@README` `` stays literal
//! - a missing / unreadable import is left in place as literal text
//!
//! Unlike slash-command files, guide files support **no command execution** —
//! imports are the only preprocessing, exactly as in Claude Code's CLAUDE.md.

use std::path::{Path, PathBuf};

/// Guide filenames tried in each location, highest priority first.
pub const GUIDE_FILENAMES: &[&str] = &["AGENTS.md", "AGENT.md", "CLAUDE.md", "GEMINI.md"];

/// Import recursion limit — matches Claude Code's documented 4 hops.
pub const MAX_IMPORT_DEPTH: usize = 4;

/// One loaded guide file, ready to append to the system prompt.
pub struct Guide {
    /// Human label for logs and the prompt header, e.g. `project (AGENTS.md)`.
    pub label: String,
    /// File contents, with `@path` imports already expanded.
    pub text: String,
}

/// Read the guide files that exist, in load order (personal, then project).
pub fn load_guides(home: &Path, workdir: &Path) -> Vec<Guide> {
    let locations = [
        ("personal", home.join(".monocle")),
        ("project", workdir.to_path_buf()),
    ];
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for (scope, dir) in locations {
        let Some((path, text)) = first_guide(&dir) else {
            continue;
        };
        // Guard the case where both locations resolve to the same file.
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());

        let expanded = expand_imports(&text, &dir, home, 0);
        let trimmed = expanded.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(Guide {
            label: format!("{scope} ({name})"),
            text: trimmed.to_string(),
        });
    }
    out
}

/// Append the loaded `guides` to `base`, each under a labeled header so the model
/// can tell instruction sources apart. With no guides, `base` is returned as-is.
pub fn augment_system_prompt(base: &str, guides: &[Guide]) -> String {
    let mut s = String::from(base);
    for g in guides {
        s.push_str(&format!("\n\n# Instructions from {}\n{}", g.label, g.text));
    }
    s
}

/// The first existing, non-blank guide file in `dir`, by [`GUIDE_FILENAMES`] priority.
fn first_guide(dir: &Path) -> Option<(PathBuf, String)> {
    GUIDE_FILENAMES.iter().find_map(|name| {
        let path = dir.join(name);
        let text = std::fs::read_to_string(&path).ok()?;
        (!text.trim().is_empty()).then_some((path, text))
    })
}

/// Expand `@path` imports in `text`. `base_dir` is the directory of the file the
/// text came from — relative imports resolve against it. Code fences are copied
/// through untouched, and recursion stops at [`MAX_IMPORT_DEPTH`].
fn expand_imports(text: &str, base_dir: &Path, home: &Path, depth: usize) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && depth < MAX_IMPORT_DEPTH {
            out.push_str(&expand_line(line, base_dir, home, depth));
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Expand imports in one line, leaving inline code spans (`` `…` ``) literal.
fn expand_line(line: &str, base_dir: &Path, home: &Path, depth: usize) -> String {
    let mut out = String::new();
    for (i, seg) in line.split('`').enumerate() {
        if i > 0 {
            out.push('`');
        }
        if i % 2 == 1 {
            out.push_str(seg); // inside a code span — literal
        } else {
            out.push_str(&expand_segment(seg, base_dir, home, depth));
        }
    }
    out
}

/// Expand every `@path` token in a stretch of non-code text.
fn expand_segment(seg: &str, base_dir: &Path, home: &Path, depth: usize) -> String {
    let mut out = String::new();
    let mut cursor = 0; // start of not-yet-copied text
    let mut search = 0;
    while let Some(rel) = seg[search..].find('@') {
        let at = search + rel;
        // `@` must start a token (segment start or after whitespace) so an email
        // address like `a@b.com` is never mistaken for an import.
        let boundary = at == 0
            || seg[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        let end = seg[at + 1..]
            .find(char::is_whitespace)
            .map(|p| at + 1 + p)
            .unwrap_or(seg.len());
        let raw = &seg[at + 1..end];
        if !boundary || raw.is_empty() {
            search = at + 1;
            continue;
        }
        match read_import(raw, base_dir, home) {
            Some((content, dir)) => {
                out.push_str(&seg[cursor..at]);
                out.push_str(expand_imports(&content, &dir, home, depth + 1).trim_end());
                cursor = end;
                search = end;
            }
            // Missing / unreadable import: leave the text exactly as written.
            None => search = at + 1,
        }
    }
    out.push_str(&seg[cursor..]);
    out
}

/// Resolve an import path (`~/…` = home, relative = against the importing file's
/// directory, absolute as-is) and read it. Also returns the directory that file's
/// own imports resolve against.
fn read_import(raw: &str, base_dir: &Path, home: &Path) -> Option<(String, PathBuf)> {
    let path = match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => {
            let p = Path::new(raw);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                base_dir.join(p)
            }
        }
    };
    let text = std::fs::read_to_string(&path).ok()?;
    let dir = path.parent().unwrap_or(base_dir).to_path_buf();
    Some((text, dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn no_guides_leaves_prompt_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let guides = load_guides(home.path(), work.path());
        assert!(guides.is_empty());
        assert_eq!(augment_system_prompt("BASE", &guides), "BASE");
    }

    #[test]
    fn loads_personal_then_project_in_order() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&home.path().join(".monocle").join("AGENTS.md"), "be terse");
        write(&work.path().join("AGENTS.md"), "use tabs");
        let guides = load_guides(home.path(), work.path());
        assert_eq!(guides.len(), 2);
        assert!(guides[0].label.starts_with("personal"));
        assert!(guides[1].label.starts_with("project"));
        let prompt = augment_system_prompt("BASE", &guides);
        assert!(prompt.starts_with("BASE"));
        assert!(prompt.find("be terse").unwrap() < prompt.find("use tabs").unwrap());
    }

    #[test]
    fn skips_missing_and_blank_files() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "   \n  ");
        assert!(load_guides(home.path(), work.path()).is_empty());
    }

    #[test]
    fn agents_md_wins_over_lower_priority_names() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "from agents");
        write(&work.path().join("CLAUDE.md"), "from claude");
        write(&work.path().join("GEMINI.md"), "from gemini");
        let guides = load_guides(home.path(), work.path());
        assert_eq!(guides.len(), 1, "at most one file per directory");
        assert_eq!(guides[0].text, "from agents");
        assert_eq!(guides[0].label, "project (AGENTS.md)");
    }

    #[test]
    fn falls_back_through_the_filename_chain() {
        for (present, expected) in [
            ("AGENT.md", "project (AGENT.md)"),
            ("CLAUDE.md", "project (CLAUDE.md)"),
            ("GEMINI.md", "project (GEMINI.md)"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            write(&work.path().join(present), "hello");
            let guides = load_guides(home.path(), work.path());
            assert_eq!(guides.len(), 1, "{present} should be picked up");
            assert_eq!(guides[0].label, expected);
        }
    }

    #[test]
    fn expands_imports_relative_to_the_importing_file() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "top\n@docs/style.md\nend");
        write(&work.path().join("docs").join("style.md"), "STYLE RULES");
        let guides = load_guides(home.path(), work.path());
        assert!(guides[0].text.contains("STYLE RULES"));
        assert!(!guides[0].text.contains("@docs/style.md"));
    }

    #[test]
    fn nested_imports_resolve_against_their_own_directory() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "@docs/a.md");
        // `b.md` is referenced relative to docs/, not the workdir.
        write(&work.path().join("docs").join("a.md"), "A\n@b.md");
        write(&work.path().join("docs").join("b.md"), "B");
        let text = &load_guides(home.path(), work.path())[0].text;
        assert!(text.contains('A') && text.contains('B'));
    }

    #[test]
    fn home_import_expands_tilde() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "@~/shared/rules.md");
        write(&home.path().join("shared").join("rules.md"), "SHARED");
        assert!(load_guides(home.path(), work.path())[0]
            .text
            .contains("SHARED"));
    }

    #[test]
    fn imports_inside_code_are_left_literal() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(
            &work.path().join("AGENTS.md"),
            "escaped `@secret.md` here\n```\n@secret.md\n```",
        );
        write(&work.path().join("secret.md"), "LEAKED");
        let text = &load_guides(home.path(), work.path())[0].text;
        assert!(
            !text.contains("LEAKED"),
            "code spans/fences must not import"
        );
        assert!(text.contains("`@secret.md`"));
    }

    #[test]
    fn missing_import_is_left_as_written() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(&work.path().join("AGENTS.md"), "see @nope.md ok");
        assert_eq!(
            load_guides(home.path(), work.path())[0].text,
            "see @nope.md ok"
        );
    }

    #[test]
    fn email_like_text_is_not_an_import() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        write(
            &work.path().join("AGENTS.md"),
            "ask jeongsoo@warmblood.kr ok",
        );
        assert_eq!(
            load_guides(home.path(), work.path())[0].text,
            "ask jeongsoo@warmblood.kr ok"
        );
    }

    #[test]
    fn import_recursion_stops_at_max_depth() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        // A self-importing file must terminate rather than recurse forever.
        write(&work.path().join("AGENTS.md"), "LOOP\n@AGENTS.md");
        let text = &load_guides(home.path(), work.path())[0].text;
        assert_eq!(text.matches("LOOP").count(), MAX_IMPORT_DEPTH + 1);
    }
}
