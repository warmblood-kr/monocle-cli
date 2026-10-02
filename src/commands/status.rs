use std::path::Path;

use serde_json::Value;

use crate::auth::ensure_fresh_token;
use crate::credentials::Credentials;
use crate::net::Client;
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

pub fn status_command(store: &Credentials, home: &Path) {
    let mut creds = match store.read() {
        Some(c) => c,
        None => {
            eprintln!("Not logged in. Run `monocle login --tenant <domain>` first.");
            return;
        }
    };

    // A status check should opportunistically keep credentials fresh without
    // turning a refresh failure into a status-command failure.
    if ensure_fresh_token(&Client::new(), store).is_ok() {
        if let Some(refreshed) = store.read() {
            creds = refreshed;
        }
    }

    let now = now_ms();

    eprintln!("Tenant: {} ({})", creds.tenant_domain, creds.tenant_name);
    eprintln!("User: {}", creds.email);

    // Access token.
    match parse_iso_ms(&creds.access_token_expires_at) {
        Some(exp) if now <= exp => {
            eprintln!(
                "Access Token: Valid ({} remaining)",
                format_remaining(exp - now)
            );
        }
        _ => eprintln!("Access Token: Expired"),
    }

    // Refresh token.
    match parse_iso_ms(&creds.refresh_token_expires_at) {
        Some(exp) if now <= exp => {
            eprintln!(
                "Refresh Token: Valid ({} remaining)",
                format_remaining(exp - now)
            );
        }
        _ => {
            eprintln!("Refresh Token: Expired");
            eprintln!("\n⚠ Refresh token has expired. Run `monocle login --tenant <domain>` to re-authenticate.");
        }
    }

    // Claude Code configuration.
    let settings_path = home.join(".claude").join("settings.json");
    let claude_configured = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .map(|s| s.get("apiKeyHelper").and_then(Value::as_str) == Some("monocle token"))
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
