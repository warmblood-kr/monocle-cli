//! Access-token refresh via the `refresh_token` grant, plus ID-token decoding.

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

use crate::credentials::{Credentials, CredentialsData};
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::oidc::{discover_oidc, resolve_stark_domain};
use crate::util::{now_ms, to_iso};

const CLIENT_ID: &str = "monocle-cli";
const REFRESH_TOKEN_TTL_DAYS: i64 = 30;

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: Option<i64>,
    #[allow(dead_code)]
    pub token_type: Option<String>,
}

/// Refresh the access token. On success returns the freshly-written credentials;
/// on a 400/401 it deletes the stored credentials (refresh token dead) and
/// returns a re-login message. The error string is what callers print verbatim.
pub fn refresh_access_token(
    client: &Client,
    current: &CredentialsData,
    creds: &Credentials,
) -> Result<CredentialsData> {
    // Discover token endpoint and router URL.
    let (token_endpoint, discovered_router_url) = (|| -> Result<(String, Option<String>)> {
        let stark_domain = resolve_stark_domain(&current.tenant_domain)?;
        let oidc = discover_oidc(client, &stark_domain)?;
        Ok((oidc.token_endpoint, oidc.router_url))
    })()
    .map_err(|e| AppError::new(format!("OIDC Discovery failed: {e}")))?;

    // Request new tokens.
    let resp = client
        .post_form(
            &token_endpoint,
            &[],
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &current.refresh_token),
                ("client_id", CLIENT_ID),
            ],
        )
        .map_err(|e| AppError::new(format!("Token refresh request failed: {e}")))?;

    if !resp.ok() {
        if resp.status == 400 || resp.status == 401 {
            creds.delete();
            return Err(AppError::new(
                "Refresh token is invalid or expired. Please run `monocle login --tenant <domain>` to re-authenticate.",
            ));
        }
        return Err(AppError::new(format!(
            "Token refresh failed (HTTP {})",
            resp.status
        )));
    }

    let token: TokenResponse = resp.json()?;

    let now = now_ms();
    let access_expires = to_iso(now + token.expires_in.unwrap_or(3600) * 1000);
    let refresh_expires = to_iso(now + REFRESH_TOKEN_TTL_DAYS * 24 * 60 * 60 * 1000);

    // Decode ID token to refresh email / tenant_name when a new one is provided.
    let mut email = current.email.clone();
    let mut tenant_name = current.tenant_name.clone();
    if let Some(id_token) = &token.id_token {
        if let Ok(payload) = decode_id_token_payload(id_token) {
            if let Some(v) = payload.get("email").and_then(Value::as_str) {
                email = v.to_string();
            }
            if let Some(v) = payload.get("tenant_name").and_then(Value::as_str) {
                tenant_name = v.to_string();
            }
        }
    }

    let new_creds = CredentialsData {
        tenant_domain: current.tenant_domain.clone(),
        tenant_name,
        email,
        access_token: token.access_token.unwrap_or_default(),
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| current.refresh_token.clone()),
        id_token: token.id_token.unwrap_or_else(|| current.id_token.clone()),
        access_token_expires_at: access_expires,
        refresh_token_expires_at: refresh_expires,
        router_url: discovered_router_url.or_else(|| current.router_url.clone()),
    };

    creds.write(&new_creds)?;
    Ok(new_creds)
}

/// Decode the payload (middle segment) of a JWT into JSON.
pub fn decode_id_token_payload(id_token: &str) -> Result<Value> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::new("Invalid ID token format"));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| AppError::new(e.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes)?;
    Ok(value)
}
