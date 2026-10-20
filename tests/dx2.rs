//! DX PR2: `-q`, `--no-color`, new `--json` shapes, `chat list --limit`, `mcp list`.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use monocle_cli::credentials::{Credentials, CredentialsData};
use serde_json::Value;

const SECRET: &str = "SECRET-TOKEN-zzz";

fn stub() -> common::Stub {
    common::stub(|_, method, url, _| {
        let path = url.split('?').next().unwrap();
        match (method, path) {
            ("GET", "/api/v1/chats/") => (
                200,
                r#"[{"id":"c-old","title":"old","updated_at":100,"created_at":50},
                    {"id":"c-new","title":"한글 새 제목","updated_at":300,"created_at":60},
                    {"id":"c-mid","title":"mid","updated_at":200,"created_at":70}]"#
                    .into(),
            ),
            ("GET", "/api/v1/knowledge/") => (
                200,
                r#"[{"id":"kb1","name":"fixtures","files":[{"id":"f1","meta":{"name":"a.txt"}}]}]"#
                    .into(),
            ),
            ("GET", "/api/v1/models/") => (
                200,
                r#"[{"id":"m1","name":"Seeded","base_model_id":"gpt-x","is_active":false}]"#.into(),
            ),
            ("GET", "/v1/models") => (
                200,
                r#"{"data":[{"id":"gpt-x","name":"GPT X","owned_by":"openai","context_window":128000,"modality":"text"}]}"#.into(),
            ),
            ("GET", "/api/v1/mcp/servers") => (200, r#"{"servers":[]}"#.into()),
            ("GET", "/boom/api/v1/chats/") => (500, "{}".into()),
            _ => (404, "{}".into()),
        }
    })
}

fn setup() -> (common::Stub, tempfile::TempDir) {
    let stub = stub();
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: stub.addr.clone(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: SECRET.into(),
            refresh_token: format!("{SECRET}-r"),
            id_token: format!("{SECRET}-i"),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(stub.router_url()),
        })
        .unwrap();
    (stub, home)
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .env_remove("FORCE_COLOR")
        .env_remove("NO_COLOR")
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

fn json_of(home: &Path, args: &[&str]) -> Value {
    let o = run(home, args);
    assert_eq!(o.status.code(), Some(0), "{args:?}: {}", err(&o));
    assert!(!out(&o).contains(SECRET), "{args:?} leaked a token");
    serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{args:?}: {e}: {}", out(&o)))
}

#[test]
fn quiet_hides_counts_and_hints_but_not_errors() {
    let (_s, home) = setup();
    let o = run(home.path(), &["chat", "list"]);
    assert!(
        err(&o).contains("thread(s)") && err(&o).contains("Profile:"),
        "{}",
        err(&o)
    );
    let o = run(home.path(), &["-q", "chat", "list"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(err(&o).is_empty(), "{}", err(&o));
    assert!(out(&o).contains("c-new"));
    // errors survive -q
    let o = run(home.path(), &["chat", "content", "bad/id", "-q"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(err(&o).contains("Invalid chat thread ID"), "{}", err(&o));
}

#[test]
fn no_color_flag_matches_no_color_env() {
    let (_s, home) = setup();
    let colored = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .arg("setup")
        .env("HOME", home.path())
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    assert!(err(&colored).contains('\x1b'), "{}", err(&colored));
    let plain = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(["--no-color", "setup"])
        .env("HOME", home.path())
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    assert!(!err(&plain).contains('\x1b'), "{}", err(&plain));
}

#[test]
fn json_shapes_are_stable_and_secret_free() {
    let (_s, home) = setup();
    let h = home.path();
    let v = json_of(h, &["chat", "list", "--json"]);
    // server order kept, ISO timestamps
    assert_eq!(v[0]["id"], "c-old");
    assert_eq!(v[0]["updated_at"], "1970-01-01T00:01:40.000Z");
    assert_eq!(v[1]["title"], "한글 새 제목");
    let v = json_of(h, &["knowledge", "list", "--json"]);
    assert_eq!(
        v[0],
        serde_json::json!({"id":"kb1","name":"fixtures","files":1})
    );
    let v = json_of(h, &["knowledge", "list", "fixtures", "--json"]);
    assert_eq!(v[0], serde_json::json!({"id":"f1","name":"a.txt"}));
    let v = json_of(h, &["router", "models", "--json"]);
    assert_eq!(v[0]["id"], "gpt-x");
    assert_eq!(v[0]["context_window"], 128000.0);
    let v = json_of(h, &["workspace-model", "list", "--json"]);
    assert_eq!(
        v[0],
        serde_json::json!({"id":"m1","name":"Seeded","base_model_id":"gpt-x","active":false})
    );
    let v = json_of(h, &["profile", "list", "--json"]);
    assert_eq!(v[0]["name"], "default");
    assert_eq!(v[0]["logged_in"], true);
    assert_eq!(v[0]["default"], true);
    let v = json_of(h, &["status", "--json"]);
    assert_eq!(v["logged_in"], true);
    assert_eq!(v["email"], "e@example.com");
    assert_eq!(v["access_token_valid"], true);
    assert_eq!(v["refresh_token_expires_at"], "2099-01-01T00:00:00Z");
    assert!(v.get("access_token").is_none());
}

#[test]
fn status_json_when_logged_out() {
    let home = tempfile::tempdir().unwrap();
    let v = json_of(home.path(), &["status", "--json"]);
    assert_eq!(v["logged_in"], false);
}

#[test]
fn chat_list_limit_takes_newest_first() {
    let (_s, home) = setup();
    let v = json_of(home.path(), &["chat", "list", "--limit", "2", "--json"]);
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["c-new", "c-mid"]);
    let o = run(home.path(), &["chat", "list", "--limit", "1"]);
    assert!(
        out(&o).contains("c-new") && !out(&o).contains("c-mid"),
        "{}",
        out(&o)
    );
}

#[test]
fn chat_list_table_pads_korean_by_display_width() {
    let (_s, home) = setup();
    let o = run(home.path(), &["chat", "list"]);
    let t = out(&o);
    let row = t.lines().find(|l| l.contains("c-new")).unwrap();
    // title column is 12 wide (한글 새 제목 = 4+... ) and UPDATED starts at the same column
    let w = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let upd = t.lines().next().unwrap().find("UPDATED").unwrap();
    let head_col = w(&t.lines().next().unwrap()[..upd]);
    let row_col = w(&row[..row.find("1970").unwrap()]);
    assert_eq!(head_col, row_col, "{t}");
}

#[test]
fn mcp_list_is_alias_of_ls() {
    let (_s, home) = setup();
    let a = run(home.path(), &["mcp", "ls", "--json"]);
    let b = run(home.path(), &["mcp", "list", "--json"]);
    assert_eq!(b.status.code(), Some(0), "{}", err(&b));
    assert_eq!(out(&a), out(&b));
}
