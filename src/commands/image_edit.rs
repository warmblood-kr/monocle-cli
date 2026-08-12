//! `monocle image-edit` — a one-shot debugging probe for chat-proxy's
//! `POST /v1/images/edits`. It sends one image plus a prompt as multipart and
//! prints the raw response body to stdout, so a human can observe one real
//! response. Deliberately minimal (one image, no mask, no image arrays) — the
//! image twin of the `audio transcribe-azure` / `speech-azure` debug commands.

use std::io::Write;
use std::path::Path;

use crate::audio_io::write_api_error_and_exit;
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::net::{Client, FilePart};
use crate::origin::auth_headers;

pub struct ImageEditOptions {
    pub prompt: String,
    pub model: String,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub input_fidelity: Option<String>,
    pub n: Option<String>,
    pub content_type: Option<String>,
}

/// The MIME types the images/edits route accepts. Anything else comes back as a
/// 400 from the server, so an unknown extension is refused here instead of on
/// the wire. Delegates the extension→MIME lookup to `attachment::mime_by_ext`
/// (the canonical table) but narrows it: that table also maps `gif` →
/// `image/gif`, which this route rejects, so `gif` is excluded here even
/// though the shared table would resolve it.
fn mime_by_ext(ext: &str) -> Option<&'static str> {
    match crate::attachment::mime_by_ext(ext)? {
        "image/gif" => None,
        mime => Some(mime),
    }
}

/// Content type to send for the image part: an explicit `--content-type` wins,
/// otherwise it is guessed from the file extension. There is no
/// `application/octet-stream` fallback on purpose — a wrong content type is a
/// server-side 400, and failing here says why.
pub fn content_type_for(path: &str, override_ct: Option<&str>) -> Result<String> {
    if let Some(ct) = override_ct {
        return Ok(ct.to_string());
    }
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    mime_by_ext(&ext).map(str::to_string).ok_or_else(|| {
        AppError::new(format!(
            "unsupported image type (extension \"{ext}\" of {path}); the endpoint accepts image/png, image/jpeg, image/webp — use --content-type to override"
        ))
    })
}

/// The multipart text fields: `model` + `prompt` always, everything else only
/// when the user actually supplied it. The route rejects unknown scalar fields
/// with a 400, so this list stays closed — do not add fields casually.
pub fn form_fields(options: &ImageEditOptions) -> Vec<(&str, &str)> {
    let mut fields: Vec<(&str, &str)> = vec![
        ("model", options.model.as_str()),
        ("prompt", options.prompt.as_str()),
    ];
    if let Some(v) = &options.size {
        fields.push(("size", v));
    }
    if let Some(v) = &options.quality {
        fields.push(("quality", v));
    }
    if let Some(v) = &options.input_fidelity {
        fields.push(("input_fidelity", v));
    }
    if let Some(v) = &options.n {
        fields.push(("n", v));
    }
    fields
}

pub fn image_edit_command(
    client: &Client,
    creds: &Credentials,
    image_path: &str,
    options: ImageEditOptions,
) -> Result<()> {
    let path = Path::new(image_path);
    if !path.exists() {
        return Err(AppError::new(format!("Image file not found: {image_path}")));
    }
    let content_type = content_type_for(image_path, options.content_type.as_deref())?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();
    let data = std::fs::read(path)?;

    let session = get_access_token(client, creds);
    let url = format!("{}{}", session.router_url, endpoints::IMAGES_EDITS);
    // stdout stays pure response data; which host was actually hit goes to stderr.
    eprintln!("POST {url}");

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_multipart(
        &url,
        &auth_headers(&bearer),
        FilePart {
            field: "image".to_string(),
            filename,
            content_type,
            data,
        },
        &form_fields(&options),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ImageEditOptions {
        ImageEditOptions {
            prompt: "make it orange".to_string(),
            model: "some-image-model".to_string(),
            size: None,
            quality: None,
            input_fidelity: None,
            n: None,
            content_type: None,
        }
    }

    #[test]
    fn content_type_guessed_from_extension() {
        assert_eq!(content_type_for("a.png", None).unwrap(), "image/png");
        assert_eq!(content_type_for("a.jpg", None).unwrap(), "image/jpeg");
        assert_eq!(content_type_for("a.jpeg", None).unwrap(), "image/jpeg");
        assert_eq!(content_type_for("a.webp", None).unwrap(), "image/webp");
        // Case-insensitive, and path components must not confuse the lookup.
        assert_eq!(
            content_type_for("/tmp/Some Dir/A.PNG", None).unwrap(),
            "image/png"
        );
    }

    #[test]
    fn unsupported_extensions_are_rejected_not_octet_stream() {
        // gif is a valid image but this route rejects it; audio_io would have
        // silently produced application/octet-stream for all of these.
        for path in ["a.gif", "a.bmp", "a.txt", "noextension"] {
            let err = content_type_for(path, None)
                .expect_err(&format!("{path} must not resolve to a content type"));
            assert!(
                err.to_string().contains("unsupported image type"),
                "unexpected error for {path}: {err}"
            );
        }
    }

    #[test]
    fn explicit_content_type_overrides_the_extension() {
        assert_eq!(
            content_type_for("a.png", Some("image/webp")).unwrap(),
            "image/webp"
        );
        // …and rescues a file whose extension we cannot map.
        assert_eq!(
            content_type_for("blob", Some("image/png")).unwrap(),
            "image/png"
        );
    }

    #[test]
    fn form_fields_are_sparse_by_default() {
        let opts = options();
        let fields = form_fields(&opts);
        assert_eq!(
            fields,
            vec![("model", "some-image-model"), ("prompt", "make it orange")]
        );
    }

    #[test]
    fn form_fields_include_only_supplied_options() {
        let mut opts = options();
        opts.size = Some("1024x1024".to_string());
        opts.n = Some("2".to_string());

        let fields = form_fields(&opts);
        assert_eq!(
            fields,
            vec![
                ("model", "some-image-model"),
                ("prompt", "make it orange"),
                ("size", "1024x1024"),
                ("n", "2"),
            ]
        );
        // quality / input_fidelity were not supplied, so they must not be sent.
        let names: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        assert!(!names.contains(&"quality"));
        assert!(!names.contains(&"input_fidelity"));
    }

    #[test]
    fn form_fields_never_send_a_field_outside_the_allowed_set() {
        // The route 400s on unknown scalar fields — this is the guard rail.
        const ALLOWED: [&str; 6] = ["model", "prompt", "size", "quality", "input_fidelity", "n"];
        let mut opts = options();
        opts.size = Some("1024x1024".to_string());
        opts.quality = Some("high".to_string());
        opts.input_fidelity = Some("high".to_string());
        opts.n = Some("1".to_string());

        let fields = form_fields(&opts);
        assert_eq!(fields.len(), ALLOWED.len(), "every option should be sent");
        for (name, _) in &fields {
            assert!(ALLOWED.contains(name), "unexpected form field: {name}");
        }
    }
}
