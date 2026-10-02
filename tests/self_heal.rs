mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;

use base64::Engine;
use common::stub;
use monocle_cli::auth::ensure_fresh_token;
use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::net::Client;

fn jwt(exp: i64) -> String {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!(r#"{{"exp":{exp}}}"#));
    format!("header.{payload}.signature")
}

fn initial_creds(tenant: &str, access_token: String) -> CredentialsData {
    CredentialsData {
        tenant_domain: tenant.into(),
        tenant_name: "Org".into(),
        email: "u@example.com".into(),
        access_token,
        refresh_token: "fake-refresh-secret".into(),
        id_token: "fake-id-token".into(),
        // Deliberately fresh to prove JWT exp takes precedence over this field.
        access_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
        refresh_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
        router_url: None,
    }
}

fn refresh_stub(status: u16, calls: Arc<AtomicUsize>) -> common::Stub {
    stub(move |addr, _method, url, _body| {
        if url.starts_with("/.well-known/openid-configuration") {
            (
                200,
                format!(
                    r#"{{"issuer":"http://{addr}","authorization_endpoint":"http://{addr}/auth","token_endpoint":"http://{addr}/oauth/token"}}"#
                ),
            )
        } else if url.starts_with("/oauth/token") {
            calls.fetch_add(1, Ordering::SeqCst);
            if status == 200 {
                (
                    status,
                    format!(
                        r#"{{"access_token":"{}","refresh_token":"fake-next-refresh","expires_in":3600}}"#,
                        jwt(4_102_444_800)
                    ),
                )
            } else {
                (status, r#"{"error":"fake-invalid-grant"}"#.into())
            }
        } else {
            (404, String::new())
        }
    })
}

#[test]
fn expired_jwt_refreshes_and_persists_credentials() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = refresh_stub(200, calls.clone());
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds
        .write(&initial_creds(&server.addr, jwt(1)))
        .unwrap();

    let session = ensure_fresh_token(&Client::new(), &creds).unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.token, creds.read().unwrap().access_token);
    assert_ne!(session.token, jwt(1));
}

#[test]
fn valid_jwt_does_not_refresh_even_when_stored_expiry_is_old() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = refresh_stub(200, calls.clone());
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    let mut data = initial_creds(&server.addr, jwt(4_102_444_800));
    data.access_token_expires_at = "2000-01-01T00:00:00.000Z".into();
    creds.write(&data).unwrap();

    ensure_fresh_token(&Client::new(), &creds).unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_refresh_errors_without_exposing_token_values() {
    for status in [400, 401] {
        let calls = Arc::new(AtomicUsize::new(0));
        let server = refresh_stub(status, calls.clone());
        let dir = tempfile::tempdir().unwrap();
        let creds = Credentials::with_home(dir.path());
        let original = initial_creds(&server.addr, jwt(1));
        creds.write(&original).unwrap();

        let err = ensure_fresh_token(&Client::new(), &creds)
            .unwrap_err()
            .to_string();

        assert!(err.contains("monocle login"), "got: {err}");
        for secret in [&original.access_token, &original.refresh_token, &original.id_token] {
            assert!(!err.contains(secret), "error leaked a credential: {err}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn parallel_expired_token_refreshes_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = refresh_stub(200, calls.clone());
    let dir = tempfile::tempdir().unwrap();
    let creds = Arc::new(Credentials::with_home(dir.path()));
    creds.write(&initial_creds(&server.addr, jwt(1))).unwrap();

    let workers: Vec<_> = (0..2)
        .map(|_| {
            let creds = creds.clone();
            thread::spawn(move || ensure_fresh_token(&Client::new(), &creds).unwrap())
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
