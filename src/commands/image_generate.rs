//! `monocle image-generate` — a one-shot debugging probe for chat-proxy's
//! `POST /v1/images/generations`. It sends a text prompt as JSON and prints
//! the raw response body to stdout, so a human can observe one real
//! response. Deliberately minimal — the text-to-image twin of `image-edit`
//! (no input image, no mask; the endpoint itself takes JSON, not multipart,
//! since there's no file to attach).

use std::io::Write;

use serde_json::{json, Value};

use crate::audio_io::write_api_error_and_exit;
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::origin::auth_headers;

pub struct ImageGenerateOptions {
    pub prompt: String,
    pub model: String,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub n: Option<String>,
}

/// The JSON request body: `model` + `prompt` always, everything else only
/// when the user actually supplied it — mirrors `image_edit::form_fields`'s
/// sparse-by-default shape, just as a JSON object instead of multipart text
/// fields (chat-proxy's `/v1/images/generations` route reads a plain JSON
/// body; see `app/converters/images.py::image_to_chat_format`).
///
/// `n` is typed as a JSON integer on the wire (chat-proxy compares it
/// numerically, e.g. its `n > 1` Gemini guard), so an unparsable `--n` is a
/// hard error here rather than silently sent as a string or dropped.
pub fn request_body(options: &ImageGenerateOptions) -> Result<Value> {
    let mut body = json!({
        "model": options.model,
        "prompt": options.prompt,
    });
    let obj = body
        .as_object_mut()
        .expect("json!({...}) is always an object");
    if let Some(v) = &options.size {
        obj.insert("size".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &options.quality {
        obj.insert("quality".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &options.n {
        let n: i64 = v
            .trim()
            .parse()
            .map_err(|_| AppError::new(format!("--n must be a positive integer, got \"{v}\"")))?;
        obj.insert("n".to_string(), Value::from(n));
    }
    Ok(body)
}

pub fn image_generate_command(
    client: &Client,
    creds: &Credentials,
    options: ImageGenerateOptions,
) -> Result<()> {
    let body = request_body(&options)?;

    let session = get_access_token(client, creds);
    let url = format!("{}{}", session.router_url, endpoints::IMAGES_GENERATIONS);
    // stdout stays pure response data; which host was actually hit goes to stderr.
    eprintln!("POST {url}");

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_json(&url, &auth_headers(&bearer), &body)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ImageGenerateOptions {
        ImageGenerateOptions {
            prompt: "a cat wearing a hat".to_string(),
            model: "some-image-model".to_string(),
            size: None,
            quality: None,
            n: None,
        }
    }

    #[test]
    fn request_body_is_sparse_by_default() {
        let body = request_body(&options()).unwrap();
        assert_eq!(
            body,
            json!({"model": "some-image-model", "prompt": "a cat wearing a hat"})
        );
    }

    #[test]
    fn request_body_includes_only_supplied_options() {
        let mut opts = options();
        opts.size = Some("1024x1024".to_string());
        opts.n = Some("2".to_string());

        let body = request_body(&opts).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "some-image-model",
                "prompt": "a cat wearing a hat",
                "size": "1024x1024",
                "n": 2,
            })
        );
        // quality was not supplied, so it must not be sent.
        assert!(body.get("quality").is_none());
    }

    #[test]
    fn request_body_sends_n_as_a_json_integer_not_a_string() {
        let mut opts = options();
        opts.n = Some("3".to_string());
        let body = request_body(&opts).unwrap();
        assert_eq!(body["n"], json!(3));
        assert!(body["n"].is_number());
    }

    #[test]
    fn request_body_rejects_a_non_numeric_n() {
        let mut opts = options();
        opts.n = Some("lots".to_string());
        let err = request_body(&opts).unwrap_err();
        assert!(
            err.to_string().contains("--n must be a positive integer"),
            "unexpected error: {err}"
        );
    }
}
