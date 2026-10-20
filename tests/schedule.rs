//! `monocle schedule ...` against a recording stub jarvice: every call's method, path,
//! Authorization header and body are asserted (binary e2e, HOME tempdir).

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};

/// (method, url, authorization header, body)
type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

const SCHED: &str = r#"{"public_id":"s-1","name":"Daily","kind":"recurring","recurrence":"FREQ=DAILY","enabled":true,"model_id":"m","timezone":"UTC","thread_mode":"new","next_fire_at":1700000000,"last_fired_at":null}"#;

/// Runs body served for GET .../runs (overridable per test through `RUNS`).
fn stub(seen: &Seen, status: u16, error_body: &str, runs: &str) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
    let (seen, error_body, runs) = (seen.clone(), error_body.to_string(), runs.to_string());
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
                .push((m.clone(), u.clone(), auth, body.clone()));
            let (code, resp) = if status != 200 {
                (status, error_body.clone())
            } else if m == "DELETE" {
                (
                    200,
                    if u.contains("gone") { "false" } else { "true" }.to_string(),
                )
            } else if u.starts_with("/api/v1/schedules/preview") {
                (200, "[1700000000,1700086400]".to_string())
            } else if u.contains("/runs") {
                (200, runs.clone())
            } else if u.ends_with("/mcp-servers") {
                (
                    200,
                    r#"{"server_ids":["github"],"ever_set":true}"#.to_string(),
                )
            } else if u == "/api/v1/schedules/" {
                (200, format!("[{SCHED}]"))
            } else {
                (200, SCHED.to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(resp).with_status_code(code));
        }
    });
    addr
}

fn run(addr: &str, args: &[&str]) -> (Output, Seen, tempfile::TempDir) {
    run_with(addr, args, RUNS_NONE)
}

fn run_with(addr: &str, args: &[&str], runs: &str) -> (Output, Seen, tempfile::TempDir) {
    let seen = Seen::default();
    let addr = if addr.is_empty() {
        stub(&seen, 200, "", runs)
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

const RUNS_NONE: &str = "[]";
const A: &str = "/api/v1/schedules";
const FIRED: &str = r#"[
 {"public_id":"r3","scheduled_for":300,"started_at":null,"finished_at":null,"status":"running","tool_results_summary":null},
 {"public_id":"r2","scheduled_for":200,"finished_at":250,"status":"succeeded","attempt":1,"error":null,
  "tool_results_summary":{"satisfied":true,"status":"succeeded","required":[],"missing":[],"failed":[],"calls":[{"name":"github__list_repos","ok":true,"error":null}]}},
 {"public_id":"r1","scheduled_for":100,"finished_at":150,"status":"succeeded",
  "tool_results_summary":{"calls":[]}}]"#;
const NOT_FIRED: &str = r#"[{"public_id":"r1","scheduled_for":1,"finished_at":2,"status":"succeeded","tool_results_summary":{"calls":[]}}]"#;
const NULL_SUMMARY: &str = r#"[{"public_id":"r1","scheduled_for":1,"finished_at":2,"status":"paused","tool_results_summary":null}]"#;
const UNKNOWN: &str = r#"[{"public_id":"r1","scheduled_for":1,"finished_at":2,"status":"failed","tool_results_summary":{"preflight_failures":["x"]}}]"#;
const UNFINISHED: &str = r#"[{"public_id":"r1","scheduled_for":1,"finished_at":null,"status":"running","tool_results_summary":{"calls":[{"name":"a__b"}]}}]"#;

#[test]
fn list_and_show_render_utc_and_json_is_passthrough() {
    let (o, seen, _h) = run("", &["schedule", "list"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("Daily") && out(&o).contains("2023-11-14 22:13"),
        "{}",
        out(&o)
    );
    // Table cells: minute precision and yes/no (`show` and `--json` keep the full values).
    let row = out(&o)
        .lines()
        .find(|l| l.contains("Daily"))
        .unwrap()
        .to_string();
    assert!(row.contains(" yes ") && !row.contains("true"), "{row}");
    assert!(!row.contains("22:13:20"), "{row}");
    let (o, seen2, _h) = run("", &["schedule", "show", "s-1"]);
    assert!(out(&o).contains("FREQ=DAILY"), "{}", out(&o));
    let (o, _s, _h) = run("", &["schedule", "show", "s-1", "--json"]);
    assert!(out(&o).contains("\"next_fire_at\": 1700000000"));
    assert_calls(&seen, &[("GET", &format!("{A}/"), "")]);
    assert_calls(&seen2, &[("GET", &format!("{A}/s-1"), "")]);
}

#[test]
fn invalid_id_sends_nothing() {
    let (o, seen, _h) = run("", &["schedule", "show", "a/../b"]);
    assert_ne!(o.status.code(), Some(0));
    assert_calls(&seen, &[]);
}

#[test]
fn toggle_on_and_off() {
    let (o, seen, _h) = run("", &["schedule", "toggle", "s-1", "disable"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_calls(
        &seen,
        &[("POST", &format!("{A}/s-1/toggle"), r#"{"enabled":false}"#)],
    );
    let (_o, seen, _h) = run("", &["schedule", "toggle", "s-1", "enable"]);
    assert_calls(
        &seen,
        &[("POST", &format!("{A}/s-1/toggle"), r#"{"enabled":true}"#)],
    );
}

#[test]
fn delete_true_ok_false_fails() {
    let (o, seen, _h) = run("", &["schedule", "delete", "s-1"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_calls(&seen, &[("DELETE", &format!("{A}/s-1/delete"), "")]);
    let (o, _s, _h) = run("", &["schedule", "delete", "gone"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("did not delete"), "{}", err(&o));
}

#[test]
fn runs_requests_limit() {
    let (o, seen, _h) = run_with("", &["schedule", "runs", "s-1", "--limit", "3"], FIRED);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("r2"));
    assert_calls(&seen, &[("GET", &format!("{A}/s-1/runs?limit=3"), "")]);
}

#[test]
fn mcp_servers_get_set_none() {
    let (o, seen, _h) = run("", &["schedule", "mcp-servers", "s-1"]);
    assert_eq!(out(&o).trim(), "github");
    assert_calls(&seen, &[("GET", &format!("{A}/s-1/mcp-servers"), "")]);
    let (_o, seen, _h) = run("", &["schedule", "mcp-servers", "s-1", "--set", "a,b"]);
    assert_calls(
        &seen,
        &[(
            "PUT",
            &format!("{A}/s-1/mcp-servers"),
            r#"{"server_ids":["a","b"]}"#,
        )],
    );
    let (_o, seen, _h) = run("", &["schedule", "mcp-servers", "s-1", "--none"]);
    assert_calls(
        &seen,
        &[(
            "PUT",
            &format!("{A}/s-1/mcp-servers"),
            r#"{"server_ids":[]}"#,
        )],
    );
}

#[test]
fn bare_set_errors_without_request() {
    let (o, seen, _h) = run("", &["schedule", "mcp-servers", "s-1", "--set"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--none"), "{}", err(&o));
    assert_calls(&seen, &[]);
}

#[test]
fn preview_builds_minimal_form() {
    let (o, seen, _h) = run(
        "",
        &[
            "schedule",
            "preview",
            "--recurrence",
            "FREQ=DAILY",
            "--dtstart",
            "1700000000",
            "--timezone",
            "Asia/Seoul",
            "--count",
            "2",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).lines().count(), 2);
    let form = r#"{"model_id":"preview","prompt_template":"preview","kind":"recurring","recurrence":"FREQ=DAILY","dtstart":1700000000,"run_at":null,"timezone":"Asia/Seoul"}"#;
    assert_calls(&seen, &[("POST", &format!("{A}/preview?count=2"), form)]);
}

#[test]
fn error_body_is_not_echoed() {
    let seen = Seen::default();
    let addr = stub(&seen, 500, "SECRET-internal-detail", "[]");
    for args in [
        &["schedule", "list"][..],
        &["schedule", "delete", "s-1"],
        &["schedule", "toggle", "s-1", "enable"],
    ] {
        let (o, _s, _h) = run(&addr, args);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            !err(&o).contains("SECRET") && !out(&o).contains("SECRET"),
            "{}",
            err(&o)
        );
    }
}

fn verdict(runs: &str, extra: &[&str]) -> (i32, String) {
    let mut a = vec!["schedule", "runs", "s-1"];
    a.extend_from_slice(extra);
    let (o, _s, _h) = run_with("", &a, runs);
    (o.status.code().unwrap(), err(&o))
}

#[test]
fn verify_exit_codes() {
    // latest FINISHED run is r2 (r3 is still running) and it called a tool
    assert_eq!(verdict(FIRED, &["--verify"]).0, 0);
    assert_eq!(verdict(FIRED, &["--verify=list_repos"]).0, 0);
    assert_eq!(verdict(FIRED, &["--verify=github__list_repos"]).0, 0);
    assert_eq!(verdict(FIRED, &["--verify=other"]).0, 1);
    // an explicitly chosen run with an empty calls list did not fire
    assert_eq!(verdict(FIRED, &["--verify", "--run", "r1"]).0, 1);
    assert_eq!(verdict(NOT_FIRED, &["--verify"]).0, 1);
    for (body, why) in [
        (NULL_SUMMARY, "no tool_results_summary"),
        (UNKNOWN, "no `calls`"),
        (UNFINISHED, "no matching finished run"),
    ] {
        let (code, e) = verdict(body, &["--verify"]);
        assert_eq!(code, 2, "{e}");
        assert!(e.contains("CANNOT VERIFY") && e.contains(why), "{e}");
    }
    assert_eq!(verdict(FIRED, &["--verify", "--run", "r3"]).0, 2);
    assert_eq!(verdict(FIRED, &["--verify", "--run", "nope"]).0, 2);
}
