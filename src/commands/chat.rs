use std::io::{IsTerminal, Write};

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

    // Read stdin + resolve any attachments up front — cheap, local-only work
    // that should fail fast (bad path, unsupported MIME) before we ever touch
    // the network for auth/model validation below.
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

    let session = get_access_token(client, creds);
    let token = session.token;
    let router_url = session.router_url;
    let bearer = format!("Bearer {token}");

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
    eprintln!("---");

    let stdin = std::io::stdin();
    loop {
        eprint!("> ");
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            // EOF (Ctrl+D)
            eprintln!("\nBye.");
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/quit" || trimmed == "/exit" {
            eprintln!("\nBye.");
            break;
        }

        eprintln!();
        match call_chat(
            &provider,
            &model,
            system_prompt.as_deref(),
            trimmed,
            max_tokens,
            &[],
        ) {
            Ok(()) => {
                let mut out = std::io::stdout();
                out.write_all(b"\n\n")?;
                out.flush()?;
            }
            Err(e) => eprintln!("Error: {e}\n"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_max_tokens;

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
}
