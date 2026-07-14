use std::io::{IsTerminal, Write};

use serde_json::Value;

use crate::agent::providers::{
    ChatRequest, ChatResponse, ImageAttachment, LlmProvider, Message, MonocleProvider,
};
use crate::agent::DEFAULT_MODEL;
use crate::attachment;
use crate::auth::{get_access_token, jarvice_url_for};
use crate::commands::model_list::{fetch_model_ids, handle_model_command};
use crate::commands::repl::{run_repl, LoopControl, ModelReplHelper};
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::Result;
use crate::net::Client;
use crate::origin::auth_headers;
use crate::responses_api::ResponsesClient;
use crate::util::home_dir;

pub struct ChatOptions {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<String>,
    pub max_tokens: Option<String>,
    /// `--file <PATH|URL>` values (repeatable), one-shot only.
    pub files: Vec<String>,
    /// `--responses`: talk to jarvice's `/api/responses` (server-managed
    /// thread) instead of the plain `/v1/chat/completions` path.
    pub responses: bool,
    /// `--thread <ID>`: continue an existing `--responses` thread.
    pub thread: Option<String>,
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
/// text deltas are written to stdout (and flushed) as they arrive. `messages` is
/// the full request the caller has already assembled (system prompt + any prior
/// turns + this turn) — this fn only executes the network call and returns the
/// assembled response so the caller can append it to its own running history.
fn call_chat(
    provider: &MonocleProvider,
    model: &str,
    messages: Vec<Message>,
    max_tokens: Option<i64>,
    images: &[ImageAttachment],
) -> Result<ChatResponse> {
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
    Ok(resp)
}

/// Read piped stdin (if not a TTY) and resolve `--file` + inband `file:<path>`
/// tokens into one one-shot turn — shared by both the plain-completions and
/// `--responses` paths. Returns `None` on an interactive TTY (nothing piped).
/// Exits the process on a bad attachment ref or empty input, matching this
/// command's existing fail-fast one-shot behavior.
fn read_one_shot_input(
    stdin_is_tty: bool,
    files: &[String],
) -> Result<Option<(String, Vec<ImageAttachment>)>> {
    if stdin_is_tty {
        return Ok(None);
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let (cleaned_text, inband_refs) = attachment::extract_inband_refs(input.trim());

    let mut refs: Vec<String> = files.to_vec();
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

    Ok(Some((cleaned_text, images)))
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

/// The slash commands the REPL understands. `/model` mirrors `monocle
/// agent`'s REPL (switches the model for subsequent turns); chat still has no
/// `/help`/`/config`/`/status`.
const CHAT_SLASH_COMMANDS: &[&str] = &["/model", "/exit", "/quit"];

pub fn chat_command(client: &Client, creds: &Credentials, options: ChatOptions) -> Result<()> {
    if options.responses {
        run_responses_chat(client, creds, options)
    } else {
        run_completions_chat(client, creds, options)
    }
}

/// The default path: a stateless `/v1/chat/completions` call per turn, with
/// this REPL accumulating the growing conversation itself (see `convo` below)
/// and resending it in full each turn.
fn run_completions_chat(client: &Client, creds: &Credentials, options: ChatOptions) -> Result<()> {
    let mut model = options
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
    let one_shot_input = read_one_shot_input(stdin_is_tty, &options.files)?;

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

    // Validate the model ID against the available models (non-fatal on
    // failure), and keep the full id list around — reused below as the
    // interactive REPL's `/model` completion candidates instead of a second
    // fetch (`agent.rs`'s REPL fetches separately since it has no equivalent
    // startup validation call to piggyback on).
    let mut model_ids: Vec<String> = Vec::new();
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
                model_ids = ids;
            }
        }
    }

    let provider = MonocleProvider::new(token, router_url.clone());

    // Non-interactive: stdin was piped (input + attachments already resolved above).
    if let Some((text, images)) = one_shot_input {
        eprintln!("Using model: {model}");
        eprintln!("Router: {router_url}");
        let mut messages = Vec::new();
        if let Some(sp) = &system_prompt {
            messages.push(Message::system(sp));
        }
        messages.push(Message::user(&text));
        call_chat(&provider, &model, messages, max_tokens, &images)?;
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
    eprintln!("↑/↓ history, Tab completes /model, /quit, /exit — a multi-line paste is one input.");
    eprintln!("---");

    // Separate history file from `monocle agent`'s — the two REPLs' histories
    // stay independent. The Editor build, bracketed-paste config, history
    // load/save, and the Ctrl-C/Ctrl-D/error loop itself are shared with
    // `monocle agent`'s REPL via `commands::repl::run_repl`.
    let history_path = home_dir().join(".monocle").join("chat_history");
    let helper = ModelReplHelper {
        commands: CHAT_SLASH_COMMANDS,
        models: model_ids,
    };

    // Conversation memory: mirrors `monocle agent`'s `Repl.convo` — grows for
    // the life of the process instead of `call_chat` rebuilding a one-message
    // request every turn (the REPL used to have no memory of earlier turns at
    // all). Seeded once with the system prompt, if any.
    let mut convo: Vec<Message> = Vec::new();
    if let Some(sp) = &system_prompt {
        convo.push(Message::system(sp));
    }

    run_repl(helper, history_path, "> ", |trimmed| {
        if trimmed == "/quit" || trimmed == "/exit" {
            eprintln!("Bye.");
            return Ok(LoopControl::Quit);
        }
        // `/model` switches the model for subsequent turns — handled before
        // the general dispatch since it carries an argument, same as
        // `monocle agent`'s REPL.
        if handle_model_command(trimmed, &mut model) {
            return Ok(LoopControl::Continue);
        }

        let (text, images) = match resolve_repl_attachments(trimmed) {
            Ok(v) => v,
            Err(e) => {
                // Same per-turn recovery as a `call_chat` error below: print
                // and go back to the prompt, no `call_chat` call this turn.
                eprintln!("Error: {e}");
                eprintln!();
                return Ok(LoopControl::Continue);
            }
        };

        eprintln!();
        // Reset before this turn's work runs, so the hint below reflects only
        // whether *this* turn logged a network error — not a stale flag left
        // over from an earlier one (this REPL loops for the life of the
        // process, same as `monocle agent`'s).
        crate::diag::reset();
        // Roll back this turn's user message on failure (mirrors `monocle
        // agent`'s `Repl::run_turn` mark/truncate) so a failed turn doesn't
        // leave a dangling, reply-less user message in the history sent next time.
        let mark = convo.len();
        convo.push(Message::user(text));
        match call_chat(&provider, &model, convo.clone(), max_tokens, &images) {
            Ok(resp) => {
                convo.push(Message::assistant(resp.content));
                let mut out = std::io::stdout();
                out.write_all(b"\n\n")?;
                out.flush()?;
            }
            Err(e) => {
                convo.truncate(mark);
                eprintln!("Error: {e}");
                if crate::diag::was_logged() {
                    eprintln!("  (details logged to {})", crate::diag::display_path());
                }
                eprintln!();
            }
        }
        Ok(LoopControl::Continue)
    })
}

/// `--responses`: jarvice's `/api/responses` (see `responses_api` module
/// docs). The server owns the conversation thread — this REPL only tracks
/// the `thread_id` to pass on the next turn, never a local message history.
///
/// jarvice-only (not reachable through chat-proxy's `router_url`); no custom
/// `--system-prompt`/`--max-tokens` support (the endpoint has no such
/// fields) — both are warned-and-ignored rather than silently dropped.
fn run_responses_chat(client: &Client, creds: &Credentials, options: ChatOptions) -> Result<()> {
    let mut model = options
        .model
        .as_deref()
        .unwrap_or(DEFAULT_MODEL)
        .to_string();

    if options.max_tokens.is_some() {
        eprintln!("⚠ --max-tokens is ignored with --responses (jarvice's Responses API has no such field)");
    }
    if options.system_prompt.is_some() || options.system_prompt_file.is_some() {
        eprintln!("⚠ --system-prompt/--system-prompt-file is ignored with --responses (the endpoint has no generic system-prompt field, only a voice-mode speech_system_context)");
    }

    let stdin_is_tty = std::io::stdin().is_terminal();
    if !options.files.is_empty() && stdin_is_tty {
        eprintln!(
            "--file requires piped input. Pipe your instruction, e.g.:\n  echo \"describe this image\" | monocle chat --responses --file photo.png"
        );
        std::process::exit(1);
    }

    // Auth FIRST, same rationale as the completions path — an expired/missing
    // login must surface before any local-input error masks it.
    let session = get_access_token(client, creds);
    let stored = creds.read().ok_or_else(|| {
        crate::error::AppError::new("Not logged in. Run `monocle login --tenant <domain>` first.")
    })?;
    // Deliberately `jarvice_url_for`, NOT `session.router_url` — `/api/responses`
    // is jarvice-only, unreachable through chat-proxy (see `responses_api` docs).
    let jarvice_url = jarvice_url_for(&stored);

    let one_shot_input = read_one_shot_input(stdin_is_tty, &options.files)?;

    let rc = ResponsesClient::new(client, session.token, jarvice_url.clone());

    if let Some((text, images)) = one_shot_input {
        eprintln!("Using model: {model}");
        eprintln!("jarvice: {jarvice_url}");
        let reply = rc.respond(&model, &text, &images, options.thread.as_deref())?;
        println!("{}", reply.content);
        if let Some(id) = reply.thread_id {
            eprintln!("Thread: {id}");
        }
        return Ok(());
    }

    // Interactive REPL.
    eprintln!("Monocle Chat — Responses API (model: {model})");
    eprintln!("jarvice: {jarvice_url}");
    eprintln!("Type your message. Press Ctrl+D to exit.");
    eprintln!("↑/↓ history, Tab completes /model, /quit, /exit — a multi-line paste is one input.");
    eprintln!("The server owns this conversation's thread — no local history is resent.");
    eprintln!("---");

    let history_path = home_dir().join(".monocle").join("chat_history");
    // Not a `Vec<Message>` like `run_completions_chat`'s `convo` — the server
    // persists the conversation; this is just the id to hand back next turn.
    let mut thread_id: Option<String> = options.thread.clone();
    // Best-effort, same defensive posture as `agent.rs`'s REPL: this path has
    // no startup model-list fetch to piggyback on (unlike the completions
    // path's validation call), so fetch once here for `/model` completion.
    let model_ids = fetch_model_ids(client, creds).unwrap_or_default();
    let helper = ModelReplHelper {
        commands: CHAT_SLASH_COMMANDS,
        models: model_ids,
    };

    run_repl(helper, history_path, "> ", |trimmed| {
        if trimmed == "/quit" || trimmed == "/exit" {
            eprintln!("Bye.");
            return Ok(LoopControl::Quit);
        }
        // `/model` switches the model for subsequent turns — handled before
        // the general dispatch since it carries an argument, same as
        // `monocle agent`'s REPL.
        if handle_model_command(trimmed, &mut model) {
            return Ok(LoopControl::Continue);
        }

        let (text, images) = match resolve_repl_attachments(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!();
                return Ok(LoopControl::Continue);
            }
        };

        eprintln!();
        crate::diag::reset();
        match rc.respond(&model, &text, &images, thread_id.as_deref()) {
            Ok(reply) => {
                println!("{}", reply.content);
                // Announce the thread id only when it's newly learned (the
                // first turn) or changed — not on every turn, since it's
                // normally stable for the rest of the session.
                if reply.thread_id.is_some() && thread_id != reply.thread_id {
                    thread_id = reply.thread_id;
                    if let Some(id) = &thread_id {
                        eprintln!("Thread: {id}");
                    }
                }
                println!();
            }
            Err(e) => {
                eprintln!("Error: {e}");
                if crate::diag::was_logged() {
                    eprintln!("  (details logged to {})", crate::diag::display_path());
                }
                eprintln!();
            }
        }
        Ok(LoopControl::Continue)
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve_max_tokens, resolve_repl_attachments};

    // The shared `ModelReplHelper` (Completer/Hinter, slash-command +
    // `/model` fuzzy completion) now lives in `commands::repl` and is
    // covered there, since that's where its Completer/Hinter impls live.

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
