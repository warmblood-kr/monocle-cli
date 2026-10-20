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

pub fn status_command(store: &Credentials, home: &Path, selection: &Selection, as_json: bool) {
    if as_json {
        return status_json(store, home, selection);
    }
    eprintln!("Profile: {}", selection.profile);
    eprintln!("Environment: {}", selection.environment);
    let mut creds = match store.read() {
        Some(c) => c,
        None => {
            eprintln!(
                "{}",
                crate::credentials::not_logged_in_message(
                    &selection.profile,
                    &selection.environment
                )
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

    let claude_configured = claude_configured(store, home);

    eprintln!(
        "Claude Code: {}",
        if claude_configured {
            "Configured"
        } else {
            "Not configured"
        }
    );
}

/// `status --json`: local fields only (mirrors `whoami --json`), never any token.
fn claude_configured(store: &Credentials, home: &Path) -> bool {
    let settings_path = home.join(".claude").join("settings.json");
    std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|c| serde_json::from_str::<Value>(&c).ok())
        .and_then(|s| {
            s.get("apiKeyHelper")?
                .as_str()
                .map(|h| store.accepts_token_helper(h))
        })
        .unwrap_or(false)
}

fn status_json(store: &Credentials, home: &Path, selection: &Selection) {
    let mut out = serde_json::json!({
        "logged_in": false,
        "profile": selection.profile,
        "environment": selection.environment,
    });
    if let Some(mut creds) = store.read() {
        if let Err(e) = ensure_fresh_token(&Client::new(), store) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        if let Some(refreshed) = store.read() {
            creds = refreshed;
        }
        let now = now_ms();
        let valid = |iso: &str| parse_iso_ms(iso).is_some_and(|exp| now <= exp);
        out["logged_in"] = true.into();
        out["tenant_domain"] = creds.tenant_domain.clone().into();
        out["tenant_name"] = creds.tenant_name.clone().into();
        out["email"] = creds.email.clone().into();
        out["access_token_valid"] = valid(&creds.access_token_expires_at).into();
        out["access_token_expires_at"] = creds.access_token_expires_at.clone().into();
        out["refresh_token_valid"] = valid(&creds.refresh_token_expires_at).into();
        out["refresh_token_expires_at"] = creds.refresh_token_expires_at.clone().into();
        out["claude_code_configured"] = claude_configured(store, home).into();
    } else {
        eprintln!(
            "{}",
            crate::credentials::not_logged_in_message(&selection.profile, &selection.environment)
        );
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}
