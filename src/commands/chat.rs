use std::io::{IsTerminal, Write};
use std::time::Duration;

use serde_json::Value;

use chrono::TimeZone;

use crate::agent::providers::{
    ChatRequest, ChatResponse, ImageAttachment, LlmProvider, Message, MonocleProvider, ToolCall,
};
use crate::agent::DEFAULT_MODEL;
use crate::attachment;
use crate::auth::{get_access_token, jarvice_url_for, try_access_token, AuthSession};
use crate::commands::model_list::{fetch_model_ids, handle_model_command};
use crate::commands::repl::{
    handle_diag_command, mode_banner, run_repl, LoopControl, ModelReplHelper, TurnDiagnostics,
    PROMPT,
};
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
    /// `--resume <ID>`: continue an existing `--responses` thread.
    pub resume: Option<String>,
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
///
/// Also returns the time-to-first-byte (`/diag`'s "Time to first byte" line):
/// the elapsed time from just before the network call to the FIRST streamed
/// delta, captured once via the `on_delta` closure this fn already hands to
/// `chat_stream` — no new streaming plumbing needed. `None` only if the stream
/// produced zero deltas (e.g. an empty response).
fn call_chat(
    provider: &MonocleProvider,
    model: &str,
    messages: Vec<Message>,
    max_tokens: Option<i64>,
    images: &[ImageAttachment],
) -> Result<(ChatResponse, Option<Duration>)> {
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
    let started = std::time::Instant::now();
    let mut ttfb: Option<Duration> = None;
    let resp = provider.chat_stream(&req, &mut |delta| {
        if ttfb.is_none() {
            ttfb = Some(started.elapsed());
        }
        let _ = out.write_all(delta.as_bytes());
        let _ = out.flush();
    })?;
    // A mid-stream drop was salvaged into partial output (monocle-cli#59): stdout
    // already holds the partial text, so the notice goes to stderr.
    if resp.truncated {
        eprintln!("\n⚠ the response was cut short (partial output shown).");
    }
    Ok((resp, ttfb))
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

/// Resolve a fresh (auto-refreshing) session for this turn, or print the
/// error and return `None` so the caller can bail out of the turn with
/// `LoopControl::Continue` — a dead refresh token is a per-turn error here,
/// never fatal to the whole REPL session.
fn resolve_turn_session(client: &Client, creds: &Credentials) -> Option<AuthSession> {
    match try_access_token(client, creds) {
        Ok(session) => Some(session),
        Err(e) => {
            eprintln!("Error: {e}");
            eprintln!();
            None
        }
    }
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
/// agent`'s REPL (switches the model for subsequent turns); `/diag` shows
/// diagnostics for the last turn; `/help` (re-)prints the onboarding block
/// shown at REPL startup — chat still has no `/config`/`/status` (those are
/// agent-specific session state with no chat equivalent).
const CHAT_SLASH_COMMANDS: &[&str] = &["/help", "/model", "/diag", "/exit", "/quit"];

/// The onboarding block printed once at REPL startup (both `run_completions_chat`
/// and `run_responses_chat`) and re-printed on demand by `/help` — one string,
/// two call sites, so the two never drift. The Tab-completion line is built
/// from `CHAT_SLASH_COMMANDS` itself (not hand-typed) so it can't silently
/// under-report a command again the way it previously did. The `/diag` line
/// deliberately doesn't itemize which fields it shows — `run_responses_chat`
/// never learns a served model or token usage (see `TurnDiagnostics` docs),
/// so a shared, itemized promise would overclaim for that path; `format_diag`
/// already self-describes by omitting whatever the backend didn't report.
fn chat_help_text() -> String {
    [
        "Type your message. Press Ctrl+D to exit.".to_string(),
        format!(
            "↑/↓ history, Tab completes {} — a multi-line paste is one input.",
            CHAT_SLASH_COMMANDS.join(", ")
        ),
        "/diag shows diagnostics for the last turn (only what the backend reports is shown)."
            .to_string(),
        "/help shows this message again.".to_string(),
    ]
    .join("\n")
}

/// Handle a `/help` line if `trimmed` is one: (re-)prints the onboarding
/// block. Returns `true` if this input was consumed as `/help`, mirroring
/// `handle_diag_command`'s contract.
fn handle_help_command(trimmed: &str) -> bool {
    if trimmed != "/help" {
        return false;
    }
    eprintln!("{}", chat_help_text());
    true
}

/// Dispatch a slash-command line to whichever handler recognizes it (shared
/// by both REPL loops, so the two don't drift out of sync). Returns
/// `Some(control)` when `trimmed` was consumed as a recognized command — the
/// caller should return it immediately. `None` means it wasn't a recognized
/// command and should be treated as ordinary chat input.
fn dispatch_chat_command(
    trimmed: &str,
    model: &mut String,
    diagnostics: &Option<TurnDiagnostics>,
) -> Option<LoopControl> {
    if trimmed == "/quit" || trimmed == "/exit" {
        eprintln!("Bye.");
        return Some(LoopControl::Quit);
    }
    // `/model` switches the model for subsequent turns — handled before
    // `/diag` since it carries an argument, same as `monocle agent`'s REPL.
    if handle_model_command(trimmed, model) {
        return Some(LoopControl::Continue);
    }
    if handle_diag_command(trimmed, diagnostics) {
        return Some(LoopControl::Continue);
    }
    if handle_help_command(trimmed) {
        return Some(LoopControl::Continue);
    }
    None
}

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

    // Non-interactive: stdin was piped (input + attachments already resolved above).
    if let Some((text, images)) = one_shot_input {
        eprintln!("Using model: {model}");
        eprintln!("Router: {router_url}");
        let provider = MonocleProvider::new(token, router_url.clone());
        let mut messages = Vec::new();
        if let Some(sp) = &system_prompt {
            messages.push(Message::system(sp));
        }
        messages.push(Message::user(&text));
        // One-shot mode has no `/diag` to show — TTFB is discarded here.
        let (resp, _ttfb) = call_chat(&provider, &model, messages, max_tokens, &images)?;
        if let Some(msg) = dropped_tool_calls_message(&resp.tool_calls, 0, "monocle chat") {
            eprintln!("{msg}");
        }
        let mut out = std::io::stdout();
        out.write_all(b"\n")?;
        out.flush()?;
        return Ok(());
    }

    // Built once for the REPL's lifetime (mutable) and refreshed in place
    // each turn via `MonocleProvider::refresh` — reuses the underlying HTTP
    // client/connection pool instead of rebuilding one every turn, unlike a
    // fresh `MonocleProvider::from_session` call which pays `Client::new()`'s
    // real setup cost each time.
    let mut provider = MonocleProvider::new(token, router_url.clone());

    // Interactive REPL.
    eprintln!("Monocle Chat (model: {model})");
    eprintln!("Router: {router_url}");
    if let Some(sp) = &system_prompt {
        eprintln!("System prompt loaded ({} chars)", sp.chars().count());
    }
    eprintln!("{}", chat_help_text());
    eprintln!("---");
    eprintln!("{}", mode_banner("chat", crate::colors::cyan));

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
    // Last turn's diagnostics, shown on demand by `/diag` — `None` until the
    // first successful turn.
    let mut diagnostics: Option<TurnDiagnostics> = None;

    run_repl(helper, history_path, PROMPT, |trimmed| {
        if let Some(control) = dispatch_chat_command(trimmed, &mut model, &diagnostics) {
            return Ok(control);
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

        // Fresh token each turn (`try_access_token` refreshes if near expiry),
        // refreshed in place on the shared `provider` (see
        // `MonocleProvider::refresh`) rather than rebuilding it — mirrors
        // `monocle agent`'s `Repl::run_turn` so a long-lived interactive
        // session never sends a stale bearer. Resolved before touching
        // `convo` (same order as `agent.rs`), so a refresh failure (e.g. a
        // dead refresh token) never leaves a dangling user message — it's a
        // per-turn error, not fatal, same recovery shape as a failed
        // `call_chat` below. Placed here (before `eprintln!();
        // crate::diag::reset();` below, not after) so this insertion point
        // doesn't land on the same anchor as the sibling `/diag` PR's
        // `Instant::now()` insertion right after `crate::diag::reset()`.
        let session = match resolve_turn_session(client, creds) {
            Some(s) => s,
            None => return Ok(LoopControl::Continue),
        };
        // Captured before `provider.refresh` consumes `session` — the actual
        // per-turn request goes to whatever router URL THIS session carries,
        // which can differ from the outer `router_url` after a mid-session
        // refresh re-discovers a different router. `/diag`'s endpoint must
        // reflect the request that was actually sent, not the startup value.
        let turn_router_url = session.router_url.clone();
        provider.refresh(session);

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
        // Wrapped tightly around the network call itself (not REPL input-wait
        // time) — this is what `/diag`'s `Latency (total):` line reports.
        // `call_chat`'s own `Duration` return is `/diag`'s `Time to first byte:`
        // line — captured inside `call_chat` at its first streamed delta.
        let started = std::time::Instant::now();
        match call_chat(&provider, &model, convo.clone(), max_tokens, &images) {
            Ok((resp, ttfb)) => {
                diagnostics = Some(TurnDiagnostics::for_chat(
                    model.clone(),
                    resp.model.clone(),
                    format!("{turn_router_url}{}", endpoints::CHAT_COMPLETIONS),
                    ttfb.map(|d| d.as_millis()),
                    started.elapsed().as_millis(),
                    resp.usage.clone(),
                ));
                if let Some(msg) = dropped_tool_calls_message(&resp.tool_calls, 0, "monocle chat") {
                    eprintln!("{msg}");
                }
                convo.push(Message::assistant(resp.content));
                let mut out = std::io::stdout();
                out.write_all(b"\n\n")?;
                out.flush()?;
            }
            Err(e) => {
                convo.truncate(mark);
                // A failed turn must not leave stale diagnostics from an
                // earlier successful turn silently displayable via `/diag`.
                diagnostics = None;
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

/// Resolve a validated session + jarvice's own base URL + a ready-to-use
/// `Authorization` header value — shared by `run_responses_chat` and
/// `chat_list_command`, since both need the same auth + jarvice-host
/// resolution. Deliberately `jarvice_url_for`, NOT `session.router_url`
/// (chat-proxy) — see `responses_api` module docs for why.
fn resolve_jarvice(client: &Client, creds: &Credentials) -> Result<(AuthSession, String, String)> {
    let session = get_access_token(client, creds);
    let stored = creds.read().ok_or_else(|| {
        crate::error::AppError::new("Not logged in. Run `monocle login --tenant <domain>` first.")
    })?;
    let jarvice_url = jarvice_url_for(&stored);
    let bearer = format!("Bearer {}", session.token);
    Ok((session, jarvice_url, bearer))
}

/// `monocle chat list` — list the current user's existing jarvice threads
/// (id/title/last-updated) and exit. Always jarvice's thread storage — there
/// is nothing else to list in the plain completions mode, so this never takes
/// a `--responses` flag.
pub fn chat_list_command(client: &Client, creds: &Credentials) -> Result<()> {
    let (_session, jarvice_url, bearer) = resolve_jarvice(client, creds)?;
    let threads = crate::jarvice_chats::list_threads(client, &jarvice_url, &bearer)?;
    print_threads_table(&threads);
    Ok(())
}

/// Build a warning about tool call(s) a turn couldn't act on, or `None` if
/// there's nothing to report (monocle-cli#101 / monocle#275 — see
/// `responses_api::ResponsesReply::tool_calls` for the canonical explanation
/// of why `--responses` mode can't execute them). Pure — no I/O — so it's
/// directly unit-testable; the call site does the actual `eprintln!`, same as
/// this path's other warnings degrading gracefully rather than erroring the
/// turn. `unparsed` additionally reports tool_calls whose shape didn't even
/// deserialize (see `responses_api::parse_tool_calls`) — those must be
/// mentioned too, or a shape mismatch is just as silent as never reading the
/// field at all. Shared by both `run_responses_chat` and `run_completions_chat`
/// so plain `monocle chat` gets the same non-silent behavior, not just
/// `--responses` mode (monocle-cli#101 code review).
fn dropped_tool_calls_message(
    tool_calls: &[ToolCall],
    unparsed: usize,
    mode: &str,
) -> Option<String> {
    if tool_calls.is_empty() && unparsed == 0 {
        return None;
    }
    let mut reasons = Vec::new();
    if !tool_calls.is_empty() {
        let names = tool_calls
            .iter()
            .map(|tc| tc.function.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        reasons.push(format!("tool call(s) ({names})"));
    }
    if unparsed > 0 {
        reasons.push(format!("{unparsed} tool call(s) in an unrecognized shape"));
    }
    Some(format!(
        "⚠ model requested {} but {mode} does not execute tools yet — see https://github.com/warmblood-kr/monocle-cli/issues/101 (try `monocle agent` for local tool execution today)",
        reasons.join(" and ")
    ))
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
    let (session, jarvice_url, bearer) = resolve_jarvice(client, creds)?;

    let one_shot_input = read_one_shot_input(stdin_is_tty, &options.files)?;

    if let Some((text, images)) = one_shot_input {
        eprintln!("Using model: {model}");
        eprintln!("jarvice: {jarvice_url}");
        let rc = ResponsesClient::new(client, session.token, jarvice_url.clone());
        let reply = rc.respond(&model, &text, &images, options.resume.as_deref())?;
        if let Some(msg) = dropped_tool_calls_message(
            &reply.tool_calls,
            reply.unparsed_tool_calls,
            "--responses mode",
        ) {
            eprintln!("{msg}");
        }
        println!("{}", reply.content);
        if let Some(id) = reply.thread_id {
            eprintln!("Thread: {id}");
        }
        return Ok(());
    }

    // Interactive REPL.
    eprintln!("Monocle Chat — Responses API (model: {model})");
    eprintln!("jarvice: {jarvice_url}");
    eprintln!("{}", chat_help_text());
    eprintln!("The server owns this conversation's thread — no local history is resent.");
    eprintln!("---");

    let history_path = home_dir().join(".monocle").join("chat_history");
    // Not a `Vec<Message>` like `run_completions_chat`'s `convo` — the server
    // persists the conversation; this is just the id to hand back next turn.
    let mut thread_id: Option<String> = options.resume.clone();
    // Best-effort, same defensive posture as `agent.rs`'s REPL: this path has
    // no startup model-list fetch to piggyback on (unlike the completions
    // path's validation call), so fetch once here for `/model` completion.
    let model_ids = fetch_model_ids(client, creds).unwrap_or_default();
    let helper = ModelReplHelper {
        commands: CHAT_SLASH_COMMANDS,
        models: model_ids,
    };
    // Last turn's diagnostics, shown on demand by `/diag`. This path never
    // learns a served model or token usage (see `TurnDiagnostics` docs), so
    // those fields stay `None` for the life of the session.
    let mut diagnostics: Option<TurnDiagnostics> = None;

    // Replay prior history for an existing thread, REPL-only (never in the
    // one-shot path above, never on stdout — this repo treats stdout as
    // strictly the answer/data channel). A fetch failure must not block
    // continuing the thread, so it's a warning, not a propagated error.
    if let Some(id) = &thread_id {
        match crate::jarvice_chats::get_thread(client, &jarvice_url, &bearer, id) {
            Ok(detail) => {
                let turns = detail
                    .current_id
                    .as_deref()
                    .map(|cur| crate::jarvice_chats::linearize(&detail.messages, cur))
                    .unwrap_or_default();
                if !turns.is_empty() {
                    eprintln!("--- prior history ---");
                    for node in turns {
                        match node.role.as_str() {
                            "user" => eprintln!("> {}", node.content),
                            "assistant" => eprintln!("{}", node.content),
                            _ => {}
                        }
                    }
                    eprintln!("--- end history ---");
                }
            }
            Err(e) => {
                eprintln!("Warning: could not load thread history: {e}");
            }
        }
    }

    eprintln!("{}", mode_banner("chat", crate::colors::cyan));

    run_repl(helper, history_path, PROMPT, |trimmed| {
        if let Some(control) = dispatch_chat_command(trimmed, &mut model, &diagnostics) {
            return Ok(control);
        }

        let (text, images) = match resolve_repl_attachments(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!();
                return Ok(LoopControl::Continue);
            }
        };

        // Fresh token each turn, same rationale and ordering as the
        // completions path / `monocle agent`'s `Repl::run_turn` — rebuilds
        // the client so a long-lived REPL never sends a stale bearer. A
        // refresh failure is a per-turn error, not fatal. Placed here
        // (before `eprintln!(); crate::diag::reset();` below, not after) so
        // this insertion point doesn't land on the same anchor as the
        // sibling `/diag` PR's `Instant::now()` insertion right after
        // `crate::diag::reset()`.
        let session = match resolve_turn_session(client, creds) {
            Some(s) => s,
            None => return Ok(LoopControl::Continue),
        };
        let rc = ResponsesClient::new(client, session.token, jarvice_url.clone());

        eprintln!();
        crate::diag::reset();

        // Wrapped tightly around the network call itself, same as the
        // completions path — this is what `/diag`'s `Latency (total):` line
        // reports. This path makes one blocking call (no streamed deltas), so
        // there's no `Time to first byte:` to capture — see the `for_chat`
        // call below.
        let started = std::time::Instant::now();
        match rc.respond(&model, &text, &images, thread_id.as_deref()) {
            Ok(reply) => {
                diagnostics = Some(TurnDiagnostics::for_chat(
                    model.clone(),
                    None,
                    format!("{jarvice_url}/api/responses"),
                    // `--responses` makes one blocking call with no
                    // incremental deltas — structurally no TTFB to report.
                    None,
                    started.elapsed().as_millis(),
                    None,
                ));
                if let Some(msg) = dropped_tool_calls_message(
                    &reply.tool_calls,
                    reply.unparsed_tool_calls,
                    "--responses mode",
                ) {
                    eprintln!("{msg}");
                }
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
                // A failed turn must not leave stale diagnostics from an
                // earlier successful turn silently displayable via `/diag`.
                diagnostics = None;
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

/// Render `monocle chat list`'s table to stdout — this is a terminal
/// print-and-exit command (like `monocle models`), so stdout is the correct
/// channel here, unlike the history-replay path below.
fn print_threads_table(threads: &[crate::jarvice_chats::ChatSummary]) {
    use crate::commands::model_list::pad;

    if threads.is_empty() {
        eprintln!("No threads found.");
        return;
    }

    let id_width = threads
        .iter()
        .map(|t| t.id.chars().count())
        .chain([9])
        .max()
        .unwrap();
    let title_width = threads
        .iter()
        .map(|t| t.title.chars().count())
        .chain([5])
        .max()
        .unwrap();

    let mut out = std::io::stdout();
    let _ = writeln!(
        out,
        "{}  {}  UPDATED",
        pad("THREAD ID", id_width),
        pad("TITLE", title_width)
    );
    let _ = writeln!(
        out,
        "{}  {}  {}",
        "─".repeat(id_width),
        "─".repeat(title_width),
        "─".repeat(16)
    );
    for t in threads {
        let updated = chrono::Utc
            .timestamp_opt(t.updated_at, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
        let _ = writeln!(
            out,
            "{}  {}  {}",
            pad(&t.id, id_width),
            pad(&t.title, title_width),
            updated
        );
    }
    eprintln!("\n{} thread(s).", threads.len());
}

#[cfg(test)]
mod tests {
    use super::{
        chat_help_text, dispatch_chat_command, dropped_tool_calls_message, handle_help_command,
        resolve_max_tokens, resolve_repl_attachments, TurnDiagnostics,
    };
    use crate::agent::providers::{FunctionCall, ToolCall};
    use crate::commands::repl::LoopControl;

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn dropped_tool_calls_message_none_when_nothing_to_report() {
        assert_eq!(dropped_tool_calls_message(&[], 0, "--responses mode"), None);
    }

    #[test]
    fn dropped_tool_calls_message_names_the_call_and_the_mode() {
        // monocle-cli#101 code review: the pairing between obtaining a reply
        // and warning about it was previously enforced by convention only,
        // with no test ever exercising the warning text itself. This locks
        // it down directly, against the pure (non-eprintln!) builder.
        let msg = dropped_tool_calls_message(&[tool_call("generate_image")], 0, "--responses mode")
            .expect("should produce a message");
        assert!(msg.contains("generate_image"), "{msg}");
        assert!(msg.contains("--responses mode"), "{msg}");
        assert!(msg.contains("monocle agent"), "{msg}");
    }

    #[test]
    fn dropped_tool_calls_message_reports_unparsed_count_even_with_no_named_calls() {
        let msg = dropped_tool_calls_message(&[], 3, "--responses mode").expect("should report");
        assert!(msg.contains('3'), "{msg}");
        assert!(msg.contains("unrecognized shape"), "{msg}");
    }

    #[test]
    fn dropped_tool_calls_message_reports_both_named_and_unparsed() {
        let msg = dropped_tool_calls_message(&[tool_call("read_file")], 1, "monocle chat")
            .expect("should report");
        assert!(msg.contains("read_file"), "{msg}");
        assert!(msg.contains('1'), "{msg}");
        assert!(msg.contains("monocle chat"), "{msg}");
    }

    // The shared `ModelReplHelper` (Completer/Hinter, slash-command +
    // `/model` fuzzy completion) now lives in `commands::repl` and is
    // covered there, since that's where its Completer/Hinter impls live.

    // `/model`/`/diag` dispatch through `dispatch_chat_command` isn't
    // re-tested in detail here — `handle_model_command` and
    // `handle_diag_command` already have their own coverage; this just
    // confirms the shared entry point recognizes `/quit`/`/exit` and passes
    // through anything else as ordinary chat input.
    #[test]
    fn dispatch_chat_command_quit_and_exit_stop_the_repl() {
        let mut model = "monocle-auto".to_string();
        let diagnostics: Option<TurnDiagnostics> = None;
        assert!(matches!(
            dispatch_chat_command("/quit", &mut model, &diagnostics),
            Some(LoopControl::Quit)
        ));
        assert!(matches!(
            dispatch_chat_command("/exit", &mut model, &diagnostics),
            Some(LoopControl::Quit)
        ));
    }

    #[test]
    fn dispatch_chat_command_unrecognized_line_is_chat_input() {
        let mut model = "monocle-auto".to_string();
        let diagnostics: Option<TurnDiagnostics> = None;
        assert!(dispatch_chat_command("hello there", &mut model, &diagnostics).is_none());
    }

    #[test]
    fn dispatch_chat_command_recognizes_help() {
        let mut model = "monocle-auto".to_string();
        let diagnostics: Option<TurnDiagnostics> = None;
        assert!(matches!(
            dispatch_chat_command("/help", &mut model, &diagnostics),
            Some(LoopControl::Continue)
        ));
    }

    #[test]
    fn handle_help_command_only_consumes_exact_slash_help() {
        assert!(!handle_help_command("not help"));
        assert!(handle_help_command("/help"));
    }

    #[test]
    fn chat_help_text_mentions_the_documented_commands() {
        let text = chat_help_text();
        for cmd in ["/model", "/diag", "/help", "/quit", "/exit"] {
            assert!(text.contains(cmd), "help text missing {cmd}: {text}");
        }
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
