//! Resolve a usable access token + router URL, refreshing if needed.
//!
//! Like the TS version, this exits the process with a friendly message when no
//! credentials exist or refresh fails — callers don't handle those cases.

use crate::credentials::{Credentials, CredentialsData};
use crate::net::Client;
use crate::refresh::refresh_access_token;
use crate::util::{now_ms, parse_iso_ms};

const EXPIRY_BUFFER_MS: i64 = 5 * 60 * 1000;

pub struct AuthSession {
    pub token: String,
    pub router_url: String,
}

pub fn router_url_for(creds: &CredentialsData) -> String {
    if let Some(url) = &creds.router_url {
        return url.clone();
    }
    let is_local = creds.tenant_domain.starts_with("localhost")
        || creds.tenant_domain.starts_with("127.0.0.1");
    let scheme = if is_local { "http" } else { "https" };
    format!("{scheme}://{}", creds.tenant_domain)
}

pub fn get_access_token(client: &Client, creds: &Credentials) -> AuthSession {
    let stored = match creds.read() {
        Some(c) => c,
        None => {
            eprintln!("Not logged in. Run `monocle login --tenant <domain>` first.");
            std::process::exit(1);
        }
    };

    // Refresh only when we can parse the expiry AND it is within the buffer.
    // (JS compares against NaN as `false`, i.e. unparseable expiry → no refresh.)
    let mut active = stored.clone();
    if let Some(expires_at) = parse_iso_ms(&stored.access_token_expires_at) {
        if now_ms() + EXPIRY_BUFFER_MS > expires_at {
            match refresh_access_token(client, &stored, creds) {
                Ok(refreshed) => active = refreshed,
                Err(e) => {
                    eprintln!("Token refresh failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    let router_url = router_url_for(&active);
    AuthSession {
        token: active.access_token,
        router_url,
    }
}
