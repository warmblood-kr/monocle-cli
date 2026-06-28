mod common;

use common::stub;
use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::net::Client;
use monocle_cli::oidc::discover_oidc;
use monocle_cli::refresh::refresh_access_token;

fn creds(tenant: &str, router_url: Option<String>) -> CredentialsData {
    CredentialsData {
        tenant_domain: tenant.into(),
        tenant_name: "Org".into(),
        email: "u@e.com".into(),
        access_token: "at".into(),
        refresh_token: "old_rt".into(),
        id_token: "idt".into(),
        // Far future so get_access_token never tries to refresh.
        access_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
        refresh_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
        router_url,
    }
}

#[test]
fn discover_parses_document() {
    let s = stub(|_addr, _m, url, _b| {
        if url.starts_with("/.well-known/openid-configuration") {
            (
                200,
                r#"{"issuer":"https://t","authorization_endpoint":"https://t/auth","token_endpoint":"https://t/token","router_url":"https://api"}"#
                    .to_string(),
            )
        } else {
            (404, String::new())
        }
    });
    let cfg = discover_oidc(&Client::new(), &s.addr).unwrap();
    assert_eq!(cfg.issuer, "https://t");
    assert_eq!(cfg.authorization_endpoint, "https://t/auth");
    assert_eq!(cfg.token_endpoint, "https://t/token");
    assert_eq!(cfg.router_url.as_deref(), Some("https://api"));
}

#[test]
fn discover_handles_missing_router_url() {
    let s = stub(|_addr, _m, _url, _b| {
        (
            200,
            r#"{"issuer":"https://t","authorization_endpoint":"https://t/auth","token_endpoint":"https://t/token"}"#
                .to_string(),
        )
    });
    let cfg = discover_oidc(&Client::new(), &s.addr).unwrap();
    assert!(cfg.router_url.is_none());
}

#[test]
fn discover_errors_on_http_status() {
    let s = stub(|_addr, _m, _url, _b| (404, String::new()));
    let err = discover_oidc(&Client::new(), &s.addr)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("OIDC Discovery failed (HTTP 404)"),
        "got: {err}"
    );
}

#[test]
fn discover_errors_on_missing_fields() {
    let s = stub(|_addr, _m, _url, _b| (200, r#"{"issuer":"https://t"}"#.to_string()));
    let err = discover_oidc(&Client::new(), &s.addr)
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing required fields"), "got: {err}");
}

#[test]
fn refresh_writes_new_tokens() {
    let s = stub(|addr, _m, url, _b| {
        if url.starts_with("/.well-known/openid-configuration") {
            (
                200,
                format!(
                    r#"{{"issuer":"http://{addr}","authorization_endpoint":"http://{addr}/auth","token_endpoint":"http://{addr}/oauth/token","router_url":"http://{addr}"}}"#
                ),
            )
        } else if url.starts_with("/oauth/token") {
            (
                200,
                r#"{"access_token":"new_at","refresh_token":"new_rt","expires_in":3600}"#
                    .to_string(),
            )
        } else {
            (404, String::new())
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let store = Credentials::with_home(dir.path());
    let current = creds(&s.addr, None);

    let new = refresh_access_token(&Client::new(), &current, &store).unwrap();
    assert_eq!(new.access_token, "new_at");
    assert_eq!(new.refresh_token, "new_rt");
    assert_eq!(new.router_url, Some(s.router_url()));
    // Persisted to disk.
    assert_eq!(store.read().unwrap().access_token, "new_at");
}

#[test]
fn refresh_deletes_credentials_on_401() {
    let s = stub(|addr, _m, url, _b| {
        if url.starts_with("/.well-known/openid-configuration") {
            (
                200,
                format!(
                    r#"{{"issuer":"http://{addr}","authorization_endpoint":"http://{addr}/auth","token_endpoint":"http://{addr}/oauth/token"}}"#
                ),
            )
        } else {
            (401, r#"{"error":"invalid_grant"}"#.to_string())
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let store = Credentials::with_home(dir.path());
    let current = creds(&s.addr, None);
    store.write(&current).unwrap();

    let err = refresh_access_token(&Client::new(), &current, &store)
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid or expired"), "got: {err}");
    assert!(
        store.read().is_none(),
        "credentials should be deleted on 401"
    );
}
