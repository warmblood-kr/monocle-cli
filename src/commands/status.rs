use std::path::Path;

use serde_json::Value;

use crate::auth::{access_expiry_ms, ensure_fresh_token};
use crate::credentials::Credentials;
use crate::net::Client;
use crate::profiles::Selection;
use crate::util::{now_ms, parse_iso_ms};

fn format_remaining(ms: i64) -> String {
    let total_minutes = ms / (1000 * 60);
    let days = total_minutes / (60 * 24);
    let hours = (total_minutes % (60 * 24)) / 60;
    let minutes = total_minutes % 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn status_command(store: &Credentials, home: &Path, selection: &Selection) {
    eprintln!("Profile: {}", selection.profile);
    eprintln!("Environment: {}", selection.environment);
    let mut creds = match store.read() {
        Some(c) => c,
        None => {
            eprintln!(
                "Not logged in for profile '{}' ({}). Run `monocle --profile {} login --env {}` first.",
                selection.profile, selection.environment, selection.profile, selection.environment
            );
            return;
        }
    };

    if let Err(e) = ensure_fresh_token(&Client::new(), store) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    if let Some(refreshed) = store.read() {
        creds = refreshed;
    }

    let now = now_ms();

    eprintln!("Tenant: {} ({})", creds.tenant_domain, creds.tenant_name);
    eprintln!("User: {}", creds.email);

    // Access token.
    match access_expiry_ms(&creds) {
        Some(exp) if now <= exp => {
            eprintln!(
                "Access Token: Valid ({} remaining)",
                format_remaining(exp - now)
            );
        }
        Some(_) => eprintln!("Access Token: Expired"),
        None => eprintln!("Access Token: Expiry unknown"),
    }

    // The cached refresh timestamp is an estimate, not provider evidence.
    eprintln!(
        "{}",
        refresh_expiry_diagnostic(&creds.refresh_token_expires_at, now)
    );

    // Claude Code configuration.
    let settings_path = home.join(".claude").join("settings.json");
    let token_helper = store.token_helper();
    let claude_configured = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .map(|s| s.get("apiKeyHelper").and_then(Value::as_str) == Some(token_helper.as_str()))
        .unwrap_or(false);

    eprintln!(
        "Claude Code: {}",
        if claude_configured {
            "Configured"
        } else {
            "Not configured"
        }
    );
}

fn refresh_expiry_diagnostic(timestamp: &str, now: i64) -> String {
    match parse_iso_ms(timestamp) {
        Some(exp) if now <= exp => format!(
            "Refresh Token: Cached estimate ({} remaining; provider validity not established)",
            format_remaining(exp - now)
        ),
        Some(_) => "Refresh Token: Cached expiry estimate passed; provider validity unknown. This does not establish a refresh-token failure.".into(),
        None => "Refresh Token: Expiry estimate unavailable; provider validity unknown".into(),
    }
}

#[cfg(test)]
mod expiry_diagnostic_tests {
    use super::*;
    #[test]
    fn cached_estimates_never_claim_provider_validity_or_failure() {
        let now = parse_iso_ms("2026-01-01T00:00:00.000Z").unwrap();
        let future = refresh_expiry_diagnostic("2099-01-01T00:00:00.000Z", now);
        assert!(future.contains("Cached estimate") && future.contains("validity not established"));
        let past = refresh_expiry_diagnostic("2000-01-01T00:00:00.000Z", now);
        assert!(past.contains("does not establish a refresh-token failure"));
        assert!(!past.contains("login"));
        assert!(refresh_expiry_diagnostic("bad", now).contains("unavailable"));
    }
}
