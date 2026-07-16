//! Shared management commands (`help`/`status`/`config`) for the agent surfaces.
//!
//! These are handled **locally** — never sent to the agent/LLM — by BOTH the
//! interactive REPL (`crate::commands::agent`) and the ACP server (`crate::acp`).
//! This module is the single source of truth: the classifier that recognizes a
//! management-command invocation and the formatters that render their output, so
//! the two surfaces can't drift (DRY).

use std::path::Path;

use crate::credentials::Credentials;
use crate::util::{now_ms, parse_iso_ms};

/// A recognized management command. Handled locally on both surfaces; never
/// reaches the LLM.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Management {
    Help,
    Status,
    Config,
}

/// Recognize a management-command invocation. The leading `/` is REQUIRED so a
/// legitimate one-word prompt like `help` (meaning "help me") reaches the model
/// instead of being hijacked into a local response. ACP clients advertise these
/// with slash-less names (`help`) but render/transmit them with the slash
/// (`/help`), matching how the REPL's own dispatch works. Returns `None` for
/// anything else (normal input). Case-sensitive, exact word only.
pub(crate) fn management_command(input: &str) -> Option<Management> {
    match input.trim() {
        "/help" => Some(Management::Help),
        "/status" => Some(Management::Status),
        "/config" => Some(Management::Config),
        _ => None,
    }
}

/// Plain login snapshot for `status`, built from `creds.read()` so the formatter
/// stays pure (no IO / no `Credentials` dependency).
pub(crate) struct LoginView {
    pub(crate) tenant: String,
    pub(crate) email: String,
    pub(crate) expired: bool,
}

/// Same 5-minute buffer the auth refresh path uses, so "expired" here matches
/// when a turn would actually trigger a refresh.
const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;

/// Build a plain login snapshot from stored credentials, without calling
/// `get_access_token` (which would refresh / exit the process). Reads only —
/// shared by the REPL's `/status` and ACP's `status` handler.
pub(crate) fn login_view(creds: &Credentials) -> Option<LoginView> {
    let data = creds.read()?;
    let expired = match parse_iso_ms(&data.access_token_expires_at) {
        Some(exp) => now_ms() + EXPIRY_BUFFER_MS > exp,
        None => true,
    };
    Some(LoginView {
        tenant: format!("{} ({})", data.tenant_name, data.tenant_domain),
        email: data.email,
        expired,
    })
}

pub(crate) fn help_text() -> String {
    [
        "commands:",
        "  /help    show this help",
        "  /config  show session config (model, max-steps, workdir, session)",
        "  /status  show login status and session config",
        "  /diag    show diagnostics for the last turn (served model, endpoint, latency, tokens)",
        "  /model   show the current model, or `/model <id>` to switch it",
        "  /exit    quit the REPL (also /quit, Ctrl-D)",
    ]
    .join("\n")
}

pub(crate) fn config_text(
    model: &str,
    max_steps: usize,
    workdir: &Path,
    session: Option<&str>,
) -> String {
    format!(
        "model:     {}\nmax-steps: {}\nworkdir:   {}\nsession:   {}",
        model,
        max_steps,
        workdir.display(),
        session.unwrap_or("(none)"),
    )
}

pub(crate) fn status_text(
    login: Option<&LoginView>,
    model: &str,
    max_steps: usize,
    workdir: &Path,
    session: Option<&str>,
) -> String {
    let head = match login {
        Some(v) => format!(
            "logged in as {} — {}\naccess token: {}",
            v.email,
            v.tenant,
            if v.expired { "expired" } else { "valid" },
        ),
        None => "not logged in".to_string(),
    };
    format!(
        "{}\n{}",
        head,
        config_text(model, max_steps, workdir, session)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn management_command_requires_leading_slash() {
        assert_eq!(management_command("/help"), Some(Management::Help));
        assert_eq!(management_command("/status"), Some(Management::Status));
        assert_eq!(management_command("/config"), Some(Management::Config));
    }

    #[test]
    fn management_command_trims_surrounding_whitespace() {
        assert_eq!(management_command("  /help  "), Some(Management::Help));
    }

    #[test]
    fn management_command_returns_none_for_normal_input() {
        // Bare words are NOT commands — a legit prompt "help" reaches the model.
        assert_eq!(management_command("help"), None);
        assert_eq!(management_command("status"), None);
        assert_eq!(management_command("config"), None);
        assert_eq!(management_command("hello"), None);
        assert_eq!(management_command(""), None);
        assert_eq!(management_command("/bogus"), None);
        assert_eq!(management_command("helpme"), None);
        assert_eq!(management_command("help me"), None);
    }

    #[test]
    fn help_text_lists_each_command() {
        let h = help_text();
        for cmd in ["/help", "/config", "/status", "/diag", "/model", "/exit"] {
            assert!(h.contains(cmd), "help missing {cmd}: {h}");
        }
    }

    #[test]
    fn config_text_shows_fields_and_none_session() {
        let wd = PathBuf::from("/tmp/work");
        let out = config_text("claude-x", 12, &wd, None);
        assert!(out.contains("claude-x"), "{out}");
        assert!(out.contains("12"), "{out}");
        assert!(out.contains("/tmp/work"), "{out}");
        assert!(out.contains("(none)"), "{out}");
    }

    #[test]
    fn config_text_shows_named_session() {
        let wd = PathBuf::from("/tmp/work");
        let out = config_text("claude-x", 12, &wd, Some("mysess"));
        assert!(out.contains("mysess"), "{out}");
        assert!(!out.contains("(none)"), "{out}");
    }

    #[test]
    fn status_text_logged_out() {
        let wd = PathBuf::from("/tmp/work");
        let out = status_text(None, "m", 3, &wd, None);
        assert!(out.contains("not logged in"), "{out}");
        // Config block is still shown.
        assert!(out.contains("/tmp/work"), "{out}");
    }

    #[test]
    fn status_text_logged_in_valid_and_expired() {
        let wd = PathBuf::from("/tmp/work");
        let view = LoginView {
            tenant: "Acme (acme.example.com)".to_string(),
            email: "a@b.com".to_string(),
            expired: false,
        };
        let out = status_text(Some(&view), "m", 3, &wd, None);
        assert!(out.contains("logged in as a@b.com"), "{out}");
        assert!(out.contains("Acme (acme.example.com)"), "{out}");
        assert!(out.contains("valid"), "{out}");

        let expired = LoginView {
            expired: true,
            ..view
        };
        let out = status_text(Some(&expired), "m", 3, &wd, None);
        assert!(out.contains("expired"), "{out}");
    }
}
