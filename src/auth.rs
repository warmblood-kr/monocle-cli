//! Resolve a usable access token + router URL, refreshing if needed.
//!
//! Like the TS version, this exits the process with a friendly message when no
//! credentials exist or refresh fails — callers don't handle those cases.

use crate::credentials::{Credentials, CredentialsData};
use crate::error::{AppError, Result};
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

/// Non-exiting variant of [`get_access_token`]. Returns an [`AppError`] instead
/// of printing to stderr and calling `std::process::exit(1)`, so long-lived
/// callers (e.g. the ACP server) can fail a single request and stay alive.
pub fn try_access_token(client: &Client, creds: &Credentials) -> Result<AuthSession> {
    let stored = match creds.read() {
        Some(c) => c,
        None => {
            return Err(AppError::new(
                "Not logged in. Run `monocle login --tenant <domain>` first.",
            ));
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
                    return Err(AppError::new(format!("Token refresh failed: {e}")));
                }
            }
        }
    }

    let router_url = router_url_for(&active);
    Ok(AuthSession {
        token: active.access_token,
        router_url,
    })
}

pub fn get_access_token(client: &Client, creds: &Credentials) -> AuthSession {
    match try_access_token(client, creds) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_access_token_errors_when_not_logged_in() {
        // Point at an empty temp home so no real `~/.monocle` is consulted and
        // the missing-credentials branch fires without any network access.
        let dir = tempfile::tempdir().unwrap();
        let creds = Credentials::with_home(dir.path());

        match try_access_token(&Client::new(), &creds) {
            Ok(_) => panic!("absent credentials must return Err, not Ok"),
            Err(err) => assert!(
                err.to_string().starts_with("Not logged in"),
                "unexpected message: {err}"
            ),
        }
    }
}
