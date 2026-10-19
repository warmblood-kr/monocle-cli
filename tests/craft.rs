//! `monocle craft session ...` on both lanes against one stub that emulates
//! jarvice (sdk) and the Craft server (openworker), recording every request
//! (binary e2e: HOME tempdir + credentials pointing at the stub).

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use monocle_cli::credentials::{Credentials, CredentialsData};

/// `METHOD url [auth]` per request, in order.
type Log = Arc<Mutex<Vec<String>>>;

fn reply(method: &str, url: &str) -> (u16, Vec<u8>) {
    let json = |s: &str| s.as_bytes().to_vec();
    let id = url.split('/').find(|p| *p == "parked" || *p == "boom");
    match (method, url, id) {
        // ── sdk lane (jarvice) ──
        ("GET", "/api/v1/agent/sessions?skip=0&limit=50", _) => (200, json(
            r#"[{"id":"sess-a","title":"First\nchat","status":"idle","created_at":1700000000,"updated_at":1700003600}]"#)),
        ("GET", "/api/v1/agent/sessions/sess-a", _) => (200, json(
            r#"{"id":"sess-a","user_id":"u1","title":"First chat","status":"idle","created_at":1700000000,"updated_at":1700003600,"meta":{"k":1},"craft_env":null}"#)),
        ("GET", "/api/v1/agent/sessions/sess-a/history", _) => (200, json(
            r#"{"task_id":"t","session_id":"sess-a","entries":[{"type":"user","x":1},{"type":"assistant"}],"entry_count":2,"in_flight":false}"#)),
        ("GET", "/api/v1/agent/sessions/sess-a/files?folder=outputs", _) => (200, json(
            r#"{"task_id":"t","folder":"outputs","path":"/o","files":[{"name":"a b.txt","size":5,"modified":"2024-01-01","is_directory":false},{"name":"sub","size":0,"modified":"2024-01-02","is_directory":true}]}"#)),
        ("GET", "/api/v1/agent/sessions/sess-a/files/download/outputs/a%20b.txt", _) => (200, b"sdk-bytes\x00\xff".to_vec()),
        ("DELETE", "/api/v1/agent/sessions/sess-a", _) => (200, json(r#"{"success":true}"#)),
        (_, u, Some("boom")) if u.starts_with("/api/") => (500, json(r#"{"detail":"SECRET-korean-detail"}"#)),
        (_, u, Some("parked")) if u.starts_with("/api/") => (403, json(r#"{"detail":{"code":"not_owner","secret":"SECRET"}}"#)),
        (_, u, _) if u.starts_with("/api/") => (404, json(r#"{"detail":"SECRET"}"#)),
        // ── openworker lane (craft server) ──
        ("GET", "/v1/sessions", _) => (200, json(
            r#"{"sessions":[{"session_id":"cli-stg-enforce-t1792437390","title":"Craft one","workspace":"/w","updated_at":"2024-05-01T10:00:00Z","messages":3}]}"#)),
        ("GET", "/v1/sessions?workspace=%2Fmy%20ws", _) => (200, json(r#"{"sessions":[]}"#)),
        ("GET", "/v1/sessions/cli-stg-enforce-t1792437390/messages", _) => (200, json(
            r#"{"messages":[{"role":"system","content":"SYS"},{"role":"user","content":"hello\nthere"},{"role":"assistant","text":"hi"},{"role":"tool","kind":"x"}]}"#)),
        ("GET", "/v1/sessions/cli-stg-enforce-t1792437390/artifacts", _) => (200, json(
            r#"{"artifacts":[{"name":"a.txt","path":"out/a.txt","size":7},{"weird":true}]}"#)),
        ("GET", "/v1/sessions/parked/artifacts", _) => (409, json(r#"{"artifacts":[],"error":"SECRET sandbox"}"#)),
        ("GET", "/v1/sessions/cli-stg-enforce-t1792437390/artifacts/read?path=out%2Fa%20b.txt&download=1", _) => (200, b"craft-bytes".to_vec()),
        ("GET", "/v1/sessions/boom/artifacts/read?path=x&download=1", _) => (500, json(r#"{"error":"SECRET leaked body!"}"#)),
        ("GET", "/v1/sessions/quota/messages", _) => (429, json(r#"{"error":"quota_exceeded"}"#)),
        ("GET", "/v1/sessions/nope/artifacts/read?path=x&download=1", _) => (404, json(r#"{"error":"SECRET"}"#)),
        ("DELETE", "/v1/sessions/cli-stg-enforce-t1792437390", _) => (200, json(r#"{"ok":true}"#)),
        _ => (404, json(r#"{"error":"SECRET"}"#)),
    }
}

fn stub() -> (String, Log) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
    let log = Log::default();
    let l = log.clone();
    thread::spawn(move || {
        for req in server.incoming_requests() {
            let (method, url) = (req.method().as_str().to_string(), req.url().to_string());
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_else(|| "-".into());
            l.lock().unwrap().push(format!("{method} {url} [{auth}]"));
            let (status, body) = reply(&method, &url);
            let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(status));
        }
    });
    (addr, log)
}

fn setup() -> (String, Log, tempfile::TempDir) {
    let (addr, log) = stub();
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: addr.clone(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "tok".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(format!("http://{addr}")),
        })
        .unwrap();
    (addr, log, home)
}

fn run(home: &Path, craft_url: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_monocle"));
    cmd.args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .env_remove("MONOCLE_CRAFT_URL")
        .current_dir(home);
    if let Some(u) = craft_url {
        cmd.env("MONOCLE_CRAFT_URL", u);
    }
    cmd.output().unwrap()
}

/// Run on the sdk lane (jarvice is the stub's tenant host).
fn sdk(home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["craft", "--lane", "sdk", "session"];
    a.extend_from_slice(args);
    run(home, None, &a)
}

/// Run on the default (openworker) lane against the stub as the Craft server.
fn ow(addr: &str, home: &Path, args: &[&str]) -> Output {
    let mut a = vec!["craft", "session"];
    a.extend_from_slice(args);
    run(home, Some(&format!("http://{addr}")), &a)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn requests(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

const SDK_AUTH: &str = "[Bearer tok]";
const OW_ID: &str = "cli-stg-enforce-t1792437390";

#[test]
fn sdk_list_show_history() {
    let (_, log, home) = setup();
    let o = sdk(home.path(), &["list"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("sess-a") && t.contains("idle") && t.contains("2023-11-14"),
        "{t}"
    );
    assert!(t.contains("First chat"), "newline in title collapsed: {t}");
    assert!(err(&o).contains("Profile: default"), "{}", err(&o));

    let o = sdk(home.path(), &["show", "sess-a"]);
    assert!(
        out(&o).contains("title: First chat") && out(&o).contains(r#"meta: {"k":1}"#),
        "{}",
        out(&o)
    );
    let o = sdk(home.path(), &["show", "sess-a", "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap()["user_id"],
        "u1"
    );

    let o = sdk(home.path(), &["history", "sess-a"]);
    assert_eq!(
        out(&o),
        "{\"type\":\"user\",\"x\":1}\n{\"type\":\"assistant\"}\n"
    );
    let o = sdk(home.path(), &["history", "sess-a", "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap()["entry_count"],
        2
    );

    let a = SDK_AUTH;
    assert_eq!(
        requests(&log),
        [
            format!("GET /api/v1/agent/sessions?skip=0&limit=50 {a}"),
            format!("GET /api/v1/agent/sessions/sess-a {a}"),
            format!("GET /api/v1/agent/sessions/sess-a {a}"),
            format!("GET /api/v1/agent/sessions/sess-a/history {a}"),
            format!("GET /api/v1/agent/sessions/sess-a/history {a}"),
        ]
    );
}

#[test]
fn sdk_files_download_delete() {
    let (_, log, home) = setup();
    let o = sdk(home.path(), &["files", "sess-a", "--folder", "outputs"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("a b.txt") && out(&o).contains("sub/"),
        "{}",
        out(&o)
    );

    let o = sdk(home.path(), &["download", "sess-a", "outputs/a b.txt"]);
    assert_eq!(o.stdout, b"sdk-bytes\x00\xff");
    let file = home.path().join("got.bin");
    let o = sdk(
        home.path(),
        &[
            "download",
            "sess-a",
            "/outputs/a b.txt",
            "-o",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read(&file).unwrap(), b"sdk-bytes\x00\xff");
    assert!(o.stdout.is_empty());

    let o = sdk(home.path(), &["delete", "sess-a"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("Deleted session sess-a"), "{}", err(&o));
    let o = sdk(home.path(), &["delete", "sess-a", "--json"]);
    assert!(out(&o).contains("\"success\": true"), "{}", out(&o));

    let reqs = requests(&log);
    let a = SDK_AUTH;
    assert_eq!(
        reqs[0],
        format!("GET /api/v1/agent/sessions/sess-a/files?folder=outputs {a}")
    );
    assert_eq!(
        reqs[1],
        format!("GET /api/v1/agent/sessions/sess-a/files/download/outputs/a%20b.txt {a}")
    );
    assert_eq!(reqs[2], reqs[1]);
    assert_eq!(reqs[3], format!("DELETE /api/v1/agent/sessions/sess-a {a}"));
    assert_eq!(reqs.len(), 5);
}

#[test]
fn sdk_errors_do_not_echo_bodies() {
    let (_, _, home) = setup();
    let cases = [
        (["show", "boom"], "Showing session failed: HTTP 500."),
        (["delete", "parked"], "belongs to another user"),
        (["history", "missing"], "Session 'missing' was not found."),
    ];
    for (args, want) in cases {
        let o = sdk(home.path(), &args);
        assert_eq!(o.status.code(), Some(1));
        assert!(err(&o).contains(want), "{}", err(&o));
        assert!(!err(&o).contains("SECRET") && !out(&o).contains("SECRET"));
    }
}

#[test]
fn openworker_list_show_history() {
    let (addr, log, home) = setup();
    let o = ow(&addr, home.path(), &["list"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(OW_ID)
            && out(&o).contains("Craft one")
            && out(&o).contains("2024-05-01T10:00:00Z"),
        "{}",
        out(&o)
    );
    let o = ow(
        &addr,
        home.path(),
        &["list", "--workspace", "/my ws", "--json"],
    );
    assert_eq!(out(&o).trim(), "[]");

    let o = ow(&addr, home.path(), &["show", OW_ID]);
    assert!(
        out(&o).contains("workspace: /w") && out(&o).contains("messages: 3"),
        "{}",
        out(&o)
    );
    let o = ow(&addr, home.path(), &["show", "absent"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("Session 'absent' was not found."),
        "{}",
        err(&o)
    );

    let h = ow(&addr, home.path(), &["history", OW_ID]);
    assert_eq!(out(&h), "user\thello there\nassistant\thi\ntool\t\n");
    let h = ow(&addr, home.path(), &["history", OW_ID, "--all"]);
    assert!(out(&h).starts_with("system\tSYS\nuser\t"), "{}", out(&h));
    let h = ow(&addr, home.path(), &["history", OW_ID, "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&h)).unwrap()["messages"][0]["role"],
        "system"
    );

    let a = "[Bearer tok]";
    let reqs = requests(&log);
    assert_eq!(reqs[0], format!("GET /v1/sessions {a}"));
    assert_eq!(
        reqs[1],
        format!("GET /v1/sessions?workspace=%2Fmy%20ws {a}")
    );
    assert_eq!(reqs[2], format!("GET /v1/sessions {a}"));
    assert_eq!(reqs[4], format!("GET /v1/sessions/{OW_ID}/messages {a}"));
    assert_eq!(reqs.len(), 7);
}

#[test]
fn openworker_files_download_delete() {
    let (addr, log, home) = setup();
    let o = ow(&addr, home.path(), &["files", OW_ID]);
    assert_eq!(
        out(&o),
        "a.txt\tout/a.txt\t7\n{\"weird\":true}\n",
        "{}",
        err(&o)
    );
    let o = ow(&addr, home.path(), &["files", OW_ID, "--json"]);
    assert!(out(&o).contains("\"artifacts\""), "{}", out(&o));

    let o = ow(&addr, home.path(), &["files", "parked"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("files unavailable (session parked or sandbox gone)"),
        "{}",
        err(&o)
    );
    assert!(!err(&o).contains("SECRET"));

    let o = ow(&addr, home.path(), &["download", OW_ID, "out/a b.txt"]);
    assert_eq!(o.stdout, b"craft-bytes");
    let o = ow(&addr, home.path(), &["download", "nope", "x"]);
    assert!(
        err(&o).contains("was not found") && !err(&o).contains("SECRET"),
        "{}",
        err(&o)
    );

    let o = ow(&addr, home.path(), &["delete", OW_ID]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("Deleted session"), "{}", err(&o));

    let a = "[Bearer tok]";
    let reqs = requests(&log);
    assert_eq!(reqs[0], format!("GET /v1/sessions/{OW_ID}/artifacts {a}"));
    assert_eq!(reqs[2], format!("GET /v1/sessions/parked/artifacts {a}"));
    assert_eq!(
        reqs[3],
        format!("GET /v1/sessions/{OW_ID}/artifacts/read?path=out%2Fa%20b.txt&download=1 {a}")
    );
    assert_eq!(
        reqs.last().unwrap(),
        &format!("DELETE /v1/sessions/{OW_ID} {a}")
    );
}

#[test]
fn openworker_error_bodies_shown_only_as_safe_codes() {
    let (addr, _, home) = setup();
    let o = ow(&addr, home.path(), &["history", "quota"]);
    assert!(
        err(&o).contains("failed: HTTP 429 (quota_exceeded)."),
        "{}",
        err(&o)
    );
    let o = ow(&addr, home.path(), &["download", "boom", "x"]);
    assert!(
        err(&o).contains("Downloading file failed: HTTP 500."),
        "{}",
        err(&o)
    );
    assert!(!err(&o).contains("SECRET") && !out(&o).contains("SECRET"));
}

#[test]
fn invalid_ids_rejected_before_any_request() {
    let (addr, log, home) = setup();
    for bad in ["../x", "a/b", "a b", "x?y=1"] {
        for o in [
            sdk(home.path(), &["delete", bad]),
            ow(&addr, home.path(), &["delete", bad]),
            ow(&addr, home.path(), &["download", bad, "p"]),
        ] {
            assert_eq!(o.status.code(), Some(1), "{bad}");
            assert!(
                err(&o).contains("Invalid session ID."),
                "{bad}: {}",
                err(&o)
            );
        }
    }
    let long = "a".repeat(129);
    assert_eq!(sdk(home.path(), &["show", &long]).status.code(), Some(1));
    assert!(requests(&log).is_empty(), "{:?}", requests(&log));
}

#[test]
fn lane_specific_flags_rejected_before_any_request() {
    let (addr, log, home) = setup();
    let o = sdk(home.path(), &["list", "--workspace", "/w"]);
    assert!(
        err(&o).contains("--workspace is only supported on the openworker lane"),
        "{}",
        err(&o)
    );
    let o = ow(&addr, home.path(), &["files", OW_ID, "--folder", "uploads"]);
    assert!(
        err(&o).contains("--folder is only supported on the sdk lane"),
        "{}",
        err(&o)
    );
    assert!(requests(&log).is_empty());
}

#[test]
fn missing_craft_url_names_the_environment() {
    let (_, log, home) = setup();
    // Stub tenant is 127.0.0.1, so the inferred environment is `local`.
    let o = run(home.path(), None, &["craft", "session", "list"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("Craft server URL unknown for env 'local'; set MONOCLE_CRAFT_URL."),
        "{}",
        err(&o)
    );
    // Hosts are derived per environment (the test process has no override set).
    assert_eq!(
        monocle_cli::craft_server::base_url("prod").unwrap(),
        "https://craft.monocle-ai.com"
    );
    assert_eq!(
        monocle_cli::craft_server::base_url("stg").unwrap(),
        "https://craft-stg.monocle-ai.com"
    );
    assert!(requests(&log).is_empty());
}
