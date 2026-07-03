//! Single error type. Its `Display` is the message users see — top-level
//! `main` prints `Error: {msg}` for propagated errors, mirroring the TS
//! `runAction` wrapper. Commands that exit with their own (non-`Error:`-prefixed)
//! message call `std::process::exit(1)` directly, exactly like the TS code.

use std::fmt;

#[derive(Debug)]
pub struct AppError(pub String);

impl AppError {
    pub fn new(msg: impl Into<String>) -> Self {
        AppError(msg.into())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AppError {}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
