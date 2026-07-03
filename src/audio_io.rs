//! Shared audio I/O: input resolution (file or stdin), MIME guessing, and
//! binary output handling with a TTY guard.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use crate::error::{AppError, Result};
use crate::net::Resp;

fn mime_by_ext(ext: &str) -> Option<&'static str> {
    match ext {
        ".wav" => Some("audio/wav"),
        ".mp3" => Some("audio/mpeg"),
        ".mp4" => Some("audio/mp4"),
        ".m4a" => Some("audio/mp4"),
        ".aac" => Some("audio/aac"),
        ".flac" => Some("audio/flac"),
        ".ogg" => Some("audio/ogg"),
        ".oga" => Some("audio/ogg"),
        ".opus" => Some("audio/ogg"),
        ".webm" => Some("audio/webm"),
        _ => None,
    }
}

fn dotted_ext(name: &str) -> String {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some(e) => format!(".{}", e.to_lowercase()),
        None => String::new(),
    }
}

pub struct AudioInput {
    pub data: Vec<u8>,
    pub filename: String,
    pub content_type: String,
}

pub fn read_stdin_buffer() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn read_stdin_text() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

pub fn resolve_audio_input(
    file_arg: Option<&str>,
    filename_opt: Option<&str>,
    content_type_opt: Option<&str>,
) -> Result<AudioInput> {
    if let Some(file) = file_arg {
        if file != "-" {
            let path = Path::new(file);
            if !path.exists() {
                return Err(AppError::new(format!("Audio file not found: {file}")));
            }
            let ext = dotted_ext(file);
            let data = std::fs::read(path)?;
            let filename = filename_opt.map(str::to_string).unwrap_or_else(|| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string()
            });
            let content_type = content_type_opt
                .map(str::to_string)
                .or_else(|| mime_by_ext(&ext).map(str::to_string))
                .unwrap_or_else(|| "application/octet-stream".to_string());
            return Ok(AudioInput {
                data,
                filename,
                content_type,
            });
        }
    }

    let data = read_stdin_buffer()?;
    if data.is_empty() {
        return Err(AppError::new(
            "No audio input. Pass a file path or pipe audio to stdin (`--filename` recommended when piping).",
        ));
    }
    let filename = filename_opt.unwrap_or("audio.wav").to_string();
    let ext = dotted_ext(&filename);
    let content_type = content_type_opt
        .map(str::to_string)
        .or_else(|| mime_by_ext(&ext).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok(AudioInput {
        data,
        filename,
        content_type,
    })
}

/// Print an API error (status + body) to stderr and exit 1.
pub fn write_api_error_and_exit(resp: &Resp) -> ! {
    let body = resp.text();
    eprintln!("API error {} {}", resp.status, resp.status_text);
    eprint!("{body}");
    if !body.ends_with('\n') {
        eprintln!();
    }
    std::process::exit(1);
}

pub fn ensure_not_writing_binary_to_tty(output: Option<&str>) -> Result<()> {
    if output.is_none() && std::io::stdout().is_terminal() {
        return Err(AppError::new(
            "Refusing to write binary audio to a terminal. Use `-o <path>` or pipe stdout to a file.",
        ));
    }
    Ok(())
}

pub fn write_binary_output(buffer: &[u8], output: Option<&str>) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, buffer)?;
            eprintln!("Wrote {} bytes to {}", buffer.len(), path);
        }
        None => {
            let mut stdout = std::io::stdout();
            stdout.write_all(buffer)?;
            stdout.flush()?;
        }
    }
    Ok(())
}
