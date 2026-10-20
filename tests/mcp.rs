//! `monocle mcp ...` against a recording stub jarvice: every call's method, path,
//! Authorization header and body are asserted (binary e2e, HOME tempdir).

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};

/// (method, url, authorization header, body)
type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

const LIST: &str = r#"{"servers":[
 {"id":"github","name":"github","enabled":true,"auth_required":true,"auth_satisfied":true,"tools":[{"name":"a","enabled":true},{"name":"b","enabled":false}],"future":1},
 {"id":"uuid-1","name":"My Srv","enabled":false,"auth_required":true,"auth_failed":true,"tools":[]}]}"#;

fn stub(seen: &Seen, status: u16, error_body: &str) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
    let (seen, error_body) = (seen.clone(), error_body.to_string());
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let (m, u) = (req.method().as_str().to_string(), req.url().to_string());
            seen.lock()
                .unwrap()
                .push((m.clone(), u.clone(), auth, body));
            let (code, resp) = if status != 200 {
                (status, error_body.clone())
            } else {
                match (m.as_str(), u.as_str()) {
                    ("GET", "/api/v1/mcp/servers") => (200, LIST.to_string()),
                    ("GET", "/api/v1/mcp/servers/attention") => (200, r#"{"count":2}"#.into()),
                    ("GET", u) if u.starts_with("/api/v1/mcp/servers/") => (
                        200,
                        r#"{"id":"x","name":"My Srv","enabled":true,"tools":[{"name":"t 1","enabled":false},{"name":"t2"}]}"#.into(),
                    ),
                    _ => (200, r#"{"status":"ok"}"#.into()),
                }
            };
            let _ = req.respond(tiny_http::Response::from_string(resp).with_status_code(code));
        }
    });
    addr
}

fn run(addr: &str, args: &[&str]) -> (Output, Seen, tempfile::TempDir) {
    let seen = Seen::default();
    let addr = if addr.is_empty() {
        stub(&seen, 200, "")
    } else {
        addr.to_string()
    };
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: addr,
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "tok".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: None,
        })
        .unwrap();
    (exec(home.path(), args), seen, home)
}

fn exec(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Assert the exact request sequence: (method, path, body).
fn assert_calls(seen: &Seen, want: &[(&str, &str, &str)]) {
    let got = seen.lock().unwrap();
    let got_short: Vec<(&str, &str, &str)> = got
        .iter()
        .map(|(m, u, _, b)| (m.as_str(), u.as_str(), b.as_str()))
        .collect();
    assert_eq!(got_short, want);
    assert!(got.iter().all(|c| c.2.contains("tok")), "{got:?}");
}

#[test]
fn ls_prints_table_and_tolerates_unknown_fields() {
    let (o, seen, _h) = run("", &["mcp", "ls"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("github")
            && t.contains("connected")
            && t.contains("My Srv")
            && t.contains("failed"),
        "{t}"
    );
    assert_calls(&seen, &[("GET", "/api/v1/mcp/servers", "")]);
}

#[test]
fn ls_json_passes_body_through() {
    let (o, _s, _h) = run("", &["mcp", "ls", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["servers"][0]["future"], 1);
}

#[test]
fn show_resolves_name_to_id_and_encodes_path() {
    let (o, seen, _h) = run("", &["mcp", "show", "My Srv"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("t 1") && t.contains("no") && t.contains("t2"),
        "{t}"
    );
    assert_calls(
        &seen,
        &[
            ("GET", "/api/v1/mcp/servers", ""),
            ("GET", "/api/v1/mcp/servers/uuid-1", ""),
        ],
    );
}

#[test]
fn show_json_and_unknown_server() {
    let (o, _s, _h) = run("", &["mcp", "show", "github", "--json"]);
    assert!(out(&o).contains("\"tools\""));
    let (o, seen, _h) = run("", &["mcp", "show", "nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn attention_prints_count() {
    let (o, seen, _h) = run("", &["mcp", "attention"]);
    assert_eq!(out(&o), "2\n");
    assert_calls(&seen, &[("GET", "/api/v1/mcp/servers/attention", "")]);
    let (o, _s, _h) = run("", &["mcp", "attention", "--json"]);
    assert!(out(&o).contains("\"count\": 2"));
}

#[test]
fn enable_and_disable_post_once() {
    let (o, seen, _h) = run("", &["mcp", "enable", "My Srv"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_calls(
        &seen,
        &[("POST", "/api/v1/mcp/servers/My%20Srv/enable", "{}")],
    );
    let (o, seen, _h) = run("", &["mcp", "disable", "github"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_calls(
        &seen,
        &[("POST", "/api/v1/mcp/servers/github/disable", "{}")],
    );
}

#[test]
fn tool_toggle_posts_enabled_body() {
    let (o, seen, _h) = run("", &["mcp", "tool", "github", "a/b", "disable"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_calls(
        &seen,
        &[(
            "POST",
            "/api/v1/mcp/servers/github/tools/a%2Fb",
            r#"{"enabled":false}"#,
        )],
    );
    let (_o, seen, _h) = run("", &["mcp", "tool", "github", "a", "enable"]);
    assert_calls(
        &seen,
        &[(
            "POST",
            "/api/v1/mcp/servers/github/tools/a",
            r#"{"enabled":true}"#,
        )],
    );
}

#[test]
fn errors_show_status_and_code_but_never_the_body() {
    for args in [
        &["mcp", "ls"][..],
        &["mcp", "attention"],
        &["mcp", "enable", "x"],
        &["mcp", "tool", "x", "t", "disable"],
    ] {
        let seen = Seen::default();
        let addr = stub(
            &seen,
            404,
            r#"{"detail":{"code":"not_enabled"},"leak":"SECRET_MARKER"}"#,
        );
        let (o, _s, _h) = run(&addr, args);
        assert_eq!(o.status.code(), Some(1));
        let e = err(&o);
        assert!(e.contains("HTTP 404 (not_enabled)"), "{e}");
        assert!(!e.contains("SECRET_MARKER"), "{e}");
    }
    let seen = Seen::default();
    let addr = stub(&seen, 500, "SECRET_MARKER stack");
    let (o, _s, _h) = run(&addr, &["mcp", "disable", "x"]);
    assert!(err(&o).contains("HTTP 500") && !err(&o).contains("SECRET_MARKER"));
}
