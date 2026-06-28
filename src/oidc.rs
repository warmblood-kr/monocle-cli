//! OIDC discovery, PKCE generation, and tenant→Stark domain resolution.

use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{AppError, Result};
use crate::net::Client;

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";

#[derive(Debug, Clone)]
pub struct OIDCConfig {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub device_authorization_endpoint: Option<String>,
    pub router_url: Option<String>,
}

/// PKCE `code_verifier` (43-128 chars, unreserved characters per RFC 7636).
pub fn generate_code_verifier(length: usize) -> Result<String> {
    if !(43..=128).contains(&length) {
        return Err(AppError::new(
            "code_verifier length must be between 43 and 128",
        ));
    }
    let mut bytes = vec![0u8; length];
    rand::thread_rng().fill_bytes(&mut bytes);
    let s: String = bytes
        .iter()
        .map(|b| UNRESERVED[(*b as usize) % UNRESERVED.len()] as char)
        .collect();
    Ok(s)
}

/// PKCE `code_challenge` using S256: BASE64URL(SHA256(code_verifier)).
pub fn generate_code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Random `state` for CSRF prevention (32 bytes, base64url).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Default Stark (control plane) domains by environment.
pub fn stark_domain_for_env(env: &str) -> &'static str {
    match env {
        "prod" => "monocle-ai.com",
        "stg" => "stg.monocle-ai.com",
        "local" => "localhost:9000",
        _ => "monocle-ai.com",
    }
}

/// Resolve the Stark domain from a tenant domain. Tenants always run on a
/// subdomain — bare domains are rejected.
///
///   stg-warmblood091803.monocle-ai.com → stg.monocle-ai.com
///   warmblood.monocle-ai.com           → monocle-ai.com
///   localhost:8080                      → localhost:8080
pub fn resolve_stark_domain(tenant_domain: &str) -> Result<String> {
    if tenant_domain.starts_with("localhost") || tenant_domain.starts_with("127.0.0.1") {
        return Ok(tenant_domain.to_string());
    }

    let parts: Vec<&str> = tenant_domain.split('.').collect();
    if parts.len() <= 2 {
        return Err(AppError::new(format!(
            "Invalid tenant domain: {tenant_domain}. Tenant must be a subdomain (e.g., mytenant.monocle-ai.com)"
        )));
    }

    let subdomain = parts[0];
    let base_domain = parts[1..].join(".");

    if subdomain.starts_with("stg-") {
        Ok(format!("stg.{base_domain}"))
    } else {
        Ok(base_domain)
    }
}

fn scheme_for(domain: &str) -> &'static str {
    if domain.starts_with("localhost") || domain.starts_with("127.0.0.1") {
        "http"
    } else {
        "https"
    }
}

#[derive(Deserialize)]
struct DiscoveryDoc {
    issuer: Option<String>,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    device_authorization_endpoint: Option<String>,
    router_url: Option<String>,
}

/// Discover OIDC endpoints from a Stark domain.
pub fn discover_oidc(client: &Client, domain: &str) -> Result<OIDCConfig> {
    let url = format!(
        "{}://{}/.well-known/openid-configuration",
        scheme_for(domain),
        domain
    );

    let resp = client.get(&url, &[]).map_err(|e| {
        AppError::new(format!(
            "Failed to connect to OIDC provider at {domain}: {e}"
        ))
    })?;

    if !resp.ok() {
        return Err(AppError::new(format!(
            "OIDC Discovery failed (HTTP {}) for {domain}",
            resp.status
        )));
    }

    let doc: DiscoveryDoc = resp.json()?;

    match (doc.authorization_endpoint, doc.token_endpoint, doc.issuer) {
        (Some(authorization_endpoint), Some(token_endpoint), Some(issuer)) => Ok(OIDCConfig {
            issuer,
            authorization_endpoint,
            token_endpoint,
            device_authorization_endpoint: doc.device_authorization_endpoint,
            router_url: doc.router_url,
        }),
        _ => Err(AppError::new(format!(
            "Invalid OIDC Discovery response from {domain}: missing required fields"
        ))),
    }
}
