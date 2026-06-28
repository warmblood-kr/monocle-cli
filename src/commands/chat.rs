use std::io::{IsTerminal, Write};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::origin_header;

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_MAX_TOKENS: i64 = 4096;

pub struct ChatOptions {
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<String>,
    pub max_tokens: Option<String>,
}

#[derive(Deserialize)]
struct ChatChoiceMsg {
    content: Option<String>,
}
#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatChoiceMsg>,
}
#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

fn call_chat(
    client: &Client,
    router_url: &str,
    token: &str,
    model: &str,
    system_prompt: Option<&str>,
    user_message: &str,
    max_tokens: i64,
) -> Result<String> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sp) = system_prompt {
        messages.push(json!({ "role": "system", "content": sp }));
    }
    messages.push(json!({ "role": "user", "content": user_message }));

    let bearer = format!("Bearer {token}");
    let resp = client.post_json(
        &format!("{router_url}{}", endpoints::CHAT_COMPLETIONS),
        &[("Authorization", &bearer), origin_header()],
        &json!({
            "model": model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": false,
        }),
    )?;

    if !resp.ok() {
        return Err(AppError::new(format!(
            "API error {}: {}",
            resp.status,
            resp.text()
        )));
    }

    let data: ChatResponse = resp.json()?;
    Ok(data
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .unwrap_or_default())
}

pub fn chat_command(client: &Client, creds: &Credentials, options: ChatOptions) -> Result<()> {
    let model = options
        .model
        .as_deref()
        .unwrap_or(DEFAULT_MODEL)
        .to_string();
    let max_tokens = options
        .max_tokens
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT_MAX_TOKENS);

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
        &[("Authorization", &bearer), origin_header()],
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

    // Non-interactive: stdin is piped.
    if !std::io::stdin().is_terminal() {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
        if input.trim().is_empty() {
            eprintln!("No input provided via stdin.");
            std::process::exit(1);
        }
        eprintln!("Using model: {model}");
        eprintln!("Router: {router_url}");
        let result = call_chat(
            client,
            &router_url,
            &token,
            &model,
            system_prompt.as_deref(),
            input.trim(),
            max_tokens,
        )?;
        let mut out = std::io::stdout();
        out.write_all(result.as_bytes())?;
        out.write_all(b"\n")?;
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
            client,
            &router_url,
            &token,
            &model,
            system_prompt.as_deref(),
            trimmed,
            max_tokens,
        ) {
            Ok(result) => {
                let mut out = std::io::stdout();
                out.write_all(result.as_bytes())?;
                out.write_all(b"\n\n")?;
                out.flush()?;
            }
            Err(e) => eprintln!("Error: {e}\n"),
        }
    }

    Ok(())
}
