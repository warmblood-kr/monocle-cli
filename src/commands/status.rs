use std::path::Path;

use serde_json::Value;

use crate::auth::ensure_fresh_token;
use crate::credentials::Credentials;
use crate::net::Client;
use crate::profiles::Selection;
use crate::util::{now_ms, parse_iso_ms};

pub(crate) fn format_remaining(ms: i64) -> String {
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

/// Prints the status block to stdout (it is the command's result). Returns
/// `false` when the profile is not logged in, which the caller turns into exit 1;
/// the "not logged in" explanation itself goes to stderr.
pub fn status_command(store: &Credentials, home: &Path, selection: &Selection) -> bool {
    println!("Profile: {}", selection.profile);
    println!("Environment: {}", selection.environment);
    let mut creds = match store.read() {
        Some(c) => c,
        None => {
            eprintln!(
                "Not logged in for profile '{}' ({}). Run `monocle --profile {} login --env {}` first.",
                selection.profile, selection.environment, selection.profile, selection.environment
            );
            return false;
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

    println!("Tenant: {} ({})", creds.tenant_domain, creds.tenant_name);
    println!("User: {}", creds.email);

    // Access token.
    match parse_iso_ms(&creds.access_token_expires_at) {
        Some(exp) if now <= exp => {
            println!(
                "Access Token: Valid ({} remaining)",
                format_remaining(exp - now)
            );
        }
        _ => println!("Access Token: Expired"),
    }

    // Refresh token.
    match parse_iso_ms(&creds.refresh_token_expires_at) {
        Some(exp) if now <= exp => {
            println!(
                "Refresh Token: Valid ({} remaining)",
                format_remaining(exp - now)
            );
        }
        _ => {
            println!("Refresh Token: Expired");
            eprintln!("\n⚠ Refresh token has expired. Run `monocle login --tenant <domain>` to re-authenticate.");
        }
    }

    // Claude Code configuration.
    let settings_path = home.join(".claude").join("settings.json");
    let claude_configured = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|s| {
            s.get("apiKeyHelper")?
                .as_str()
                .map(|h| store.accepts_token_helper(h))
        })
        .unwrap_or(false);

    println!(
        "Claude Code: {}",
        if claude_configured {
            "Configured"
        } else {
            "Not configured"
        }
    );
    true
}
