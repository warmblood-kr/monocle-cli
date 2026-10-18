//! Resolve a usable access token + router URL, refreshing if needed.
//!
//! Like the TS version, this exits the process with a friendly message when no
//! credentials exist or refresh fails — callers don't handle those cases.

use crate::credentials::{Credentials, CredentialsData};
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::refresh::{decode_id_token_payload, refresh_access_token};
use crate::util::{now_ms, parse_iso_ms};

const EXPIRY_BUFFER_MS: i64 = 120 * 1000;

pub struct AuthSession {
    pub token: String,
    pub router_url: String,
}

/// `http` for a local dev host (`localhost`/`127.0.0.1`), else `https`.
fn scheme_for_host(host: &str) -> &'static str {
    let is_local = host.starts_with("localhost") || host.starts_with("127.0.0.1");
    if is_local {
        "http"
    } else {
        "https"
    }
}

pub fn router_url_for(creds: &CredentialsData) -> String {
    if let Some(url) = &creds.router_url {
        return url.clone();
    }
    format!(
        "{}://{}",
        scheme_for_host(&creds.tenant_domain),
        creds.tenant_domain
    )
}

/// jarvice's own base URL, always derived from `tenant_domain` — jarvice is
/// tenant-host-routed (unlike chat-proxy behind `router_url`), so it is
/// reachable only at its per-tenant hostname, never through `router_url`.
/// Deliberately ignores `creds.router_url` (that's chat-proxy's address, a
/// different host — conflating the two has been a recurring trap here).
pub fn jarvice_url_for(creds: &CredentialsData) -> String {
    format!(
        "{}://{}",
        scheme_for_host(&creds.tenant_domain),
        creds.tenant_domain
    )
}

/// Return a usable access token, serializing refreshes across processes.
/// The lock is held from the post-lock read through any refresh and write.
pub fn ensure_fresh_token(client: &Client, creds: &Credentials) -> Result<AuthSession> {
    let dir = creds.dir();
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".refresh.lock"))?;
    lock.lock()?;

    let stored = match creds.read() {
        Some(c) => c,
        None => {
            return Err(AppError::new(
                "Not logged in. Run `monocle login --tenant <domain>` first.",
            ));
        }
    };

    let mut active = stored.clone();
    let expires_at = access_expiry_ms(&stored);
    if expires_at.is_some_and(|exp| now_ms() + EXPIRY_BUFFER_MS >= exp) {
        match refresh_access_token(client, &stored, creds) {
            Ok(refreshed) => active = refreshed,
            // refresh.rs's 400/401 message already says to run `monocle login`;
            // transient errors (network, 5xx) keep their own text.
            Err(e) => return Err(AppError::new(format!("Token refresh failed: {e}"))),
        }
    }

    let router_url = router_url_for(&active);
    Ok(AuthSession {
        token: active.access_token,
        router_url,
    })
}

/// Shared by refresh policy and diagnostics: decoded JWT `exp` takes precedence;
/// a decoded JWT without `exp` has unknown expiry, never a fabricated ISO fallback.
pub fn access_expiry_ms(creds: &CredentialsData) -> Option<i64> {
    match jwt_exp_ms(&creds.access_token) {
        Some(exp) => exp,
        None => parse_iso_ms(&creds.access_token_expires_at),
    }
}

/// `Some(None)` means a JWT payload was decoded but has no usable `exp`; `None`
/// means the access token is not a parseable JWT and the ISO field may be used.
fn jwt_exp_ms(token: &str) -> Option<Option<i64>> {
    let payload = decode_id_token_payload(token).ok()?;
    let exp = payload.get("exp").and_then(serde_json::Value::as_i64);
    Some(exp.and_then(|seconds| seconds.checked_mul(1000)))
}

/// Non-exiting variant of [`get_access_token`]. Returns an [`AppError`] instead
/// of printing to stderr and calling `std::process::exit(1)`, so long-lived
/// callers (e.g. the ACP server) can fail a single request and stay alive.
pub fn try_access_token(client: &Client, creds: &Credentials) -> Result<AuthSession> {
    ensure_fresh_token(client, creds)
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
    fn diagnostic_expiry_uses_auth_policy() {
        use base64::Engine;
        let mut creds: CredentialsData = serde_json::from_value(serde_json::json!({
            "tenant_domain":"synthetic", "tenant_name":"synthetic", "email":"synthetic",
            "access_token":"opaque", "refresh_token":"synthetic", "id_token":"synthetic",
            "access_token_expires_at":"2000-01-01T00:00:00.000Z",
            "refresh_token_expires_at":"2099-01-01T00:00:00.000Z"
        }))
        .unwrap();
        assert_eq!(
            access_expiry_ms(&creds),
            parse_iso_ms(&creds.access_token_expires_at)
        );
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":123}"#);
        creds.access_token = format!("x.{payload}.x");
        assert_eq!(access_expiry_ms(&creds), Some(123000));
        creds.access_token = "x.e30.x".into();
        assert_eq!(access_expiry_ms(&creds), None);
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
