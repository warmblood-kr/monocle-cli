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
    // Authorization codes are single-use; a lost response must not replay one.
    let resp = match client.post_form_once(
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
    let issued_at = Instant::now();
    let device: DeviceAuthResponse = resp.json()?;
    validate_device_timing(device.interval.unwrap_or(5), device.expires_in)?;

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
    // Start the provider's lifetime before displaying the instructions.
    eprintln!("This device code expires in {} seconds. If it expires before approval, rerun `monocle login --device-code` with the same tenant/environment options.", device.expires_in);
    eprintln!("{}", c::dim("Waiting for authorization..."));

    let token = poll_for_token(
        client,
        &oidc.token_endpoint,
        &device.device_code,
        device.interval.unwrap_or(5),
        device.expires_in,
        issued_at,
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
    issued_at: Instant,
) -> Result<TokenResponse> {
    poll_with_clock(
        interval,
        expires_in,
        || issued_at.elapsed(),
        std::thread::sleep,
        || {
            let resp = client.post_form(
                token_endpoint,
                &[],
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_code),
                    ("client_id", CLIENT_ID),
                ],
            )?;

            device_poll_response(resp.status, resp.bytes())
        },
    )
}

fn device_poll_response(status: u16, body: impl AsRef<[u8]>) -> Result<PollReply> {
    let body = body.as_ref();
    if (200..300).contains(&status) {
        let token: TokenResponse = serde_json::from_slice(body)?;
        if token.access_token.as_deref().is_none_or(str::is_empty) {
            return Err(AppError::new("No access_token in device token response"));
        }
        return Ok(PollReply::Success(token));
    }

    let val = serde_json::from_slice::<Value>(body).ok();
    let error = val
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str);

    match error {
        Some("authorization_pending") => Ok(PollReply::Pending),
        Some("slow_down") => Ok(PollReply::SlowDown),
        Some("expired_token") => Err(device_expired()),
        Some("access_denied") => Err(AppError::new(
            "Authorization request was denied by the user.",
        )),
        Some(e) => {
            let desc = val
                .as_ref()
                .and_then(|v| v.get("error_description"))
                .and_then(Value::as_str)
                .unwrap_or("");
            Err(AppError::new(format!("Token polling failed: {e} - {desc}")))
        }
        None => {
            let raw = String::from_utf8_lossy(body);
            let snippet: String = raw.chars().take(200).collect();
            let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            let snippet = if snippet.is_empty() {
                "(empty)".to_string()
            } else {
                snippet
            };
            Err(AppError::new(format!(
            "Token polling failed: server returned HTTP {} with non-OAuth response. Body: {snippet}",
            status
        )))
        }
    }
}

fn device_expired() -> AppError {
    AppError::new("Device authorization code expired before token retrieval completed. Rerun `monocle login --device-code` with the same tenant/environment options. This is not a refresh-token failure.")
}

fn validate_device_timing(interval: i64, expires_in: i64) -> Result<()> {
    // Bounds avoid overflow/unbounded sleeps on malformed provider responses.
    if !(1..=86400).contains(&interval) || !(1..=86400).contains(&expires_in) {
        return Err(AppError::new("Invalid device authorization timing: interval and expires_in must be positive and at most 86400 seconds"));
    }
    Ok(())
}

enum PollReply {
    Pending,
    SlowDown,
    Success(TokenResponse),
}

// The same loop runs in production and with a synthetic monotonic clock in tests.
// It has no credentials/store argument: unsuccessful polling cannot persist tokens.
fn poll_with_clock(
    interval: i64,
    expires_in: i64,
    elapsed: impl Fn() -> Duration,
    mut sleep: impl FnMut(Duration),
    mut request: impl FnMut() -> Result<PollReply>,
) -> Result<TokenResponse> {
    validate_device_timing(interval, expires_in)?;
    let lifetime = Duration::from_secs(expires_in as u64);
    let mut interval = Duration::from_secs(interval as u64);
    loop {
        let remaining = lifetime.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Err(device_expired());
        }
        sleep(interval.min(remaining));
        // A sleep may overshoot. Never start an HTTP poll at/after the deadline.
        if elapsed() >= lifetime {
            return Err(device_expired());
        }
        match request()? {
            PollReply::Pending => {}
            PollReply::SlowDown => interval = interval.saturating_add(Duration::from_secs(5)),
            PollReply::Success(token) => return Ok(token),
        }
    }
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

#[cfg(test)]
mod device_deadline_tests {
    use super::*;
    use std::cell::Cell;

    fn success() -> PollReply {
        PollReply::Success(serde_json::from_str(r#"{"access_token":"synthetic"}"#).unwrap())
    }

    #[test]
    fn pending_then_success_uses_provider_interval() {
        let elapsed = Cell::new(Duration::ZERO);
        let mut calls = 0;
        let token = poll_with_clock(
            2,
            10,
            || elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
            || {
                calls += 1;
                Ok(if calls == 1 {
                    PollReply::Pending
                } else {
                    success()
                })
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(elapsed.get(), Duration::from_secs(4));
        assert_eq!(token.access_token.as_deref(), Some("synthetic"));
    }

    #[test]
    fn sleep_crossing_deadline_never_polls() {
        let elapsed = Cell::new(Duration::ZERO);
        let result = poll_with_clock(
            2,
            3,
            || elapsed.get(),
            |d| elapsed.set(elapsed.get() + d + Duration::from_secs(2)),
            || panic!("must not request after expiry"),
        );
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("not a refresh-token failure"));
    }

    #[test]
    fn slow_down_crossing_deadline_does_not_request_again() {
        let elapsed = Cell::new(Duration::ZERO);
        let mut calls = 0;
        let result = poll_with_clock(
            2,
            8,
            || elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
            || {
                calls += 1;
                Ok(PollReply::SlowDown)
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
        assert_eq!(elapsed.get(), Duration::from_secs(8));
    }

    #[test]
    fn server_expiry_stops_without_regenerating_grant() {
        let elapsed = Cell::new(Duration::ZERO);
        let mut calls = 0;
        let result = poll_with_clock(
            1,
            8,
            || elapsed.get(),
            |d| elapsed.set(elapsed.get() + d),
            || {
                calls += 1;
                device_poll_response(400, r#"{"error":"expired_token"}"#)
            },
        );
        assert_eq!(calls, 1);
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("same tenant/environment"));
    }

    #[test]
    fn invalid_timing_never_sleeps_or_requests() {
        for (interval, lifetime) in [
            (0, 3),
            (-1, 3),
            (2, 0),
            (2, -1),
            (i64::MAX, 3),
            (2, i64::MAX),
        ] {
            assert!(poll_with_clock(
                interval,
                lifetime,
                || Duration::ZERO,
                |_| panic!("invalid timing must not sleep"),
                || panic!("invalid timing must not poll")
            )
            .is_err());
        }
    }

    #[test]
    fn device_response_categories_and_missing_token_are_distinct() {
        assert!(matches!(
            device_poll_response(400, r#"{"error":"authorization_pending"}"#).unwrap(),
            PollReply::Pending
        ));
        assert!(matches!(
            device_poll_response(400, r#"{"error":"slow_down"}"#).unwrap(),
            PollReply::SlowDown
        ));
        assert!(device_poll_response(400, r#"{"error":"expired_token"}"#)
            .err()
            .unwrap()
            .to_string()
            .contains("not a refresh-token failure"));
        assert!(device_poll_response(400, r#"{"error":"access_denied"}"#)
            .err()
            .unwrap()
            .to_string()
            .contains("denied by the user"));
        for body in [r#"{}"#, r#"{"access_token":""}"#] {
            assert!(device_poll_response(200, body).is_err());
        }
        assert!(matches!(
            device_poll_response(200, r#"{"access_token":"synthetic"}"#).unwrap(),
            PollReply::Success(_)
        ));
    }

    #[test]
    fn device_expiry_diagnostic_does_not_infer_approval_timing() {
        let local = poll_with_clock(
            2,
            3,
            || Duration::from_secs(3),
            |_| panic!("expired grant must not sleep"),
            || panic!("expired grant must not poll"),
        )
        .err()
        .unwrap();
        let server = device_poll_response(400, r#"{"error":"expired_token"}"#)
            .err()
            .unwrap();
        for error in [local, server] {
            let message = error.to_string();
            assert!(message.contains("expired before token retrieval completed"));
            assert!(!message.contains("approval"));
            assert!(message.contains("same tenant/environment options"));
            assert!(message.contains("not a refresh-token failure"));
        }
    }

    #[test]
    fn already_expired_does_not_sleep_or_request() {
        assert!(poll_with_clock(
            2,
            3,
            || Duration::from_secs(3),
            |_| panic!("expired grant must not sleep"),
            || panic!("expired grant must not poll")
        )
        .is_err());
    }
}

#[cfg(test)]
mod browser_exchange_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::thread;

    // Each test runs in its own process with a temporary diagnostic HOME;
    // never mutate process-global HOME while other tests are running.
    fn isolated_parent(test: &str) -> bool {
        if std::env::var_os("MONOCLE_BROWSER_TEST_HOME")
            .is_some_and(|home| Some(home) == std::env::var_os("HOME"))
        {
            return false;
        }
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                &format!("commands::login::browser_exchange_tests::{test}"),
                "--exact",
                "--test-threads=1",
            ])
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("MONOCLE_BROWSER_TEST_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "synthetic child failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    // All values are synthetic. No browser, discovery, live account or home store.
    struct Outcome {
        browser: Browser,
        html: String,
        forms: Vec<String>,
        saved: bool,
        unchanged: bool,
    }

    fn exercise(
        state: &str,
        status: u16,
        body: &'static str,
        lose_response: bool,
        fail_save: bool,
    ) -> Outcome {
        let home = tempfile::tempdir().unwrap();
        let store = Credentials::with_home(home.path());
        if fail_save {
            std::fs::write(store.dir(), "synthetic obstruction").unwrap();
        } else {
            store
                .write(&build_creds(
                    Some("synthetic.invalid"),
                    None,
                    "previous-synthetic-access".into(),
                    None,
                    None,
                    None,
                ))
                .unwrap();
        }
        let prior_bytes = std::fs::read(store.path()).ok();
        let tokens = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/token", tokens.local_addr().unwrap());
        tokens.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_server = stop.clone();
        let provider = thread::spawn(move || {
            let mut forms = Vec::new();
            while !stop_server.load(Ordering::SeqCst) {
                let (mut stream, _) = match tokens.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("synthetic listener failed: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /token "));
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut form = vec![0; length];
                reader.read_exact(&mut form).unwrap();
                forms.push(String::from_utf8(form).unwrap());
                if lose_response && forms.len() == 1 {
                    // Code consumed; response lost/corrupted before valid HTTP headers.
                    // Invalid HTTP avoids timing-dependent empty-close behavior in hyper.
                    stream.write_all(b"not an http response\r\n\r\n").unwrap();
                } else {
                    let (status, body) = if lose_response {
                        (400, r#"{"error":"invalid_grant"}"#)
                    } else {
                        (status, body)
                    };
                    write!(stream, "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}", body.len()).unwrap();
                }
            }
            forms
        });
        let callback = Server::http("127.0.0.1:0").unwrap();
        let redirect = format!(
            "http://{}/oauth/oidc/callback",
            callback.server_addr().to_ip().unwrap()
        );
        let redirect_worker = redirect.clone();
        let worker_home = home.path().to_path_buf();
        let worker = thread::spawn(move || {
            let req = callback
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            handle_callback(
                &Client::new(),
                &Credentials::with_home(worker_home),
                Some("synthetic.invalid"),
                "synthetic-state",
                &OIDCConfig {
                    issuer: "synthetic".into(),
                    authorization_endpoint: "unused".into(),
                    token_endpoint: endpoint,
                    device_authorization_endpoint: None,
                    router_url: None,
                },
                &redirect_worker,
                "synthetic-verifier",
                req,
            )
            .unwrap()
        });
        let html = Client::new()
            .get(
                &format!("{redirect}?state={state}&code=synthetic-code"),
                &[],
            )
            .unwrap()
            .text();
        let browser = worker.join().unwrap();
        stop.store(true, Ordering::SeqCst);
        let forms = provider.join().unwrap();
        for form in &forms {
            let params = parse_query(form);
            let has = |name: &str, value: &str| params.iter().any(|(k, v)| k == name && v == value);
            assert!(has("grant_type", "authorization_code"));
            assert!(has("client_id", CLIENT_ID));
            assert!(has("code", "synthetic-code"));
            assert!(has("code_verifier", "synthetic-verifier"));
            assert!(has("redirect_uri", &redirect));
        }
        let saved = if fail_save {
            false
        } else {
            let data = store.read().unwrap();
            let saved = data.access_token == "synthetic-access";
            if saved {
                #[cfg(unix)]
                assert_eq!(store.file_mode(), Some(0o600));
            }
            saved
        };
        Outcome {
            browser,
            html,
            forms,
            saved,
            unchanged: prior_bytes == std::fs::read(store.path()).ok(),
        }
    }

    #[test]
    fn consumed_code_is_not_replayed_after_response_loss() {
        if isolated_parent("consumed_code_is_not_replayed_after_response_loss") {
            return;
        }
        let out = exercise("synthetic-state", 200, "unused", true, false);
        assert_eq!(out.forms.len(), 1, "single-use code must not be replayed");
        assert!(matches!(out.browser, Browser::Fatal(_)));
        assert!(!out.html.contains("invalid_grant"));
        assert!(!out.saved);
        assert!(out.unchanged);
    }

    #[test]
    fn valid_callback_exchanges_and_saves() {
        if isolated_parent("valid_callback_exchanges_and_saves") {
            return;
        }
        let out = exercise(
            "synthetic-state",
            200,
            r#"{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":3600}"#,
            false,
            false,
        );
        assert_eq!(out.forms.len(), 1);
        assert!(matches!(out.browser, Browser::Success));
        assert!(out.html.contains("Authentication Successful"));
        assert!(!out.html.contains("synthetic-access"));
        assert!(out.saved);
    }

    #[test]
    fn state_mismatch_never_exchanges_or_saves() {
        if isolated_parent("state_mismatch_never_exchanges_or_saves") {
            return;
        }
        let out = exercise("wrong", 200, "unused", false, false);
        assert!(out.forms.is_empty());
        assert!(matches!(out.browser, Browser::Fatal(_)));
        assert!(!out.saved);
        assert!(out.unchanged);
    }

    #[test]
    fn rejected_exchange_never_saves_or_retries() {
        if isolated_parent("rejected_exchange_never_saves_or_retries") {
            return;
        }
        let out = exercise(
            "synthetic-state",
            403,
            r#"{"error":"access_denied"}"#,
            false,
            false,
        );
        assert_eq!(out.forms.len(), 1);
        assert!(matches!(out.browser, Browser::Fatal(_)));
        assert!(out.html.contains("Token exchange failed (HTTP 403)"));
        assert!(!out.saved);
        assert!(out.unchanged);
    }

    #[test]
    fn malformed_or_missing_token_never_saves() {
        if isolated_parent("malformed_or_missing_token_never_saves") {
            return;
        }
        for body in ["not JSON", r#"{"refresh_token":"synthetic-refresh"}"#] {
            let out = exercise("synthetic-state", 200, body, false, false);
            assert_eq!(out.forms.len(), 1);
            assert!(matches!(out.browser, Browser::Fatal(_)));
            assert!(!out.saved);
            assert!(out.unchanged);
        }
    }

    #[test]
    fn persistence_failure_does_not_report_login_success() {
        if isolated_parent("persistence_failure_does_not_report_login_success") {
            return;
        }
        let out = exercise(
            "synthetic-state",
            200,
            r#"{"access_token":"synthetic-access"}"#,
            false,
            true,
        );
        assert_eq!(out.forms.len(), 1);
        assert!(matches!(out.browser, Browser::Fatal(_)));
        assert!(!out.html.contains("Authentication Successful"));
    }
}
