use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tiny_http::{Header, Request, Response, Server};

use crate::colors as c;
use crate::credentials::{Credentials, CredentialsData};
use crate::error::{AppError, Result};
use crate::net::Client;
use crate::oidc::{
    discover_oidc, generate_code_challenge, generate_code_verifier, generate_state,
    resolve_stark_domain, stark_domain_for_env, OIDCConfig,
};
use crate::refresh::{decode_id_token_payload, TokenResponse};
use crate::util::{form_urlencode, now_ms, parse_query, to_iso};

const CLIENT_ID: &str = "monocle-cli";
const SCOPES: &str = "openid profile email";
const REFRESH_TOKEN_TTL_DAYS: i64 = 30;
const BROWSER_CALLBACK_TIMEOUT_MS: u64 = 90_000;

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Monocle - Authentication Successful</title>
<style>body{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f8f9fa}
.card{text-align:center;padding:2rem;border-radius:8px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}
h1{color:#22c55e;margin-bottom:0.5rem}p{color:#6b7280}</style></head>
<body><div class="card"><h1>Authentication Successful</h1><p>You can close this window and return to the terminal.</p></div></body></html>"#;

fn error_html(msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html><head><title>Monocle - Authentication Failed</title>
<style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f8f9fa}}
.card{{text-align:center;padding:2rem;border-radius:8px;background:white;box-shadow:0 2px 8px rgba(0,0,0,0.1)}}
h1{{color:#ef4444;margin-bottom:0.5rem}}p{{color:#6b7280}}</style></head>
<body><div class="card"><h1>✗ Authentication Failed</h1><p>{msg}</p></div></body></html>"#
    )
}

#[derive(Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: i64,
    interval: Option<i64>,
}

/// Outcome of the browser flow: success, "fall back to device code", or fatal.
enum Browser {
    Success,
    Unavailable(String),
    Fatal(AppError),
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Detect environments where a local browser almost certainly cannot be used:
/// SSH, Emacs inner shell, CI, and Linux console without DISPLAY.
fn detect_headless(device_code: bool) -> bool {
    if device_code {
        return true;
    }
    if env_truthy("SSH_CLIENT") || env_truthy("SSH_TTY") || env_truthy("SSH_CONNECTION") {
        return true;
    }
    if env_truthy("INSIDE_EMACS") || env_truthy("CI") {
        return true;
    }
    if cfg!(target_os = "linux") && !env_truthy("DISPLAY") && !env_truthy("WAYLAND_DISPLAY") {
        return true;
    }
    false
}

fn stark_domain(tenant: Option<&str>, env: &str) -> Result<String> {
    match tenant {
        Some(t) => resolve_stark_domain(t),
        None => Ok(stark_domain_for_env(env).to_string()),
    }
}

pub fn login_command(
    client: &Client,
    store: &Credentials,
    tenant: Option<String>,
    env: String,
    device_code: bool,
) -> Result<()> {
    let tenant_ref = tenant.as_deref();

    if detect_headless(device_code) {
        return device_code_login(client, store, tenant_ref, &env);
    }

    // Ambiguous environment: try browser flow, fall back to device code if the
    // browser can't launch or no callback arrives in time.
    match browser_code_login(client, store, tenant_ref, &env) {
        Browser::Success => Ok(()),
        Browser::Unavailable(reason) => {
            eprint!("\nBrowser flow unavailable: {reason}\nFalling back to device code...\n\n");
            device_code_login(client, store, tenant_ref, &env)
        }
        Browser::Fatal(e) => Err(e),
    }
}

fn discover(client: &Client, tenant: Option<&str>, env: &str) -> Result<OIDCConfig> {
    let domain = stark_domain(tenant, env)?;
    eprintln!(
        "{}",
        c::dim(&format!("Discovering OIDC configuration for {domain}..."))
    );
    discover_oidc(client, &domain)
}

fn browser_code_login(
    client: &Client,
    store: &Credentials,
    tenant: Option<&str>,
    env: &str,
) -> Browser {
    let oidc = match discover(client, tenant, env) {
        Ok(o) => o,
        Err(e) => return Browser::Fatal(e),
    };

    let code_verifier = match generate_code_verifier(64) {
        Ok(v) => v,
        Err(e) => return Browser::Fatal(e),
    };
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    let server = match Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => return Browser::Fatal(AppError::new(e.to_string())),
    };
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let redirect_uri = format!("http://127.0.0.1:{port}/oauth/oidc/callback");

    // Build the authorization URL.
    let mut params: Vec<(&str, &str)> = vec![
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("scope", SCOPES),
        ("redirect_uri", &redirect_uri),
        ("code_challenge", &code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
    ];
    if let Some(t) = tenant {
        params.push(("tenant", t));
    }
    let auth_url = format!(
        "{}?{}",
        oidc.authorization_endpoint,
        form_urlencode(&params)
    );

    eprintln!("Opening browser for authentication...");
    eprintln!("If the browser doesn't open, visit: {auth_url}");

    if let Err(e) = open::that(&auth_url) {
        return Browser::Unavailable(format!("failed to launch browser: {e}"));
    }

    // Wait for the callback, bounded by the overall timeout.
    let deadline = Instant::now() + Duration::from_millis(BROWSER_CALLBACK_TIMEOUT_MS);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Browser::Unavailable(format!(
                "no callback received within {}s",
                BROWSER_CALLBACK_TIMEOUT_MS / 1000
            ));
        }
        match server.recv_timeout(deadline - now) {
            Ok(Some(req)) => {
                if let Some(outcome) = handle_callback(
                    client,
                    store,
                    tenant,
                    &state,
                    &oidc,
                    &redirect_uri,
                    &code_verifier,
                    req,
                ) {
                    return outcome;
                }
                // Non-callback path → 404 already sent; keep waiting.
            }
            Ok(None) => {
                return Browser::Unavailable(format!(
                    "no callback received within {}s",
                    BROWSER_CALLBACK_TIMEOUT_MS / 1000
                ));
            }
            Err(e) => return Browser::Fatal(AppError::new(e.to_string())),
        }
    }
}

/// Handle one request. Returns `Some(outcome)` for the OAuth callback (terminal);
/// `None` for any other path (404 sent, keep listening).
#[allow(clippy::too_many_arguments)]
fn handle_callback(
    client: &Client,
    store: &Credentials,
    tenant: Option<&str>,
    state: &str,
    oidc: &OIDCConfig,
    redirect_uri: &str,
    code_verifier: &str,
    req: Request,
) -> Option<Browser> {
    let raw = req.url().to_string();
    let (path, query) = match raw.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw.as_str(), ""),
    };

    if path != "/oauth/oidc/callback" {
        let _ = req.respond(Response::from_string("Not found").with_status_code(404));
        return None;
    }

    let params = parse_query(query);
    let get = |k: &str| {
        params
            .iter()
            .find(|(kk, _)| kk == k)
            .map(|(_, v)| v.clone())
    };

    if let Some(err) = get("error") {
        let desc = get("error_description")
            .filter(|s| !s.is_empty())
            .unwrap_or(err);
        respond_html(req, &error_html(&desc));
        return Some(Browser::Fatal(AppError::new(format!(
            "Authorization failed: {desc}"
        ))));
    }

    if get("state").as_deref() != Some(state) {
        respond_html(req, &error_html("State mismatch - possible CSRF attack"));
        return Some(Browser::Fatal(AppError::new(
            "State mismatch - possible CSRF attack",
        )));
    }

    let code = match get("code").filter(|s| !s.is_empty()) {
        Some(code) => code,
        None => {
            respond_html(req, &error_html("No authorization code received"));
            return Some(Browser::Fatal(AppError::new(
                "No authorization code received",
            )));
        }
    };

    // Exchange code for tokens.
    let resp = match client.post_form(
        &oidc.token_endpoint,
        &[],
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", code_verifier),
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            respond_html(req, &error_html(&e.to_string()));
            return Some(Browser::Fatal(e));
        }
    };

    if !resp.ok() {
        let err_text = resp
            .json::<Value>()
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| "{}".to_string());
        let msg = format!("Token exchange failed (HTTP {}): {err_text}", resp.status);
        respond_html(req, &error_html(&msg));
        return Some(Browser::Fatal(AppError::new(msg)));
    }

    let token: TokenResponse = match resp.json() {
        Ok(t) => t,
        Err(e) => {
            respond_html(req, &error_html(&e.to_string()));
            return Some(Browser::Fatal(e));
        }
    };

    let access_token = match token.access_token.clone().filter(|s| !s.is_empty()) {
        Some(a) => a,
        None => {
            respond_html(req, &error_html("No access_token in token response"));
            return Some(Browser::Fatal(AppError::new(
                "No access_token in token response",
            )));
        }
    };

    let creds = build_creds(
        tenant,
        oidc.router_url.clone(),
        access_token,
        token.refresh_token,
        token.id_token,
        token.expires_in,
    );

    if let Err(e) = store.write(&creds) {
        respond_html(req, &error_html(&e.to_string()));
        return Some(Browser::Fatal(e));
    }

    respond_html(req, SUCCESS_HTML);
    print_login_success(&creds.email, &creds.tenant_name, false);
    Some(Browser::Success)
}

fn respond_html(req: Request, html: &str) {
    let header = Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..]).unwrap();
    let _ = req.respond(Response::from_string(html).with_header(header));
}

fn device_code_login(
    client: &Client,
    store: &Credentials,
    tenant: Option<&str>,
    env: &str,
) -> Result<()> {
    let oidc = discover(client, tenant, env)?;

    let device_endpoint = oidc.device_authorization_endpoint.clone().ok_or_else(|| {
        AppError::new(
            "This OIDC provider does not support the Device Authorization Grant. \
             Remove --device-code flag or use a browser-capable environment.",
        )
    })?;

    let resp = client.post_form(
        &device_endpoint,
        &[],
        &[("client_id", CLIENT_ID), ("scope", SCOPES)],
    )?;
    if !resp.ok() {
        return Err(AppError::new(format!(
            "Device authorization request failed (HTTP {})",
            resp.status
        )));
    }
    let device: DeviceAuthResponse = resp.json()?;

    eprintln!();
    eprintln!("{}", c::bold("To authenticate, visit:"));
    eprintln!();
    eprintln!("  {}", c::cyan(&device.verification_uri));
    eprintln!();
    eprintln!("  {} {}", c::dim("Code:"), c::bold(&device.user_code));
    eprintln!();
    if let Some(complete) = &device.verification_uri_complete {
        eprintln!("{}", c::dim("Or open this URL directly:"));
        eprintln!("  {}", c::cyan(complete));
        eprintln!();
    }
    eprintln!("{}", c::dim("Waiting for authorization..."));

    let token = poll_for_token(
        client,
        &oidc.token_endpoint,
        &device.device_code,
        device.interval.unwrap_or(5),
        device.expires_in,
    )?;

    let creds = build_creds(
        tenant,
        oidc.router_url.clone(),
        token.access_token.unwrap_or_default(),
        token.refresh_token,
        token.id_token,
        token.expires_in,
    );
    store.write(&creds)?;
    print_login_success(&creds.email, &creds.tenant_name, true);
    Ok(())
}

fn poll_for_token(
    client: &Client,
    token_endpoint: &str,
    device_code: &str,
    interval: i64,
    expires_in: i64,
) -> Result<TokenResponse> {
    let deadline = now_ms() + expires_in * 1000;
    let mut current_interval = interval;

    while now_ms() < deadline {
        std::thread::sleep(Duration::from_secs(current_interval.max(0) as u64));

        let resp = client.post_form(
            token_endpoint,
            &[],
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", CLIENT_ID),
            ],
        )?;

        if resp.ok() {
            return resp.json();
        }

        let val = resp.json::<Value>().ok();
        let error = val
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str);

        match error {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                current_interval += 5;
                continue;
            }
            Some("expired_token") => {
                return Err(AppError::new(
                    "Device code expired. Please run login again.",
                ))
            }
            Some("access_denied") => {
                return Err(AppError::new(
                    "Authorization request was denied by the user.",
                ))
            }
            Some(e) => {
                let desc = val
                    .as_ref()
                    .and_then(|v| v.get("error_description"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(AppError::new(format!("Token polling failed: {e} - {desc}")));
            }
            None => {
                let raw = resp.text();
                let snippet: String = raw.chars().take(200).collect();
                let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
                let snippet = if snippet.is_empty() {
                    "(empty)".to_string()
                } else {
                    snippet
                };
                return Err(AppError::new(format!(
                    "Token polling failed: server returned HTTP {} with non-OAuth response. Body: {snippet}",
                    resp.status
                )));
            }
        }
    }

    Err(AppError::new(
        "Device code expired. Please run login again.",
    ))
}

fn build_creds(
    tenant: Option<&str>,
    router_url: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
) -> CredentialsData {
    let mut email = "unknown".to_string();
    let mut tenant_domain = tenant.unwrap_or("").to_string();
    let mut tenant_name = tenant_domain.clone();

    if let Some(idt) = &id_token {
        if let Ok(payload) = decode_id_token_payload(idt) {
            email = payload
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            tenant_domain = payload
                .get("tenant_domain")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or(tenant_domain);
            tenant_name = payload
                .get("tenant_name")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| tenant_domain.clone());
        }
    }

    let now = now_ms();
    CredentialsData {
        tenant_domain,
        tenant_name,
        email,
        access_token,
        refresh_token: refresh_token.unwrap_or_default(),
        id_token: id_token.unwrap_or_default(),
        access_token_expires_at: to_iso(now + expires_in.unwrap_or(3600) * 1000),
        refresh_token_expires_at: to_iso(now + REFRESH_TOKEN_TTL_DAYS * 24 * 60 * 60 * 1000),
        router_url,
    }
}

fn print_login_success(email: &str, tenant_name: &str, leading_newline: bool) {
    eprintln!(
        "{}{} Logged in as {} {}",
        if leading_newline { "\n" } else { "" },
        c::green("✓"),
        c::bold(email),
        c::dim(&format!("({tenant_name})"))
    );
    eprintln!(
        "\n{} {}",
        c::dim("Launch Claude Code through Monocle with:"),
        c::bold("monocle claude")
    );
    eprintln!(
        "{} {} {} {}{}",
        c::dim("(To route plain"),
        c::bold("claude"),
        c::dim("globally through Monocle, run"),
        c::bold("monocle setup"),
        c::dim(".)")
    );
}
