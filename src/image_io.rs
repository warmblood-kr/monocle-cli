//! Shared image I/O for `monocle image generate`/`edit`: local file resolution
//! (MIME table scoped to what chat-proxy's `/v1/images/edits` accepts —
//! png/jpeg/webp) and writing back the `{data:[{b64_json}]}` response shape.
//! Mirrors `audio_io.rs`'s conventions but is kept separate since the MIME
//! table differs (see `attachment.rs`'s doc comment for the same precedent).

use std::path::Path;

use base64::Engine;

use crate::error::{AppError, Result};

fn mime_by_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[derive(Debug)]
pub struct ImageFile {
    pub data: Vec<u8>,
    pub filename: String,
    pub content_type: String,
}

/// Read a local image file for `/v1/images/edits` (the `image`/`mask` parts).
pub fn resolve_image_file(path_str: &str) -> Result<ImageFile> {
    let path = Path::new(path_str);
    if !path.exists() {
        return Err(AppError::new(format!("image file not found: {path_str}")));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let content_type = mime_by_ext(&ext)
        .ok_or_else(|| {
            AppError::new(format!(
                "unsupported image type: \"{ext}\" (expected png, jpg/jpeg, or webp) from {path_str}"
            ))
        })?
        .to_string();
    let data = std::fs::read(path)?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();
    Ok(ImageFile {
        data,
        filename,
        content_type,
    })
}

/// Decode `/v1/images/generations` or `/v1/images/edits`' `{data:[{b64_json}]}`
/// response and write each image to disk. A single result goes to `output`
/// verbatim; a second+ (when `--n` > 1) gets `-1`, `-2`, ... spliced before
/// the extension so no returned image is silently dropped.
pub fn write_images(response: &serde_json::Value, output: &str) -> Result<usize> {
    let data = response
        .get("data")
        .and_then(|d| d.as_array())
        .filter(|d| !d.is_empty())
        .ok_or_else(|| {
            AppError::new(format!(
                "unexpected response: no non-empty `data` array (got: {response})"
            ))
        })?;

    for (i, item) in data.iter().enumerate() {
        let b64 = item
            .get("b64_json")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AppError::new(format!("unexpected response: data[{i}] has no b64_json"))
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| AppError::new(format!("invalid base64 in data[{i}]: {e}")))?;
        let path = indexed_path(output, i);
        std::fs::write(&path, &bytes)?;
        eprintln!("Wrote {} bytes to {}", bytes.len(), path);
    }
    Ok(data.len())
}

fn indexed_path(output: &str, i: usize) -> String {
    if i == 0 {
        return output.to_string();
    }
    let path = Path::new(output);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = path.extension().and_then(|e| e.to_str());
    let filename = match ext {
        Some(ext) => format!("{stem}-{i}.{ext}"),
        None => format!("{stem}-{i}"),
    };
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join(filename).to_string_lossy().into_owned(),
        None => filename,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_image_file_reads_bytes_and_detects_mime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"pngbytes").unwrap();

        let img = resolve_image_file(path.to_str().unwrap()).expect("should resolve");
        assert_eq!(img.data, b"pngbytes");
        assert_eq!(img.content_type, "image/png");
        assert_eq!(img.filename, "a.png");
    }

    #[test]
    fn resolve_image_file_rejects_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.gif");
        std::fs::write(&path, b"gifbytes").unwrap();

        let err = resolve_image_file(path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("unsupported image type"));
    }

    #[test]
    fn write_images_decodes_and_writes_single_image() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let resp = serde_json::json!({"data": [{"b64_json": b64}]});

        let n = write_images(&resp, out.to_str().unwrap()).expect("should write");
        assert_eq!(n, 1);
        assert_eq!(std::fs::read(&out).unwrap(), b"hello");
    }

    #[test]
    fn write_images_splices_index_before_extension_for_n_greater_than_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.png");
        let b64_a = base64::engine::general_purpose::STANDARD.encode(b"aaa");
        let b64_b = base64::engine::general_purpose::STANDARD.encode(b"bbb");
        let resp = serde_json::json!({"data": [{"b64_json": b64_a}, {"b64_json": b64_b}]});

        write_images(&resp, out.to_str().unwrap()).expect("should write");
        assert_eq!(std::fs::read(&out).unwrap(), b"aaa");
        assert_eq!(std::fs::read(dir.path().join("out-1.png")).unwrap(), b"bbb");
    }

    #[test]
    fn write_images_errors_on_missing_data_array() {
        let err = write_images(&serde_json::json!({}), "out.png").unwrap_err();
        assert!(err.to_string().contains("no non-empty `data` array"));
    }
}
