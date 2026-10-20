//! `monocle whoami [--server] [--json]` — who the CLI thinks you are.
//!
//! Default is purely local: reads the stored credentials, never refreshes and
//! makes no request. Token strings are never printed — only id_token claims
//! (`sub`/`iss`/`aud`/`email`, when present) and expiry times.
//!
//! `--server` also asks jarvice via `GET /api/v1/auths/`. (`/api/v1/usage/me`
//! is non-minting but returns usage, not identity.) That endpoint mints a
//! server-side session token on every call; [`ServerMe`] has no token field, so
//! the token and any Set-Cookie are dropped unread.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{get_access_token, jarvice_url_for};
use crate::commands::status::format_remaining;
use crate::credentials::Credentials;
use crate::error::{AppError, Result};
use crate::jarvice_knowledge::api_error;
use crate::net::Client;
use crate::origin::auth_headers;
use crate::profiles::Selection;
use crate::refresh::decode_id_token_payload;
use crate::util::{now_ms, parse_iso_ms};

/// The only fields we keep from `/api/v1/auths/` (no `token`).
#[derive(Deserialize)]
struct ServerMe {
    #[serde(default)]
    id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    permissions: Value,
}

fn parse_me(body: &[u8]) -> Result<ServerMe> {
    serde_json::from_slice(body)
        .map_err(|_| AppError::new("Server identity lookup returned an unexpected response."))
}

fn expiry(iso: &str, now: i64) -> Value {
    match parse_iso_ms(iso) {
        Some(exp) if now <= exp => json!({
            "expires_at": iso, "valid": true,
            "remaining": format_remaining(exp - now),
        }),
        _ => json!({"expires_at": iso, "valid": false, "remaining": null}),
    }
}

fn server_identity(client: &Client, creds: &Credentials, local_email: &str) -> Result<Value> {
    let stored = creds
        .read()
        .ok_or_else(|| AppError::new(creds.not_logged_in()))?;
    let bearer = format!("Bearer {}", get_access_token(client, creds).token);
    let url = format!("{}/api/v1/auths/", jarvice_url_for(&stored));
    let resp = client.get(&url, &auth_headers(&bearer))?;
    if !resp.ok() {
        let hint = if matches!(resp.status, 401 | 403) {
            " The tenant may not accept this login token, or the token lacks an email claim (a known staging issue)."
        } else {
            ""
        };
        let e = api_error("Server identity lookup", &resp);
        return Err(AppError::new(format!("{e}{hint}")));
    }
    // The body carries a freshly minted session token and the response a
    // Set-Cookie with it. Parse straight into `ServerMe` (no token field), drop
    // the whole `Resp` before anything else runs, and report a parse failure
    // with a fixed message: serde's error text can quote body content.
    let me = parse_me(resp.bytes());
    drop(resp);
    let me = me?;
    Ok(json!({
        "id": me.id, "email": me.email, "name": me.name, "role": me.role,
        "permission_groups": me.permissions.as_object().map_or(0, |o| o.len()),
        "email_matches_local": me.email.eq_ignore_ascii_case(local_email),
    }))
}

pub fn whoami_command(
    client: &Client,
    creds: &Credentials,
    selection: &Selection,
    server: bool,
    as_json: bool,
) -> Result<()> {
    let c = creds.read().ok_or_else(|| {
        AppError::new(crate::credentials::not_logged_in_message(
            &selection.profile,
            &selection.environment,
        ))
    })?;
    let now = now_ms();
    let payload = decode_id_token_payload(&c.id_token).ok();
    let mut claims = serde_json::Map::new();
    for k in ["sub", "iss", "aud", "email"] {
        if let Some(v) = payload.as_ref().and_then(|p| p.get(k)) {
            claims.insert(k.into(), v.clone());
        }
    }
    let mut out = json!({
        "profile": selection.profile,
        "environment": selection.environment,
        "tenant": {"domain": c.tenant_domain, "name": c.tenant_name},
        "email": c.email,
        "id_token_claims": claims,
        "access_token": expiry(&c.access_token_expires_at, now),
        "refresh_token": expiry(&c.refresh_token_expires_at, now),
    });
    if server {
        out["server"] = server_identity(client, creds, &c.email)?;
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("Profile: {}", selection.profile);
    println!("Environment: {}", selection.environment);
    println!("Tenant: {} ({})", c.tenant_domain, c.tenant_name);
    println!("Email: {}", c.email);
    for (k, v) in &claims {
        println!(
            "id_token {k}: {}",
            v.as_str().map_or(v.to_string(), str::to_string)
        );
    }
    if payload.is_none() {
        println!("id_token: unreadable");
    }
    for (label, key) in [
        ("Access Token", "access_token"),
        ("Refresh Token", "refresh_token"),
    ] {
        match out[key]["remaining"].as_str() {
            Some(r) => println!("{label}: valid ({r} remaining)"),
            None => println!("{label}: expired"),
        }
    }
    if let Some(s) = out.get("server") {
        println!(
            "Server: id {} email {} name {} role {} ({} permission groups)",
            s["id"].as_str().unwrap_or(""),
            s["email"].as_str().unwrap_or(""),
            s["name"].as_str().unwrap_or(""),
            s["role"].as_str().unwrap_or(""),
            s["permission_groups"]
        );
        let m = s["email_matches_local"].as_bool().unwrap_or(false);
        println!(
            "Server email matches local: {}",
            if m { "yes" } else { "NO" }
        );
    }
    Ok(())
}
