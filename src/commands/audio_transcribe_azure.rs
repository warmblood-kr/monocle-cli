use std::io::Write;

use serde_json::{Map, Value};

use crate::audio_io::{resolve_audio_input, write_api_error_and_exit};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::{Client, FilePart};
use crate::origin::auth_headers;

#[derive(Default)]
pub struct AudioTranscribeAzureOptions {
    pub locales: Option<Vec<String>>,
    pub diarization: bool,
    pub profanity: Option<String>,
    pub channels: Option<String>,
    pub definition: Option<String>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

fn build_definition(opts: &AudioTranscribeAzureOptions) -> Result<Option<String>> {
    if let Some(def) = &opts.definition {
        // Fail fast on bad JSON (matches the TS `JSON.parse` validation).
        serde_json::from_str::<Value>(def)?;
        return Ok(Some(def.clone()));
    }

    let mut def = Map::new();
    if let Some(locales) = &opts.locales {
        if !locales.is_empty() {
            def.insert(
                "locales".to_string(),
                Value::Array(locales.iter().map(|l| Value::String(l.clone())).collect()),
            );
        }
    }
    if opts.diarization {
        def.insert("diarizationEnabled".to_string(), Value::Bool(true));
    }
    if let Some(p) = &opts.profanity {
        def.insert("profanityFilterMode".to_string(), Value::String(p.clone()));
    }
    if let Some(channels) = &opts.channels {
        let nums: Vec<Value> = channels
            .split(',')
            .filter_map(|c| c.trim().parse::<i64>().ok())
            .map(|n| Value::Number(n.into()))
            .collect();
        def.insert("channels".to_string(), Value::Array(nums));
    }

    if def.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(def).to_string()))
    }
}

pub fn audio_transcribe_azure_command(
    client: &Client,
    creds: &Credentials,
    file_arg: Option<&str>,
    options: AudioTranscribeAzureOptions,
) -> Result<()> {
    let definition = build_definition(&options)?.ok_or_else(|| {
        AppError::new(
            "Azure Fast Transcription requires a `definition` JSON. Pass at least one of:\n\
             \x20 --locale <code>   (repeatable, e.g. --locale en-US --locale ko-KR)\n\
             \x20 --diarization\n\
             \x20 --profanity <None|Removed|Masked|Tags>\n\
             \x20 --channels <0,1>\n\
             \x20 --definition <raw JSON>",
        )
    })?;

    let session = get_access_token(client, creds);

    let input = resolve_audio_input(
        file_arg,
        options.filename.as_deref(),
        options.content_type.as_deref(),
    )?;

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_multipart(
        &format!("{}{}", session.router_url, endpoints::AZURE_SPEECH_TO_TEXT),
        &auth_headers(&bearer),
        vec![FilePart {
            field: "audio".to_string(),
            filename: input.filename,
            content_type: input.content_type,
            data: input.data,
        }],
        // Server expects `definition` as a plain string form field.
        &[("definition", &definition)],
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
