//! A tiny diagnostics log for otherwise-invisible network/streaming failures
//! (e.g. "error decoding response body" during `monocle agent` streaming).
//!
//! This is deliberately NOT a logging framework — no crate, no persistent
//! logger/subscriber. Just a plain append-only text file at
//! `~/.monocle/cli.log`, written only at the handful of `net.rs` sites where
//! an error is about to be collapsed into a generic `AppError(String)` and
//! the request context (method, URL, underlying error) would otherwise be
//! lost for good.
//!
//! Top-level error-display sites can check [`was_logged`] to add a one-line
//! hint pointing the user at the log, without every `AppError` needing to
//! carry structured diagnostic data.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::util::{home_dir, now_ms, to_iso};

/// Set once `log_network_error` has successfully written a line this run, so
/// top-level error printers know whether pointing the user at the log file is
/// actually useful.
static WAS_LOGGED: AtomicBool = AtomicBool::new(false);

/// The friendly path shown to users, matching this codebase's existing
/// `~/.monocle/...` convention (README.md, doc comments) rather than the
/// fully resolved OS path.
pub fn display_path() -> &'static str {
    "~/.monocle/cli.log"
}

/// The real, resolved log file path: `<home>/.monocle/cli.log`.
pub fn log_path() -> PathBuf {
    home_dir().join(".monocle").join("cli.log")
}

/// Whether a diagnostic line has been written since the last [`reset`] (or
/// since process start, if `reset` was never called).
pub fn was_logged() -> bool {
    WAS_LOGGED.load(Ordering::Relaxed)
}

/// Clear the flag. A one-shot command process (e.g. `main.rs`) never needs
/// this — it exits right after printing its one error. A long-lived REPL
/// (`monocle agent`, `monocle chat`) must call this at the *start* of each
/// turn, before that turn's work runs, so a later turn's hint reflects only
/// whether *that* turn logged a network error — not a stale flag left over
/// from an earlier turn.
pub fn reset() {
    WAS_LOGGED.store(false, Ordering::Relaxed);
}

/// Append one diagnostic line: timestamp, HTTP method + (redacted) URL, and
/// the raw underlying error text — never the response body or credentials.
///
/// Best-effort: this must never mask or replace the original error, so any
/// failure to write (missing home dir, permissions, disk full, ...) is
/// silently dropped.
pub fn log_network_error(method: &str, url: &str, err: &str) {
    let path = log_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    harden_permissions(&path);
    let line = format!(
        "{} {method} {} — {err}\n",
        to_iso(now_ms()),
        redact_url(url)
    );
    if file.write_all(line.as_bytes()).is_ok() {
        WAS_LOGGED.store(true, Ordering::Relaxed);
    }
}

/// Best-effort `chmod 600` on Unix, mirroring `credentials.rs` (no secrets are
/// logged here, but there's no reason to leave a diagnostics file
/// world-readable either). A no-op on other platforms; failures are ignored —
/// same best-effort posture as the rest of this module.
#[cfg(unix)]
fn harden_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_permissions(_path: &std::path::Path) {}

/// Strip any query string before logging. Every call site today sends auth
/// only via the `Authorization` header (never as a URL/query param), so the
/// bare URL is already safe — this is defense in depth against a future call
/// site that embeds something sensitive in `?...`.
fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<redacted>"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{redact_url, reset, was_logged, WAS_LOGGED};
    use std::sync::atomic::Ordering;

    #[test]
    fn redacts_query_string() {
        assert_eq!(
            redact_url("https://api.example.com/v1/chat?token=secret"),
            "https://api.example.com/v1/chat?<redacted>"
        );
    }

    #[test]
    fn leaves_bare_url_unchanged() {
        assert_eq!(
            redact_url("https://api.example.com/v1/chat"),
            "https://api.example.com/v1/chat"
        );
    }

    #[test]
    fn reset_clears_the_flag() {
        // `WAS_LOGGED` is a process-global, so drive it directly rather than
        // via `log_network_error` (which would touch the real filesystem and
        // race other tests running in parallel).
        WAS_LOGGED.store(true, Ordering::Relaxed);
        assert!(was_logged());
        reset();
        assert!(!was_logged());
    }
}
