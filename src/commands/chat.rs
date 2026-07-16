use std::io::{IsTerminal, Write};

use serde_json::Value;

use chrono::TimeZone;

use crate::agent::providers::{
    ChatRequest, ChatResponse, ImageAttachment, LlmProvider, Message, MonocleProvider, TokenUsage,
};
use crate::agent::DEFAULT_MODEL;
use crate::attachment;
use crate::auth::{get_access_token, jarvice_url_for, try_access_token, AuthSession};
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
/// diagnostics for the last turn; chat still has no `/help`/`/config`/`/status`.
const CHAT_SLASH_COMMANDS: &[&str] = &["/model", "/diag", "/exit", "/quit"];

/// Diagnostics for the most recently completed turn, shown on demand by
/// `/diag` — REPL-only, overwritten each turn (no history is kept). Built
/// fresh after every successful turn in both `run_completions_chat` and
/// `run_responses_chat`; `served_model`/`usage` stay `None` wherever the
/// backend doesn't report them (the `--responses` path reports neither today
/// — see module docs on `responses_api::ResponsesReply`).
struct TurnDiagnostics {
    requested_model: String,
    served_model: Option<String>,
    endpoint: String,
    latency_ms: u128,
    usage: Option<TokenUsage>,
}

/// Render `/diag`'s bordered block (same `--- ... ---` framing as the
/// `--responses` REPL's prior-history replay). Lines for data the backend
/// didn't report are omitted entirely — never printed as `None`/`null`.
fn format_diag(d: &TurnDiagnostics) -> String {
    let mut lines = vec![
        "--- diag ---".to_string(),
        format!("Endpoint: {}", d.endpoint),
        format!("Requested model: {}", d.requested_model),
    ];
    if let Some(served) = &d.served_model {
        lines.push(format!("Served model: {served}"));
    }
    lines.push(format!("Latency: {}ms", d.latency_ms));
    if let Some(u) = &d.usage {
        lines.push(format!(
            "Tokens: {} prompt + {} completion = {} total",
            u.prompt_tokens, u.completion_tokens, u.total_tokens
        ));
    }
    lines.push("--- end diag ---".to_string());
    lines.join("\n")
}

/// Handle a `/diag` line if `trimmed` is one: prints the last turn's
/// diagnostics (or a "nothing yet" hint before the first turn). Returns
/// `true` if this input was consumed as `/diag`, mirroring
/// `model_list::handle_model_command`'s contract.
fn handle_diag_command(trimmed: &str, diagnostics: &Option<TurnDiagnostics>) -> bool {
    if trimmed != "/diag" {
        return false;
    }
    match diagnostics {
        None => eprintln!(
            "{}",
            crate::colors::dim("No response yet — send a message first.")
        ),
        Some(d) => eprintln!("{}", format_diag(d)),
    }
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
        call_chat(&provider, &model, messages, max_tokens, &images)?;
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
    eprintln!("Type your message. Press Ctrl+D to exit.");
    eprintln!("↑/↓ history, Tab completes /model, /quit, /exit — a multi-line paste is one input.");
    eprintln!(
        "/diag shows diagnostics (served model, endpoint, latency, tokens) for the last turn."
    );
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
    // Last turn's diagnostics, shown on demand by `/diag` — `None` until the
    // first successful turn.
    let mut diagnostics: Option<TurnDiagnostics> = None;

    run_repl(helper, history_path, "> ", |trimmed| {
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
        // time) — this is what `/diag`'s `Latency:` line reports.
        let started = std::time::Instant::now();
        match call_chat(&provider, &model, convo.clone(), max_tokens, &images) {
            Ok(resp) => {
                diagnostics = Some(TurnDiagnostics {
                    requested_model: model.clone(),
                    served_model: resp.model.clone(),
                    endpoint: format!("{turn_router_url}{}", endpoints::CHAT_COMPLETIONS),
                    latency_ms: started.elapsed().as_millis(),
                    usage: resp.usage.clone(),
                });
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
    eprintln!("/diag shows diagnostics (endpoint, latency) for the last turn.");
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

    run_repl(helper, history_path, "> ", |trimmed| {
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
        // completions path — this is what `/diag`'s `Latency:` line reports.
        let started = std::time::Instant::now();
        match rc.respond(&model, &text, &images, thread_id.as_deref()) {
            Ok(reply) => {
                diagnostics = Some(TurnDiagnostics {
                    requested_model: model.clone(),
                    served_model: None,
                    endpoint: format!("{jarvice_url}/api/responses"),
                    latency_ms: started.elapsed().as_millis(),
                    usage: None,
                });
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
        dispatch_chat_command, format_diag, resolve_max_tokens, resolve_repl_attachments,
        TurnDiagnostics,
    };
    use crate::agent::providers::TokenUsage;
    use crate::commands::repl::LoopControl;

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
    fn format_diag_omits_served_model_and_usage_when_unavailable() {
        // The `--responses` path: only endpoint/requested-model/latency are
        // ever known — served model and usage must never render as a literal
        // "None"/"null", they're just absent lines.
        let d = TurnDiagnostics {
            requested_model: "monocle-auto".to_string(),
            served_model: None,
            endpoint: "https://acme.monocle-ai.com/api/responses".to_string(),
            latency_ms: 842,
            usage: None,
        };
        let block = format_diag(&d);
        assert_eq!(
            block,
            "--- diag ---\n\
             Endpoint: https://acme.monocle-ai.com/api/responses\n\
             Requested model: monocle-auto\n\
             Latency: 842ms\n\
             --- end diag ---"
        );
        assert!(!block.contains("None"));
        assert!(!block.contains("null"));
    }

    #[test]
    fn format_diag_shows_served_model_and_usage_when_present() {
        let d = TurnDiagnostics {
            requested_model: "monocle-auto".to_string(),
            served_model: Some("claude-sonnet-4-6".to_string()),
            endpoint: "https://api.monocle-ai.com/v1/chat/completions".to_string(),
            latency_ms: 1234,
            usage: Some(TokenUsage {
                prompt_tokens: 120,
                completion_tokens: 45,
                total_tokens: 165,
            }),
        };
        let block = format_diag(&d);
        assert_eq!(
            block,
            "--- diag ---\n\
             Endpoint: https://api.monocle-ai.com/v1/chat/completions\n\
             Requested model: monocle-auto\n\
             Served model: claude-sonnet-4-6\n\
             Latency: 1234ms\n\
             Tokens: 120 prompt + 45 completion = 165 total\n\
             --- end diag ---"
        );
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
