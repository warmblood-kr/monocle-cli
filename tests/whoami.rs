//! `monocle whoami`: local mode makes no request and never prints token
//! strings; `--server` calls GET /api/v1/auths/ and drops the minted token.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;
use monocle_cli::credentials::{Credentials, CredentialsData};

const ACCESS: &str = "ACCESS-MARKER-aaa";
const REFRESH: &str = "REFRESH-MARKER-bbb";
const MINTED: &str = "MINTED-MARKER-ccc";

/// Serves every connection with one canned response; records request heads.
fn stub(status: u16, body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let h = heads.clone();
    thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            h.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase());
            let _ = write!(
                s,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nset-cookie: token={MINTED}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (addr, heads)
}

fn home(domain: &str) -> tempfile::TempDir {
    let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    let id_token = format!(
        "h.{}.IDSIG-MARKER",
        enc(r#"{"sub":"user-sub-1","iss":"https://stark.example","aud":"monocle-cli"}"#)
    );
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: domain.into(),
            tenant_name: "Tenant T".into(),
            email: "e@example.com".into(),
            access_token: ACCESS.into(),
            refresh_token: REFRESH.into(),
            id_token,
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: None,
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

fn all(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn no_secrets(s: &str) {
    for m in [ACCESS, REFRESH, MINTED, "IDSIG-MARKER"] {
        assert!(!s.contains(m), "leaked {m}: {s}");
    }
}

#[test]
fn local_mode_makes_no_request_and_prints_no_token() {
    let (addr, heads) = stub(200, "{}");
    let home = home(&addr);
    let o = run(home.path(), &["whoami"]);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    for want in [
        "Email: e@example.com",
        "id_token sub: user-sub-1",
        "id_token iss: https://stark.example",
        "id_token aud: monocle-cli",
        "Tenant: ",
        "Access Token: valid",
    ] {
        assert!(out.contains(want), "{want} in {out}");
    }
    no_secrets(&all(&o));
    assert!(heads.lock().unwrap().is_empty());
}

#[test]
fn json_shape() {
    let home = home("unused.invalid");
    let o = run(home.path(), &["whoami", "--json"]);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["email"], "e@example.com");
    assert_eq!(v["tenant"]["name"], "Tenant T");
    assert_eq!(v["id_token_claims"]["sub"], "user-sub-1");
    assert_eq!(v["access_token"]["valid"], true);
    assert!(v.get("server").is_none());
    no_secrets(&all(&o));
}

#[test]
fn server_mode_calls_auths_and_drops_token() {
    let (addr, heads) = stub(
        200,
        r#"{"id":"u1","email":"E@example.com","name":"N","role":"admin","token":"MINTED-MARKER-ccc","token_type":"Bearer","permissions":{"a":{},"b":{}}}"#,
    );
    let home = home(&addr);
    let o = run(home.path(), &["whoami", "--server"]);
    assert_eq!(o.status.code(), Some(0), "{}", all(&o));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(out.contains("role admin (2 permission groups)"), "{out}");
    assert!(out.contains("Server email matches local: yes"), "{out}");
    let heads = heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with("get /api/v1/auths/ http/1.1"),
        "{}",
        heads[0]
    );
    assert!(heads[0].contains(&format!(
        "authorization: bearer {}",
        ACCESS.to_ascii_lowercase()
    )));
    no_secrets(&all(&o));

    let o = run(home.path(), &["whoami", "--server", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["server"]["permission_groups"], 2);
    assert_eq!(v["server"]["email_matches_local"], true);
    no_secrets(&all(&o));
}

#[test]
fn server_error_never_echoes_body() {
    let (addr, _) = stub(
        401,
        r#"{"detail":"BODY-SECRET","token":"MINTED-MARKER-ccc"}"#,
    );
    let home = home(&addr);
    let o = run(home.path(), &["whoami", "--server"]);
    assert_eq!(o.status.code(), Some(1));
    let s = all(&o);
    assert!(s.contains("HTTP 401"), "{s}");
    assert!(s.contains("stark#1061"), "{s}");
    assert!(!s.contains("BODY-SECRET"), "{s}");
    no_secrets(&s);
}

#[test]
fn malformed_or_failing_response_never_leaks_token_or_cookie() {
    // Parse failure (wrong type for `id`, token in the body) and a 500 that
    // carries the minted token: neither may reach stdout/stderr. The CLI has no
    // verbose/debug flag, and `Resp` has no `Debug` impl, so these two paths
    // are the only ways a body could surface.
    for (status, body) in [
        (200, r#"{"token":"MINTED-MARKER-ccc","id":{"x":1}}"#),
        (200, r#"{"token":"MINTED-MARKER-ccc"} trailing"#),
        (500, r#"{"token":"MINTED-MARKER-ccc"}"#),
    ] {
        let (addr, _) = stub(status, body);
        let home = home(&addr);
        for args in [
            &["whoami", "--server"][..],
            &["whoami", "--server", "--json"][..],
        ] {
            let o = run(home.path(), args);
            assert_eq!(o.status.code(), Some(1), "{}", all(&o));
            no_secrets(&all(&o));
        }
    }
}
