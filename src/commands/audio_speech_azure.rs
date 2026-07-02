use crate::audio_io::{
    ensure_not_writing_binary_to_tty, read_stdin_text, write_api_error_and_exit,
    write_binary_output,
};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::origin_header;

const DEFAULT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

#[derive(Default)]
pub struct AudioSpeechAzureOptions {
    pub format: Option<String>,
    pub output: Option<String>,
}

pub fn audio_speech_azure_command(
    client: &Client,
    creds: &Credentials,
    body_arg: Option<&str>,
    options: AudioSpeechAzureOptions,
) -> Result<()> {
    let mut body = body_arg.unwrap_or("").to_string();
    if body.is_empty() || body == "-" {
        body = read_stdin_text()?;
    }
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err(AppError::new(
            "No input. Pass an SSML document as an argument or pipe it via stdin.",
        ));
    }

    if !body.starts_with("<speak") {
        return Err(AppError::new(
            "Azure TTS requires SSML. Body must start with `<speak …>`. \
             Tip: keep SSML in a file and pipe it in to avoid shell-escaping issues:\n\
             \x20 monocle audio speech-azure -o out.mp3 < my.ssml",
        ));
    }

    ensure_not_writing_binary_to_tty(options.output.as_deref())?;

    let session = get_access_token(client, creds);

    let bearer = format!("Bearer {}", session.token);
    let format = options.format.as_deref().unwrap_or(DEFAULT_FORMAT);
    let resp = client.post_bytes(
        &format!("{}{}", session.router_url, endpoints::AZURE_TEXT_TO_SPEECH),
        &[
            ("Authorization", &bearer),
            ("X-Microsoft-OutputFormat", format),
            origin_header(),
        ],
        "application/ssml+xml",
        body.into_bytes(),
    )?;

    if !resp.ok() {
        write_api_error_and_exit(&resp);
    }

    write_binary_output(resp.bytes(), options.output.as_deref())
}
