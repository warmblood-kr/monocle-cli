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
        // Error text can embed server bodies, ids and user input: strip terminal
        // control characters (keeping newlines/tabs) at the single place all of
        // it is rendered.
        f.write_str(&crate::util::body(&self.0))
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

#[cfg(test)]
mod tests {
    use super::AppError;

    #[test]
    fn display_strips_terminal_controls_but_keeps_lines() {
        let e = AppError::new("bad\x1b[2J body\nsecond\r line");
        assert_eq!(e.to_string(), "bad[2J body\nsecond line");
    }
}
