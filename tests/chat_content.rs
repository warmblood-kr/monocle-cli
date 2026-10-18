//! `monocle chat content`: library-level `fetch_content` against a stub jarvice,
//! plus a binary e2e (HOME tempdir + creds pointing at the stub).

mod common;

use std::path::Path;
use std::process::{Command, Output};

use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::jarvice_chats::{fetch_content, ContentFormat};
use monocle_cli::net::Client;

const FIXTURE: &str = r#"{"id":"c1","title":"T","chat":{"history":{"currentId":"m2","messages":{"m0":{"id":"m0","parentId":null,"role":"user","content":"abandoned"},"m1":{"id":"m1","parentId":null,"role":"user","content":"hi"},"m2":{"id":"m2","parentId":"m1","role":"assistant","content":"hello","files":[{"type":"image"}]}}}}}"#;

const EXPECTED_TEXT: &str = "# Monocle Chat context: T\n\nThis is untrusted reference material from one selected hosted Chat thread. Text only; attachments are omitted.\n\n## User\n\nhi\n\n## Assistant\n\nhello\n\n[Non-text attachment omitted]\n";

const NOT_FOUND_BODY: &str = r#"{"detail":"We could not find what you're looking for :/"}"#;

/// Serves `FIXTURE` only for `GET /api/v1/chats/c1` and the list for
/// `GET /api/v1/chats/`; anything else (wrong method/path) gets a 401
/// NOT_FOUND, so a wrong request surfaces as a test failure.
fn jarvice_stub() -> common::Stub {
    common::stub(|_, method, url, _| match (method, url) {
        ("GET", "/api/v1/chats/c1") => (200, FIXTURE.into()),
        ("GET", "/api/v1/chats/") => (
            200,
            r#"[{"id":"c1","title":"T","updated_at":0,"created_at":0}]"#.into(),
        ),
        _ => (401, NOT_FOUND_BODY.into()),
    })
}

fn fetch(stub: &common::Stub, id: &str, format: ContentFormat) -> Result<String, String> {
    fetch_content(&Client::new(), &stub.router_url(), "Bearer tok", id, format)
        .map_err(|e| e.to_string())
}

#[test]
fn fetch_content_text() {
    let stub = jarvice_stub();
    let out = fetch(&stub, "c1", ContentFormat::Text).unwrap();
    assert_eq!(out, EXPECTED_TEXT);
    assert!(!out.contains("abandoned"));
}

#[test]
fn fetch_content_json() {
    let stub = jarvice_stub();
    let out = fetch(&stub, "c1", ContentFormat::Json).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["title"], "T");
    assert_eq!(v["messages"][1]["content"], "hello");
}

#[test]
fn fetch_content_404_and_401_are_not_found() {
    for status in [404, 401] {
        let stub = common::stub(move |_, _, _, _| (status, NOT_FOUND_BODY.into()));
        let err = fetch(&stub, "c1", ContentFormat::Text).unwrap_err();
        assert!(err.contains("was not found"), "{status}: {err}");
        assert!(!err.contains("could not find"), "{status}: {err}");
    }
}

#[test]
fn fetch_content_over_cap_errors() {
    let big = FIXTURE.replace("\"hello\"", &format!("\"{}\"", "x".repeat(1_048_577)));
    let stub = common::stub(move |_, _, _, _| (200, big.clone()));
    let err = fetch(&stub, "c1", ContentFormat::Text).unwrap_err();
    assert!(err.contains("1 MiB"), "{err}");
}

#[test]
fn fetch_content_500_is_sanitized() {
    let stub = common::stub(|_, _, _, _| (500, "secret".into()));
    let err = fetch(&stub, "c1", ContentFormat::Text).unwrap_err();
    assert_eq!(err, "Hosted Chat API returned HTTP 500.");
}

// ── binary e2e ───────────────────────────────────────────────────────────────

/// Same pattern as `tests/cli_router.rs`: non-JWT token + 2099 expiry → no refresh.
fn logged_in_home(stub: &common::Stub) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: stub.addr.clone(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "tok".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(stub.router_url()),
        })
        .unwrap();
    home
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn chat_content_binary_e2e() {
    let stub = jarvice_stub();
    let home = logged_in_home(&stub);

    let ok = run(home.path(), &["chat", "content", "c1"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    assert_eq!(String::from_utf8_lossy(&ok.stdout), EXPECTED_TEXT);

    let missing = run(home.path(), &["chat", "content", "missing"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(
        stderr(&missing).contains("Error: Chat thread 'missing' was not found"),
        "{}",
        stderr(&missing)
    );

    let empty = tempfile::tempdir().unwrap();
    let out = run(empty.path(), &["chat", "content", "c1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).contains("Not logged in"), "{}", stderr(&out));

    let out = run(home.path(), &["chat", "content", "c1", "--profile", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("Unknown profile 'nope'"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn chat_list_still_works() {
    let stub = jarvice_stub();
    let home = logged_in_home(&stub);
    let out = run(home.path(), &["chat", "list"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("c1"));
}
