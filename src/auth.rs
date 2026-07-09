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
    pub jarvice_url: String,
}

/// `http` for local dev hosts (`localhost`/`127.0.0.1`), `https` otherwise —
/// shared by [`router_url_for`]'s legacy fallback and [`jarvice_url_for`], both
/// of which derive a base URL from the same `tenant_domain`.
fn scheme_for_tenant_domain(tenant_domain: &str) -> &'static str {
    let is_local = tenant_domain.starts_with("localhost") || tenant_domain.starts_with("127.0.0.1");
    if is_local {
        "http"
    } else {
        "https"
    }
}

/// chat-proxy's base URL: `creds.router_url` when present, else (legacy
/// credentials predating that field) `{scheme}://{tenant_domain}`.
pub fn router_url_for(creds: &CredentialsData) -> String {
    if let Some(url) = &creds.router_url {
        return url.clone();
    }
    let scheme = scheme_for_tenant_domain(&creds.tenant_domain);
    format!("{scheme}://{}", creds.tenant_domain)
}

/// jarvice's base URL, always derived from `tenant_domain` — jarvice is a
/// different host from chat-proxy, so `creds.router_url` (chat-proxy-specific)
/// must NOT be used here even when present.
pub fn jarvice_url_for(creds: &CredentialsData) -> String {
    let scheme = scheme_for_tenant_domain(&creds.tenant_domain);
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
    let jarvice_url = jarvice_url_for(&active);
    Ok(AuthSession {
        token: active.access_token,
        router_url,
        jarvice_url,
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

    fn fixture_creds(tenant_domain: &str, router_url: Option<&str>) -> CredentialsData {
        CredentialsData {
            tenant_domain: tenant_domain.to_string(),
            tenant_name: "acme".to_string(),
            email: "user@example.com".to_string(),
            access_token: "at".to_string(),
            refresh_token: "rt".to_string(),
            id_token: "it".to_string(),
            access_token_expires_at: "2099-01-01T00:00:00.000Z".to_string(),
            refresh_token_expires_at: "2099-01-01T00:00:00.000Z".to_string(),
            router_url: router_url.map(str::to_string),
        }
    }

    #[test]
    fn jarvice_url_for_uses_https_for_a_real_domain() {
        let creds = fixture_creds("stg-agent.monocle-ai.com", None);
        assert_eq!(jarvice_url_for(&creds), "https://stg-agent.monocle-ai.com");
    }

    #[test]
    fn jarvice_url_for_uses_http_for_localhost() {
        let creds = fixture_creds("localhost:8080", None);
        assert_eq!(jarvice_url_for(&creds), "http://localhost:8080");
    }

    #[test]
    fn jarvice_url_for_uses_http_for_127_0_0_1() {
        let creds = fixture_creds("127.0.0.1:8080", None);
        assert_eq!(jarvice_url_for(&creds), "http://127.0.0.1:8080");
    }

    #[test]
    fn jarvice_url_for_ignores_router_url_even_when_present() {
        // The bug this function fixes: chat-proxy's `router_url` is NOT
        // jarvice's base URL, so it must never be consulted here — jarvice is
        // always resolved straight from `tenant_domain`.
        let creds = fixture_creds(
            "stg-agent.monocle-ai.com",
            Some("https://api-stg.monocle-ai.com"),
        );
        assert_eq!(jarvice_url_for(&creds), "https://stg-agent.monocle-ai.com");
    }

    #[test]
    fn router_url_for_prefers_stored_router_url_when_present() {
        let creds = fixture_creds(
            "stg-agent.monocle-ai.com",
            Some("https://api-stg.monocle-ai.com"),
        );
        assert_eq!(router_url_for(&creds), "https://api-stg.monocle-ai.com");
    }

    #[test]
    fn router_url_for_falls_back_to_tenant_domain_for_legacy_credentials() {
        let creds = fixture_creds("stg-agent.monocle-ai.com", None);
        assert_eq!(router_url_for(&creds), "https://stg-agent.monocle-ai.com");
    }

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
