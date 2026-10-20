//! `monocle schedule create|update` against a recording stub jarvice (binary e2e).

use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

const A: &str = "/api/v1/schedules";
/// A stored schedule with non-default values everywhere, plus server-only fields.
const STORED: &str = r#"{"id":7,"public_id":"s-1","user_id":"u","name":"Old","model_id":"m1","prompt_template":"old prompt",
"kind":"recurring","recurrence":"FREQ=DAILY","run_at":null,"dtstart":1700000040,"timezone":"Asia/Seoul",
"thread_mode":"continue","target_chat_id":"chat-9","requires_web_search":true,"enabled":false,
"expires_at":1900000000,"overlap_policy":"skip","catchup_policy":"run_missed","next_fire_at":1700000100,
"last_fired_at":null,"created_at":1,"updated_at":2}"#;

/// `put_status`: status of PUT /mcp-servers; `fail_post`: status for create/update.
fn stub(seen: &Seen, put_status: u16, post_status: u16) -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
    let seen = seen.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (m, u) = (req.method().as_str().to_string(), req.url().to_string());
            seen.lock().unwrap().push((m.clone(), u.clone(), body));
            let (code, resp) = if u.ends_with("/mcp-servers") && m == "PUT" {
                (put_status, r#"{"detail":"SECRET-SERVER-TEXT"}"#.to_string())
            } else if m == "POST" && post_status != 200 {
                (
                    post_status,
                    r#"{"detail":"SECRET-SERVER-TEXT"}"#.to_string(),
                )
            } else if u.starts_with(&format!("{A}/preview")) {
                (200, "[1700000000,1700086400]".to_string())
            } else if u == format!("{A}/create") {
                (200, r#"{"public_id":"new-1","name":"n"}"#.to_string())
            } else if u.ends_with("/mcp-servers") {
                (200, r#"{"server_ids":[],"ever_set":false}"#.to_string())
            } else {
                (200, STORED.to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(resp).with_status_code(code));
        }
    });
    addr
}

fn run(args: &[&str]) -> (Output, Seen) {
    run_codes(args, 200, 200)
}

fn run_codes(args: &[&str], put_status: u16, post_status: u16) -> (Output, Seen) {
    let seen = Seen::default();
    let addr = stub(&seen, put_status, post_status);
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
    let o = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home.path())
        .output()
        .unwrap();
    (o, seen)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn calls(seen: &Seen) -> Vec<(String, String)> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|c| (c.0.clone(), c.1.clone()))
        .collect()
}
fn body(seen: &Seen, i: usize) -> Value {
    serde_json::from_str(&seen.lock().unwrap()[i].2).unwrap()
}
fn call(m: &str, u: &str) -> (String, String) {
    (m.to_string(), u.to_string())
}

#[test]
fn create_one_off_body_exact() {
    let (o, seen) = run(&[
        "schedule",
        "create",
        "--model",
        "m1",
        "--prompt",
        "hi",
        "--at",
        "2023-11-14T22:13:20Z",
        "--name",
        "N",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "new-1\n");
    assert!(err(&o).contains("Created schedule new-1"), "{}", err(&o));
    assert_eq!(calls(&seen), vec![call("POST", &format!("{A}/create"))]);
    assert_eq!(
        body(&seen, 0),
        json!({"name":"N","model_id":"m1","prompt_template":"hi","kind":"one_off","recurrence":null,
               "run_at":1700000000,"dtstart":null,"timezone":"UTC","thread_mode":"new","target_chat_id":null,
               "requires_web_search":false,"enabled":true,"expires_at":null,"overlap_policy":"skip",
               "catchup_policy":"skip_missed"})
    );
}

#[test]
fn create_recurring_body_with_all_flags() {
    let (o, seen) = run(&[
        "schedule",
        "create",
        "--model",
        "m1",
        "--prompt",
        "hi",
        "--rrule",
        "FREQ=DAILY",
        "--dtstart",
        "2023-11-14T22:14:00Z",
        "--timezone",
        "Asia/Seoul",
        "--thread-mode",
        "continue",
        "--target-chat",
        "chat-1",
        "--expires-at",
        "2030-01-01T00:00:00Z",
        "--disabled",
        "--catchup",
        "run_missed",
        "--web-search",
        "--json",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("\"public_id\": \"new-1\""), "{}", out(&o));
    assert_eq!(
        body(&seen, 0),
        json!({"name":null,"model_id":"m1","prompt_template":"hi","kind":"recurring","recurrence":"FREQ=DAILY",
               "run_at":null,"dtstart":1700000040,"timezone":"Asia/Seoul","thread_mode":"continue",
               "target_chat_id":"chat-1","requires_web_search":true,"enabled":false,"expires_at":1893456000,
               "overlap_policy":"skip","catchup_policy":"run_missed"})
    );
}

#[test]
fn create_prompt_file_and_relative_time() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("p.txt");
    std::fs::write(&f, "from file").unwrap();
    let (o, seen) = run(&[
        "schedule",
        "create",
        "--model",
        "m",
        "--prompt-file",
        f.to_str().unwrap(),
        "--at",
        "+10m",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let b = body(&seen, 0);
    assert_eq!(b["prompt_template"], "from file");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        (b["run_at"].as_i64().unwrap() - (now + 600)).abs() < 30,
        "{b}"
    );
}

#[test]
fn mcp_server_triggers_put_after_create() {
    let (o, seen) = run(&[
        "schedule",
        "create",
        "--model",
        "m",
        "--prompt",
        "p",
        "--at",
        "+1h",
        "--mcp-server",
        "github",
        "--mcp-server",
        "slack",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        calls(&seen),
        vec![
            call("POST", &format!("{A}/create")),
            call("PUT", &format!("{A}/new-1/mcp-servers"))
        ]
    );
    assert_eq!(body(&seen, 1), json!({"server_ids":["github","slack"]}));
    assert_eq!(out(&o), "new-1\n");
}

#[test]
fn mcp_put_failure_reports_id_and_fails_without_echoing_body() {
    let (o, seen) = run_codes(
        &[
            "schedule",
            "create",
            "--model",
            "m",
            "--prompt",
            "p",
            "--at",
            "+1h",
            "--mcp-server",
            "github",
        ],
        500,
        200,
    );
    assert_ne!(o.status.code(), Some(0));
    assert!(
        err(&o).contains("Schedule new-1 was created"),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("schedule delete new-1"), "{}", err(&o));
    assert!(!err(&o).contains("SECRET") && !out(&o).contains("SECRET"));
    assert_eq!(calls(&seen).len(), 2);
}

#[test]
fn create_error_body_not_echoed() {
    let (o, _s) = run_codes(
        &[
            "schedule", "create", "--model", "m", "--prompt", "p", "--at", "+1h",
        ],
        200,
        403,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("HTTP 403") && !err(&o).contains("SECRET"),
        "{}",
        err(&o)
    );
}

#[test]
fn dry_run_only_previews() {
    let (o, seen) = run(&[
        "schedule",
        "create",
        "--model",
        "m",
        "--prompt",
        "p",
        "--rrule",
        "FREQ=DAILY",
        "--dtstart",
        "+1h",
        "--dry-run",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        calls(&seen),
        vec![call("POST", &format!("{A}/preview?count=5"))]
    );
    assert_eq!(body(&seen, 0)["kind"], "recurring");
    assert!(out(&o).contains("2023-11-14 22:13:20Z"), "{}", out(&o));
}

#[test]
fn invalid_combinations_make_no_request() {
    let cases: &[&[&str]] = &[
        &[
            "create",
            "--model",
            "m",
            "--prompt",
            "p",
            "--at",
            "+1m",
            "--rrule",
            "FREQ=DAILY",
        ],
        &[
            "create",
            "--model",
            "m",
            "--prompt",
            "p",
            "--at",
            "+1m",
            "--dtstart",
            "+1m",
        ],
        &["create", "--prompt", "p", "--at", "+1m"],
        &["create", "--model", "m", "--at", "+1m"],
        &["create", "--model", "m", "--prompt", "p"],
        &[
            "create",
            "--model",
            "m",
            "--prompt",
            "p",
            "--rrule",
            "FREQ=DAILY",
        ],
        &[
            "create", "--model", "m", "--prompt", "p", "--at", "tomorrow",
        ],
        &[
            "create",
            "--model",
            "m",
            "--prompt",
            "p",
            "--at",
            "+1m",
            "--thread-mode",
            "bogus",
        ],
        &["update", "s-1"],
        &["update", "s-1", "--at", "+1m", "--rrule", "FREQ=DAILY"],
        &["update", "a/../b", "--name", "x"],
    ];
    for c in cases {
        let mut args = vec!["schedule"];
        args.extend_from_slice(c);
        let (o, seen) = run(&args);
        assert_ne!(o.status.code(), Some(0), "{c:?}");
        assert!(calls(&seen).is_empty(), "{c:?}: {:?}", calls(&seen));
    }
}

#[test]
fn update_gets_then_posts_full_form_preserving_untouched() {
    let (o, seen) = run(&[
        "schedule",
        "update",
        "s-1",
        "--name",
        "New",
        "--no-web-search",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        calls(&seen),
        vec![
            call("GET", &format!("{A}/s-1")),
            call("POST", &format!("{A}/s-1/update"))
        ]
    );
    // Everything except the overridden fields is what GET returned; next_fire_at is not sent.
    assert_eq!(
        body(&seen, 1),
        json!({"name":"New","model_id":"m1","prompt_template":"old prompt","kind":"recurring",
               "recurrence":"FREQ=DAILY","run_at":null,"dtstart":1700000040,"timezone":"Asia/Seoul",
               "thread_mode":"continue","target_chat_id":"chat-9","requires_web_search":false,"enabled":false,
               "expires_at":1900000000,"overlap_policy":"skip","catchup_policy":"run_missed"})
    );
    assert!(err(&o).contains("Updated schedule s-1"), "{}", err(&o));
}

#[test]
fn update_switching_kind_and_clearing_fields() {
    let (o, seen) = run(&[
        "schedule",
        "update",
        "s-1",
        "--at",
        "2023-11-14T22:13:20Z",
        "--expires-at",
        "none",
        "--target-chat",
        "none",
        "--enabled",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let b = body(&seen, 1);
    assert_eq!(
        (
            &b["kind"],
            &b["run_at"],
            &b["recurrence"],
            &b["dtstart"],
            &b["expires_at"],
            &b["target_chat_id"],
            &b["enabled"]
        ),
        (
            &json!("one_off"),
            &json!(1700000000),
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &json!(true)
        )
    );
    assert_eq!(b["model_id"], "m1");
}

#[test]
fn update_error_not_echoed() {
    let (o, _s) = run_codes(&["schedule", "update", "s-1", "--name", "x"], 200, 400);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("HTTP 400") && !err(&o).contains("SECRET"),
        "{}",
        err(&o)
    );
}
