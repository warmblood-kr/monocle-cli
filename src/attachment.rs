//! File/image attachment resolution for `monocle chat` (vision eval
//! primitive). Mirrors `audio_io.rs`'s patterns for file/MIME/error handling,
//! but is kept separate since its MIME table is image-only (audio_io's is
//! audio-only) — see the file/image-attachments plan.

use std::path::Path;

use base64::Engine;

use crate::agent::providers::ImageAttachment;
use crate::error::{AppError, Result};

/// Sentence punctuation trimmed off the trailing end of a captured `file:`
/// token before it is treated as a path (e.g. `file:./a.png.` in "...a.png.").
const TRAILING_PUNCT: [char; 9] = ['.', ',', ';', ':', '!', '?', ')', '\'', '"'];

pub(crate) fn mime_by_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Resolve a `--file` value or an inband `file:<path>` reference into an
/// [`ImageAttachment`].
///
/// - `http://` / `https://` → passed through verbatim as a remote
///   `image_url.url` (no fetch, no base64).
/// - Anything else is treated as a local path: read the bytes, guess MIME by
///   extension, and if it's `image/*` encode as a `data:<mime>;base64,...`
///   URI. A missing file or a non-image MIME is a hard, typed error (matches
///   the file-not-found style in `audio_io::resolve_audio_input`).
pub fn resolve(value: &str) -> Result<ImageAttachment> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(ImageAttachment {
            url: value.to_string(),
        });
    }

    let path = Path::new(value);
    if !path.exists() {
        return Err(AppError::new(format!("file not found: {value}")));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let mime = mime_by_ext(&ext).unwrap_or("application/octet-stream");
    if !mime.starts_with("image/") {
        return Err(AppError::new(format!(
            "unsupported type: {mime} (from extension \"{ext}\" of {value})"
        )));
    }

    let data = std::fs::read(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Ok(ImageAttachment {
        url: format!("data:{mime};base64,{b64}"),
    })
}

/// Scan `text` for inband `file:<path>` tokens (Org-mode-style — NOT a strict
/// `file://` URI; no `//` required, relative paths welcome), returning the
/// text with each matched token removed and the ordered list of raw path
/// strings found (trailing sentence punctuation trimmed off each).
///
/// All whitespace other than the removed tokens themselves is preserved
/// verbatim, so text with no `file:` tokens comes back byte-identical — this
/// is the regression oracle for the common (no-attachment) path.
pub fn extract_inband_refs(text: &str) -> (String, Vec<String>) {
    let mut refs = Vec::new();
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    loop {
        let word_start = match rest.find(|c: char| !c.is_whitespace()) {
            Some(idx) => idx,
            None => {
                result.push_str(rest);
                break;
            }
        };
        // Whitespace before the next word — keep verbatim.
        result.push_str(&rest[..word_start]);
        let after_ws = &rest[word_start..];
        let word_end = after_ws
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after_ws.len());
        let word = &after_ws[..word_end];

        // Marker match is case-insensitive (`File:`, `FILE:`, ... all count —
        // mobile autocapitalize / sentence-case habit shouldn't silently drop
        // an attachment), but only the 5-byte marker itself is case-folded for
        // the comparison; the path remainder is used verbatim since filesystem
        // paths are case-sensitive on Linux/macOS.
        let is_file_marker = word
            .get(..5)
            .map(|prefix| prefix.eq_ignore_ascii_case("file:"))
            .unwrap_or(false);

        if is_file_marker {
            let path = &word[5..];
            let trimmed = path.trim_end_matches(TRAILING_PUNCT.as_slice());
            if trimmed.is_empty() {
                // "file:" with nothing usable after it — not a real token.
                result.push_str(word);
            } else {
                refs.push(trimmed.to_string());
                // Token is stripped from the outgoing text entirely.
            }
        } else {
            result.push_str(word);
        }

        rest = &after_ws[word_end..];
    }

    (result, refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolves_local_png_to_data_uri() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"not a real png but bytes are enough").unwrap();

        let img = resolve(path.to_str().unwrap()).expect("should resolve");
        let expected_b64 = base64::engine::general_purpose::STANDARD
            .encode(b"not a real png but bytes are enough");
        assert_eq!(img.url, format!("data:image/png;base64,{expected_b64}"));
    }

    #[test]
    fn http_url_passes_through_unencoded() {
        let img = resolve("https://example.com/a.png").expect("should pass through");
        assert_eq!(img.url, "https://example.com/a.png");

        let img = resolve("http://example.com/a.png").expect("should pass through");
        assert_eq!(img.url, "http://example.com/a.png");
    }

    #[test]
    fn nonexistent_path_errors() {
        let err = resolve("/no/such/path/ever.png").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn non_image_extension_errors_with_typed_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.heic");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello").unwrap();

        let err = resolve(path.to_str().unwrap()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with("unsupported type:"),
            "unexpected message: {msg}"
        );
        // Regression: the rejected extension (and the offending path) must be
        // named in the error — otherwise the user has no way to diagnose
        // "what happened with this file/format" (the whole point of the
        // feature).
        assert!(
            msg.contains("heic"),
            "error should name the rejected extension: {msg}"
        );
        assert!(
            msg.contains(path.to_str().unwrap()),
            "error should name the offending path: {msg}"
        );
    }

    #[test]
    fn extract_inband_refs_strips_tokens_and_trims_trailing_punct() {
        let (cleaned, refs) = extract_inband_refs("compare file:./a.png and file:./b.png.");
        assert_eq!(refs, vec!["./a.png".to_string(), "./b.png".to_string()]);
        // Tokens removed; surrounding words/whitespace otherwise untouched.
        assert!(!cleaned.contains("file:"));
        assert!(cleaned.contains("compare"));
        assert!(cleaned.contains("and"));
    }

    #[test]
    fn inband_marker_is_case_insensitive_but_path_case_is_preserved() {
        let (cleaned, refs) = extract_inband_refs("look at File:./Photo.png please");
        assert_eq!(refs, vec!["./Photo.png".to_string()]);
        assert!(!cleaned.to_lowercase().contains("file:"));
        assert!(cleaned.contains("look"));
        assert!(cleaned.contains("please"));

        let (cleaned2, refs2) = extract_inband_refs("FILE:./B.PNG");
        assert_eq!(refs2, vec!["./B.PNG".to_string()]);
        assert!(cleaned2.trim().is_empty());

        // Lowercase marker still works unchanged (regression).
        let (_, refs3) = extract_inband_refs("file:./a.png");
        assert_eq!(refs3, vec!["./a.png".to_string()]);
    }

    #[test]
    fn bare_https_substring_is_left_untouched() {
        let (cleaned, refs) = extract_inband_refs("see https://example.com/x.png please");
        assert!(refs.is_empty());
        assert_eq!(cleaned, "see https://example.com/x.png please");
    }

    #[test]
    fn no_tokens_is_byte_identical() {
        let text = "line one\nline two   with   spaces\tand a tab";
        let (cleaned, refs) = extract_inband_refs(text);
        assert!(refs.is_empty());
        assert_eq!(cleaned, text);
    }
}
