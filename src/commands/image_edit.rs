use crate::audio_io::{read_stdin_text, write_api_error_and_exit};
use crate::auth::get_access_token;
use crate::credentials::Credentials;
use crate::endpoints;
use crate::error::{AppError, Result};
use crate::image_io::{resolve_image_file, write_images};
use crate::net::{Client, FilePart};
use crate::origin::auth_headers;

pub struct ImageEditOptions {
    pub model: String,
    pub mask: Option<String>,
    pub size: Option<String>,
    pub n: Option<u32>,
    pub quality: Option<String>,
    pub output: String,
}

fn file_part(field: &str, image: crate::image_io::ImageFile) -> FilePart {
    FilePart {
        field: field.to_string(),
        filename: image.filename,
        content_type: image.content_type,
        data: image.data,
    }
}

pub fn image_edit_command(
    client: &Client,
    creds: &Credentials,
    image_path: &str,
    prompt_arg: Option<&str>,
    options: ImageEditOptions,
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

    let mut files = vec![file_part("image", resolve_image_file(image_path)?)];
    if let Some(mask_path) = &options.mask {
        files.push(file_part("mask", resolve_image_file(mask_path)?));
    }

    let n_str = options.n.map(|n| n.to_string());
    let mut fields: Vec<(&str, &str)> = vec![("model", &options.model), ("prompt", &prompt)];
    if let Some(size) = &options.size {
        fields.push(("size", size));
    }
    if let Some(n) = &n_str {
        fields.push(("n", n));
    }
    if let Some(quality) = &options.quality {
        fields.push(("quality", quality));
    }

    let bearer = format!("Bearer {}", session.token);
    let resp = client.post_multipart(
        &format!("{}{}", session.router_url, endpoints::IMAGE_EDITS),
        &auth_headers(&bearer),
        files,
        &fields,
    )?;

    if !resp.ok() {
        write_api_error_and_exit(&resp);
    }

    let body: serde_json::Value = resp.json()?;
    write_images(&body, &options.output)?;
    Ok(())
}
