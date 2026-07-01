use serde_json::json;

use crate::audio_io::{
    ensure_not_writing_binary_to_tty, read_stdin_text, write_api_error_and_exit,
    write_binary_output,
};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

const DEFAULT_MODEL: &str = "gpt-4o-mini-tts";
const DEFAULT_VOICE: &str = "alloy";
const DEFAULT_FORMAT: &str = "mp3";

#[derive(Default)]
pub struct AudioSpeechOptions {
    pub model: Option<String>,
    pub voice: Option<String>,
    pub format: Option<String>,
    pub speed: Option<String>,
    pub instructions: Option<String>,
    pub output: Option<String>,
}

pub fn audio_speech_command(
    client: &Client,
    creds: &Credentials,
    text_arg: Option<&str>,
    options: AudioSpeechOptions,
) -> Result<()> {
    let mut text = text_arg.unwrap_or("").to_string();
    if text.is_empty() || text == "-" {
        text = read_stdin_text()?.trim().to_string();
    }
    if text.is_empty() {
        return Err(AppError::new(
            "No input text. Pass text as an argument or pipe it via stdin.",
        ));
    }

    ensure_not_writing_binary_to_tty(options.output.as_deref())?;

    let session = get_access_token(client, creds);

    let mut payload = json!({
        "model": options.model.as_deref().unwrap_or(DEFAULT_MODEL),
        "voice": options.voice.as_deref().unwrap_or(DEFAULT_VOICE),
        "input": text,
        "response_format": options.format.as_deref().unwrap_or(DEFAULT_FORMAT),
    });
    if let Some(speed) = &options.speed {
        if let Ok(n) = speed.trim().parse::<f64>() {
            payload["speed"] = json!(n);
        }
    }
    if let Some(instr) = &options.instructions {
        payload["instructions"] = json!(instr);
    }

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_json(
        &format!("{}{}", session.router_url, endpoints::AUDIO_SPEECH),
        &auth_headers(&bearer),
        &payload,
    )?;

    if !resp.ok() {
        write_api_error_and_exit(&resp);
    }

    write_binary_output(resp.bytes(), options.output.as_deref())
}
