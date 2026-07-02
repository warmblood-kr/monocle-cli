//! Session persistence — append-only JSONL of the conversation, replayed on
//! resume (SDD §9 Phase 2). Path-injectable and pure so it is trivially tested
//! against a tempdir; no globals.
//!
//! One `Message` per line. Resume = read every line back into the conversation;
//! continue = append only the new messages. Append-only keeps the file cheap to
//! grow and mirrors the in-memory append-only conversation (prompt-cache friendly).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::agent::providers::Message;
use crate::error::{AppError, Result};

pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replay the persisted conversation (empty if the file does not exist yet).
    pub fn load(&self) -> Result<Vec<Message>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let mut messages = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            match serde_json::from_str::<Message>(line) {
                Ok(message) => messages.push(message),
                Err(e) => {
                    // Tolerate a truncated/corrupt FINAL line (e.g. the process was
                    // killed mid-write); a corrupt earlier line is real corruption.
                    if i + 1 == lines.len() {
                        eprintln!("Warning: dropping incomplete final session line: {e}");
                        break;
                    }
                    return Err(AppError::new(format!(
                        "corrupt session line {}: {e}",
                        i + 1
                    )));
                }
            }
        }
        Ok(messages)
    }

    /// Append messages (one JSON object per line). Creates the file/dir as needed.
    pub fn append(&self, messages: &[Message]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        for message in messages {
            let line = serde_json::to_string(message)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

/// Default file for a named session under the user's monocle dir:
/// `<home>/.monocle/agent/<name>.jsonl`.
pub fn session_path(home: &Path, name: &str) -> PathBuf {
    home.join(".monocle")
        .join("agent")
        .join(format!("{name}.jsonl"))
}
