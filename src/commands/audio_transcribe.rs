use std::io::Write;

use crate::audio_io::{resolve_audio_input, write_api_error_and_exit};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::Result;
use crate::net::{Client, FilePart};
use crate::origin::origin_header;

#[derive(Default)]
pub struct AudioTranscribeOptions {
    pub model: Option<String>,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub response_format: Option<String>,
    pub temperature: Option<String>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

pub fn audio_transcribe_command(
    client: &Client,
    creds: &Credentials,
    file_arg: Option<&str>,
    options: AudioTranscribeOptions,
) -> Result<()> {
    let session = get_access_token(client, creds);

    let input = resolve_audio_input(
        file_arg,
        options.filename.as_deref(),
        options.content_type.as_deref(),
    )?;

    let mut fields: Vec<(&str, &str)> = Vec::new();
    if let Some(v) = &options.model {
        fields.push(("model", v));
    }
    if let Some(v) = &options.language {
        fields.push(("language", v));
    }
    if let Some(v) = &options.prompt {
        fields.push(("prompt", v));
    }
    if let Some(v) = &options.response_format {
        fields.push(("response_format", v));
    }
    if let Some(v) = &options.temperature {
        fields.push(("temperature", v));
    }

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_multipart(
        &format!("{}{}", session.router_url, endpoints::AUDIO_TRANSCRIPTIONS),
        &[("Authorization", &bearer), origin_header()],
        FilePart {
            field: "file".to_string(),
            filename: input.filename,
            content_type: input.content_type,
            data: input.data,
        },
        &fields,
    )?;

    if !resp.ok() {
        write_api_error_and_exit(&resp);
    }

    let body = resp.text();
    let mut out = std::io::stdout();
    out.write_all(body.as_bytes())?;
    if !body.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    Ok(())
}
