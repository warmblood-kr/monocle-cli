//! A bearer that expires mid-command is refreshed once and the rejected request
//! replayed (monocle-cli#184) — in `net::Client`, not per command.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine;
use monocle_cli::auth::ensure_fresh_token;
use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::net::Client;
use serde_json::json;

fn jwt(exp: i64) -> String {
    let payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
    format!("header.{payload}.signature")
}

struct Hits {
    refresh: AtomicUsize,
    api: AtomicUsize,
}

/// Serves discovery, `/oauth/token` (status `refresh_status`, issues `new`) and
/// `/api` (200 only for `Bearer <new>`, else 401). Returns addr + counters.
fn server(refresh_status: u16, new: String) -> (String, Arc<Hits>) {
    let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", srv.server_addr().to_ip().unwrap().port());
    let hits = Arc::new(Hits {
        refresh: AtomicUsize::new(0),
        api: AtomicUsize::new(0),
    });
    let (a, h) = (addr.clone(), hits.clone());
    std::thread::spawn(move || {
        for mut req in srv.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let auth = req
                .headers()
                .iter()
                .filter(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .collect::<Vec<_>>();
            let url = req.url().to_string();
            let (status, out) = if url.starts_with("/.well-known/") {
                (
                    200,
                    format!(
                        r#"{{"issuer":"http://{a}","authorization_endpoint":"http://{a}/auth","token_endpoint":"http://{a}/oauth/token"}}"#
                    ),
                )
            } else if url.starts_with("/oauth/token") {
                h.refresh.fetch_add(1, Ordering::SeqCst);
                if refresh_status == 200 {
                    (
                        200,
                        format!(r#"{{"access_token":"{new}","expires_in":3600}}"#),
                    )
                } else {
                    (refresh_status, r#"{"error":"invalid_grant"}"#.into())
                }
            } else {
                h.api.fetch_add(1, Ordering::SeqCst);
                // exactly one Authorization header, and it is the new token
                if auth == [format!("Bearer {new}")] {
                    (200, "{}".into())
                } else {
                    (401, "{}".into())
                }
            };
            let _ = req.respond(tiny_http::Response::from_string(out).with_status_code(status));
        }
    });
    (addr, hits)
}

fn setup(addr: &str) -> (tempfile::TempDir, Credentials, Client, String) {
    let old = jwt(4_102_444_800);
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials::with_home(dir.path());
    creds
        .write(&CredentialsData {
            tenant_domain: addr.into(),
            tenant_name: "Org".into(),
            email: "u@example.com".into(),
            access_token: old.clone(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00.000Z".into(),
            router_url: None,
        })
        .unwrap();
    let client = Client::new();
    let session = ensure_fresh_token(&client, &creds).unwrap();
    assert_eq!(session.token, old);
    (dir, creds, client, old)
}

#[test]
fn batch_survives_mid_batch_expiry_with_one_refresh() {
    let new = jwt(4_102_444_801);
    let (addr, hits) = server(200, new.clone());
    let (_dir, creds, client, old) = setup(&addr);
    let url = format!("http://{addr}/api");
    for _ in 0..4 {
        // every post still carries the stale bearer; only the first needs a refresh
        let bearer = format!("Bearer {old}");
        let r = client
            .post_json(&url, &[("Authorization", &bearer)], &json!({}))
            .unwrap();
        // later calls get 401 again with the stale token but reuse the stored one
        assert_eq!(r.status, 200);
    }
    assert_eq!(hits.refresh.load(Ordering::SeqCst), 1);
    assert_eq!(creds.read().unwrap().access_token, new);
}

#[test]
fn single_401_means_one_refresh_and_one_replay() {
    let new = jwt(4_102_444_801);
    let (addr, hits) = server(200, new);
    let (_dir, _creds, client, old) = setup(&addr);
    let bearer = format!("Bearer {old}");
    let r = client
        .post_json_once(
            &format!("http://{addr}/api"),
            &[("Authorization", &bearer)],
            &json!({}),
        )
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(hits.refresh.load(Ordering::SeqCst), 1);
    assert_eq!(hits.api.load(Ordering::SeqCst), 2);
}

#[test]
fn second_401_after_replay_is_returned_without_another_refresh() {
    // `addr` issues refreshed tokens; `addr2` (the API called) wants a different one.
    let (addr, hits) = server(200, jwt(4_102_444_801));
    let (_dir, _creds, client, old) = setup(&addr);
    let (addr2, hits2) = server(200, jwt(4_102_444_802));
    let r = client
        .get(
            &format!("http://{addr2}/api"),
            &[("Authorization", &format!("Bearer {old}"))],
        )
        .unwrap();
    assert_eq!(r.status, 401);
    assert_eq!(hits.refresh.load(Ordering::SeqCst), 1);
    assert_eq!(hits2.api.load(Ordering::SeqCst), 2, "exactly one replay");
}

#[test]
fn failed_refresh_returns_original_401_and_does_not_loop() {
    let (addr, hits) = server(400, jwt(4_102_444_801));
    let (_dir, _creds, client, old) = setup(&addr);
    let bearer = format!("Bearer {old}");
    for _ in 0..2 {
        let r = client
            .get(&format!("http://{addr}/api"), &[("Authorization", &bearer)])
            .unwrap();
        assert_eq!(r.status, 401);
    }
    assert_eq!(hits.refresh.load(Ordering::SeqCst), 1);
    assert_eq!(
        hits.api.load(Ordering::SeqCst),
        2,
        "no replay without a token"
    );
}

#[test]
fn once_variant_makes_one_transport_attempt_on_connection_drop() {
    let (addr, _hits) = server(200, jwt(4_102_444_801));
    let (_dir, _creds, client, old) = setup(&addr);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let drop_url = format!("http://{}/api", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let c = accepts.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            c.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    let bearer = format!("Bearer {old}");
    let r = client.post_json_once(&drop_url, &[("Authorization", &bearer)], &json!({}));
    assert!(r.is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}
