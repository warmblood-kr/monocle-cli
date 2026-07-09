use std::io::{IsTerminal, Write};

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Config, Context, Editor, Helper, Highlighter, Hinter, Validator};
use serde_json::Value;

use crate::agent::providers::{
    ChatRequest, ImageAttachment, LlmProvider, Message, MonocleProvider,
};
use crate::agent::DEFAULT_MODEL;
use crate::attachment;
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::Result;
use crate::net::Client;
use crate::origin::auth_headers;
use crate::util::home_dir;

pub struct ChatOptions {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<String>,
    pub max_tokens: Option<String>,
    /// `--file <PATH|URL>` values (repeatable), one-shot only.
    pub files: Vec<String>,
}

/// Resolve the `--max-tokens` flag to an output-token limit. Returns `Some(n)`
/// only when the flag was passed AND parses as a **positive** integer; otherwise
/// `None`, so the request omits `max_tokens` and the router/model uses its own
/// (higher, model-appropriate) default. Pure: a present-but-invalid flag is
/// detectable by the caller as `flag.is_some() && resolve_max_tokens(flag) ==
/// None`, which is where the user-facing warning is emitted.
fn resolve_max_tokens(flag: Option<&str>) -> Option<i64> {
    flag.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
}

/// One streaming chat turn via the shared provider (chat-proxy routing): assistant
/// text deltas are written to stdout (and flushed) as they arrive.
fn call_chat(
    provider: &MonocleProvider,
    model: &str,
    system_prompt: Option<&str>,
    user_message: &str,
    max_tokens: Option<i64>,
    images: &[ImageAttachment],
) -> Result<()> {
    let mut messages = Vec::new();
    if let Some(sp) = system_prompt {
        messages.push(Message::system(sp));
    }
    messages.push(Message::user(user_message));

    let req = ChatRequest {
        model: model.to_string(),
        messages,
        max_tokens,
        images: images.to_vec(),
        ..Default::default()
    };
    // Acquire the stdout lock once for the whole stream rather than per token —
    // the closure writes+flushes to this single handle (per-delta flush keeps the
    // output live).
    let mut out = std::io::stdout().lock();
    let resp = provider.chat_stream(&req, &mut |delta| {
        let _ = out.write_all(delta.as_bytes());
        let _ = out.flush();
    })?;
    // A mid-stream drop was salvaged into partial output (monocle-cli#59): stdout
    // already holds the partial text, so the notice goes to stderr.
    if resp.truncated {
        eprintln!("\n⚠ the response was cut short (partial output shown).");
    }
    Ok(())
}

/// Resolve inband `file:<path>` refs in a REPL line into images, mirroring the
/// one-shot path's resolution (`attachment::extract_inband_refs` +
/// `attachment::resolve`) but returning a failure as `Err(String)` instead of
/// exiting the process — a bad ref in the REPL is a per-turn error, not a
/// reason to kill the whole session. Returns the cleaned text (`file:` tokens
/// stripped, same as the one-shot path) alongside the resolved attachments;
/// zero refs is the common case and just passes `line` through unchanged.
fn resolve_repl_attachments(
    line: &str,
) -> std::result::Result<(String, Vec<ImageAttachment>), String> {
    let (cleaned_text, refs) = attachment::extract_inband_refs(line);
    let mut images: Vec<ImageAttachment> = Vec::new();
    for r in &refs {
        match attachment::resolve(r) {
            Ok(img) => images.push(img),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok((cleaned_text.trim().to_string(), images))
}

/// The slash commands the REPL understands — just quitting, for now (chat has
/// no `/help`/`/model`/`/config`/`/status`, unlike `monocle agent`'s REPL).
const CHAT_SLASH_COMMANDS: &[&str] = &["/exit", "/quit"];

/// rustyline line helper for `monocle chat`'s REPL: Tab-completes the two
/// slash commands. Hinting/highlighting/validation are the derived no-op
/// defaults — multi-line handling comes from `bracketed_paste`, not a custom
/// `Validator`. Deliberately simpler than `agent.rs`'s `ReplHelper` (no
/// `/model`-argument fuzzy completion, no model-id list to carry).
#[derive(Helper, Hinter, Highlighter, Validator)]
struct ChatReplHelper;

impl Completer for ChatReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Only offer completion for a slash-command being typed at line start.
        let head = &line[..pos];
        if !head.starts_with('/') {
            return Ok((0, Vec::new()));
        }
        let candidates = CHAT_SLASH_COMMANDS
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

pub fn chat_command(client: &Client, creds: &Credentials, options: ChatOptions) -> Result<()> {
    let model = options
        .model
        .as_deref()
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let max_tokens_flag = options.max_tokens.as_deref();
    let max_tokens = resolve_max_tokens(max_tokens_flag);
    // The flag was given but didn't resolve to a positive integer — warn (to
    // stderr) rather than silently omitting it, so a typo isn't mistaken for the
    // model's default limit.
    if let Some(raw) = max_tokens_flag {
        if max_tokens.is_none() {
            eprintln!("⚠ ignoring --max-tokens '{raw}' (a positive integer is required)");
        }
    }

    let stdin_is_tty = std::io::stdin().is_terminal();

    // Attachments (`--file` / inband `file:<path>`) are one-shot only (piped
    // stdin), never the interactive REPL.
    if !options.files.is_empty() && stdin_is_tty {
        eprintln!(
            "--file requires piped input. Pipe your instruction, e.g.:\n  echo \"describe this image\" | monocle chat --file photo.png"
        );
        std::process::exit(1);
    }

    // Auth FIRST — before any local-input validation. Otherwise an expired/
    // missing-credentials error can be masked by a local failure that happens
    // to come first (e.g. empty piped stdin reporting "No input provided via
    // stdin." while the real problem is "Not logged in").
    let session = get_access_token(client, creds);
    let token = session.token;
    let router_url = session.router_url;
    let bearer = format!("Bearer {token}");

    // Read stdin + resolve any attachments — cheap, local-only work that
    // should fail fast (bad path, unsupported MIME) before the second network
    // call (model-ID validation) below, but after the auth check above.
    let one_shot_input: Option<(String, Vec<ImageAttachment>)> = if !stdin_is_tty {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
        let (cleaned_text, inband_refs) = attachment::extract_inband_refs(input.trim());

        let mut refs: Vec<String> = options.files.clone();
        refs.extend(inband_refs);

        let mut images: Vec<ImageAttachment> = Vec::new();
        for r in &refs {
            match attachment::resolve(r) {
                Ok(img) => images.push(img),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        let cleaned_text = cleaned_text.trim().to_string();
        // Image-only messages are valid — only bail when BOTH the text and the
        // attachments are empty.
        if cleaned_text.is_empty() && images.is_empty() {
            eprintln!("No input provided via stdin.");
            std::process::exit(1);
        }

        Some((cleaned_text, images))
    } else {
        None
    };

    // Resolve system prompt.
    let system_prompt: Option<String> = if let Some(path) = &options.system_prompt_file {
        if !std::path::Path::new(path).exists() {
            eprintln!("System prompt file not found: {path}");
            std::process::exit(1);
        }
        Some(std::fs::read_to_string(path)?)
    } else {
        options.system_prompt.clone()
    };

    // Validate the model ID against the available models (non-fatal on failure).
    if let Ok(resp) = client.get(
        &format!("{router_url}{}", endpoints::MODELS),
        &auth_headers(&bearer),
    ) {
        if resp.ok() {
            if let Ok(data) = resp.json::<Value>() {
                let ids: Vec<String> = data
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if !ids.iter().any(|id| id == &model) {
                    eprintln!("Error: Model \"{model}\" not found.");
                    eprintln!("Available models:");
                    for id in &ids {
                        eprintln!("  {id}");
                    }
                    std::process::exit(1);
                }
            }
        }
    }

    let provider = MonocleProvider::new(token, router_url.clone());

    // Non-interactive: stdin was piped (input + attachments already resolved above).
    if let Some((text, images)) = one_shot_input {
        eprintln!("Using model: {model}");
        eprintln!("Router: {router_url}");
        call_chat(
            &provider,
            &model,
            system_prompt.as_deref(),
            &text,
            max_tokens,
            &images,
        )?;
        let mut out = std::io::stdout();
        out.write_all(b"\n")?;
        out.flush()?;
        return Ok(());
    }

    // Interactive REPL.
    eprintln!("Monocle Chat (model: {model})");
    eprintln!("Router: {router_url}");
    if let Some(sp) = &system_prompt {
        eprintln!("System prompt loaded ({} chars)", sp.chars().count());
    }
    eprintln!("Type your message. Press Ctrl+D to exit.");
    eprintln!("↑/↓ history, Tab completes /quit, /exit — a multi-line paste is one input.");
    eprintln!("---");

    // rustyline gives the input line proper editing (←/→, ↑/↓ history, Emacs
    // Ctrl-A/E/K/Y) with its default Emacs keymap + file history, mirroring
    // `monocle agent`'s REPL (`src/commands/agent.rs`). `bracketed_paste(true)`
    // makes a multi-line paste arrive as a single input buffer (submitted only
    // on Enter) instead of each embedded newline being read as a separate
    // accept-line → separate turn.
    let config = Config::builder().bracketed_paste(true).build();
    let mut rl: Editor<ChatReplHelper, DefaultHistory> =
        Editor::with_config(config).map_err(|e| crate::error::AppError::new(e.to_string()))?;
    rl.set_helper(Some(ChatReplHelper));
    // Separate history file from `monocle agent`'s — the two REPLs' histories
    // stay independent.
    let history_path = home_dir().join(".monocle").join("chat_history");
    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Best-effort history persistence: a missing/unreadable file just means an
    // empty history this session.
    let _ = rl.load_history(&history_path);

    let prompt = "> ";
    loop {
        match rl.readline(prompt) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // History gets the raw typed line (before `file:` tokens are
                // stripped) — recalling it via ↑ should show what was actually
                // typed, matching the non-attachment behavior this REPL already
                // had.
                let _ = rl.add_history_entry(line.as_str());
                if trimmed == "/quit" || trimmed == "/exit" {
                    eprintln!("Bye.");
                    break;
                }

                let (text, images) = match resolve_repl_attachments(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        // Same per-turn recovery as a `call_chat` error below:
                        // print and go back to the prompt, no `call_chat` call
                        // for this turn.
                        eprintln!("Error: {e}");
                        eprintln!();
                        continue;
                    }
                };

                eprintln!();
                // Reset before this turn's work runs, so the hint below reflects
                // only whether *this* turn logged a network error — not a stale
                // flag left over from an earlier one (this REPL loops for the
                // life of the process, same as `monocle agent`'s).
                crate::diag::reset();
                match call_chat(
                    &provider,
                    &model,
                    system_prompt.as_deref(),
                    &text,
                    max_tokens,
                    &images,
                ) {
                    Ok(()) => {
                        let mut out = std::io::stdout();
                        out.write_all(b"\n\n")?;
                        out.flush()?;
                    }
                    Err(e) => {
                        eprintln!("Error: {e}");
                        if crate::diag::was_logged() {
                            eprintln!("  (details logged to {})", crate::diag::display_path());
                        }
                        eprintln!();
                    }
                }
            }
            // Ctrl-C: don't quit — just start a fresh prompt (matches `monocle
            // agent`'s idle-prompt behavior).
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D (EOF): quit, matching the previous read_line behavior.
            Err(ReadlineError::Eof) => {
                eprintln!("Bye.");
                break;
            }
            Err(e) => {
                eprintln!("Error: {e}");
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_max_tokens, resolve_repl_attachments, ChatReplHelper};
    use rustyline::completion::Completer;
    use rustyline::history::DefaultHistory;
    use rustyline::Context;

    #[test]
    fn complete_slash_command_narrows_to_matching_prefix() {
        let helper = ChatReplHelper;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/qu";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
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
    fn complete_bare_slash_offers_both_commands() {
        let helper = ChatReplHelper;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "/";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert_eq!(
            candidates
                .into_iter()
                .map(|p| p.replacement)
                .collect::<Vec<_>>(),
            vec!["/exit".to_string(), "/quit".to_string()]
        );
    }

    #[test]
    fn complete_non_slash_text_offers_nothing() {
        let helper = ChatReplHelper;
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "hello";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, 0);
        assert!(candidates.is_empty());
    }

    #[test]
    fn absent_flag_omits() {
        assert_eq!(resolve_max_tokens(None), None);
    }

    #[test]
    fn valid_integer_is_used() {
        assert_eq!(resolve_max_tokens(Some("8000")), Some(8000));
        // Surrounding whitespace is tolerated.
        assert_eq!(resolve_max_tokens(Some("  4096 ")), Some(4096));
    }

    #[test]
    fn unparseable_flag_omits() {
        assert_eq!(resolve_max_tokens(Some("x")), None);
        assert_eq!(resolve_max_tokens(Some("")), None);
    }

    #[test]
    fn non_positive_flag_omits() {
        // Present but not a positive integer → None (caller warns).
        assert_eq!(resolve_max_tokens(Some("0")), None);
        assert_eq!(resolve_max_tokens(Some("-1")), None);
        assert_eq!(resolve_max_tokens(Some("  -42 ")), None);
    }

    #[test]
    fn repl_line_with_no_file_ref_passes_through_unchanged() {
        let (text, images) = resolve_repl_attachments("hello there").unwrap();
        assert_eq!(text, "hello there");
        assert!(images.is_empty());
    }

    #[test]
    fn repl_line_with_valid_file_ref_resolves_and_strips_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"bytes").unwrap();
        let line = format!("what's in file:{} ?", path.to_str().unwrap());

        let (text, images) = resolve_repl_attachments(&line).unwrap();
        assert!(!text.contains("file:"));
        assert!(text.contains("what's in"));
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn repl_line_with_image_only_file_ref_yields_empty_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"bytes").unwrap();
        let line = format!("file:{}", path.to_str().unwrap());

        let (text, images) = resolve_repl_attachments(&line).unwrap();
        assert!(text.is_empty());
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn repl_line_with_nonexistent_file_ref_errors_without_panicking() {
        let err = resolve_repl_attachments("file:/no/such/path.png").unwrap_err();
        assert!(err.contains("not found"), "unexpected message: {err}");
    }

    #[test]
    fn repl_line_with_unsupported_extension_errors_with_typed_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.heic");
        std::fs::write(&path, b"hello").unwrap();
        let line = format!("file:{}", path.to_str().unwrap());

        let err = resolve_repl_attachments(&line).unwrap_err();
        assert!(
            err.starts_with("unsupported type:"),
            "unexpected message: {err}"
        );
    }
}
