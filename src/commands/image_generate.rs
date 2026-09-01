use serde_json::json;

use crate::audio_io::{read_stdin_text, write_api_error_and_exit};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::image_io::write_images;
use crate::net::Client;
use crate::origin::auth_headers;

pub struct ImageGenerateOptions {
    pub model: String,
    pub size: Option<String>,
    pub n: Option<u32>,
    pub quality: Option<String>,
    pub output: String,
}

pub fn image_generate_command(
    client: &Client,
    creds: &Credentials,
    prompt_arg: Option<&str>,
    options: ImageGenerateOptions,
) -> Result<()> {
    let mut prompt = prompt_arg.unwrap_or("").to_string();
    if prompt.is_empty() || prompt == "-" {
        prompt = read_stdin_text()?.trim().to_string();
    }
    if prompt.is_empty() {
        return Err(AppError::new(
            "No prompt. Pass one as an argument or pipe it via stdin.",
        ));
    }

    let session = get_access_token(client, creds);

    let mut payload = json!({
        "model": options.model,
        "prompt": prompt,
    });
    if let Some(size) = &options.size {
        payload["size"] = json!(size);
    }
    if let Some(n) = options.n {
        payload["n"] = json!(n);
    }
    if let Some(quality) = &options.quality {
        payload["quality"] = json!(quality);
    }

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_json(
        &format!("{}{}", session.router_url, endpoints::IMAGE_GENERATIONS),
        &auth_headers(&bearer),
        &payload,
    )?;

    if !resp.ok() {
        write_api_error_and_exit(&resp);
    }

    let body: serde_json::Value = resp.json()?;
    write_images(&body, &options.output)?;
    Ok(())
}
