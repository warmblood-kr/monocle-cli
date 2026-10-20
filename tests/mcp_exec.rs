//! `monocle mcp exec` against a recording stub jarvice: preflight (one GET, no
//! chat on failure), request body (`tools`, template), `--verify` exit codes.

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};

const LIST: &str = r#"{"servers":[
 {"id":"law","name":"law","enabled":true,"tools":[{"name":"get","enabled":true},{"name":"off","enabled":false}]},
 {"id":"dis","name":"dis","enabled":false,"tools":[]},
 {"id":"au","name":"au","enabled":true,"auth_required":true,"tools":[]}]}"#;
/// (method, url, body)
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;
const ANSWER: &str = "it returned x";

fn reply(tools_used: Option<&str>) -> String {
    let tu = tools_used.map_or(String::new(), |t| format!(r#","tools_used":{t}"#));
    format!(r#"{{"choices":[{{"message":{{"role":"assistant","content":"{ANSWER}"}}}}]{tu}}}"#)
}

/// Returns (stub, recorded (method,url,body)).
fn stub(resp: String, chat_status: u16) -> (common::Stub, Seen) {
    let seen: Seen = Arc::default();
    let s = seen.clone();
    let st = common::stub_with_headers(move |_, m, u, b| {
        s.lock().unwrap().push((m.into(), u.into(), b.into()));
        match (m, u) {
            ("GET", "/api/v1/mcp/servers") => (200, LIST.into(), vec![]),
            ("POST", "/api/responses") if chat_status == 200 => {
                (200, resp.clone(), vec![("x-thread-id", "th-9".to_string())])
            }
            ("POST", "/api/responses") => (chat_status, "SECRET-SERVER-BODY".into(), vec![]),
            _ => (404, String::new(), vec![]),
        }
    });
    (st, seen)
}

fn run(stub: &common::Stub, args: &[&str]) -> Output {
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
    exec(home.path(), args)
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

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn chats(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|c| c.1 == "/api/responses")
        .map(|c| c.2.clone())
        .collect()
}

#[test]
fn tool_sends_single_tool_id_and_template_and_prints_thread() {
    let (st, seen) = stub(reply(None), 200);
    let o = run(&st, &["mcp", "exec", "law/get"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("{ANSWER}\n"));
    assert!(err(&o).contains("Thread: th-9"), "{}", err(&o));
    let c = chats(&seen);
    assert_eq!(c.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&c[0]).unwrap();
    assert_eq!(v["tools"], serde_json::json!(["mcp:law__get"]));
    assert!(
        c[0].contains(
            "Call the `get` tool of the `law` MCP server now with a minimal valid argument"
        ),
        "{}",
        c[0]
    );
}

#[test]
fn server_only_uses_whole_server_id_and_prompt_replaces_template() {
    let (st, seen) = stub(reply(None), 200);
    let o = run(&st, &["mcp", "exec", "law", "do", "it"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let c = chats(&seen);
    let v: serde_json::Value = serde_json::from_str(&c[0]).unwrap();
    assert_eq!(v["tools"], serde_json::json!(["mcp:law"]));
    assert!(c[0].contains("do it") && !c[0].contains("Do not answer from memory"));
}

#[test]
fn verify_exit_codes() {
    let code = |r: String, args: &[&str]| {
        let (st, _) = stub(r, 200);
        run(&st, args).status.code()
    };
    let t = ["mcp", "exec", "law/get", "--verify"];
    let s = ["mcp", "exec", "law", "--verify"];
    assert_eq!(code(reply(Some(r#"[{"name":"law__get"}]"#)), &t), Some(0));
    assert_eq!(code(reply(Some(r#"[{"name":"get"}]"#)), &t), Some(0));
    assert_eq!(code(reply(Some(r#"[]"#)), &t), Some(1));
    assert_eq!(code(reply(Some(r#"[{"name":"law__other"}]"#)), &t), Some(1));
    assert_eq!(code(reply(None), &t), Some(2));
    assert_eq!(code(reply(Some(r#"[{"name":"law__x"}]"#)), &s), Some(0));
    assert_eq!(code(reply(Some(r#"[{"name":"web_search"}]"#)), &s), Some(1));
    assert_eq!(code(reply(Some(r#"[]"#)), &s), Some(1));
}

#[test]
fn preflight_failures_make_no_chat_request() {
    for (target, needle) in [
        ("nope", "was not found"),
        ("dis", "is disabled"),
        ("au", "needs authentication"),
        ("law/off", "is disabled"),
        ("law/zzz", "has no tool"),
    ] {
        let (st, seen) = stub(reply(None), 200);
        let o = run(&st, &["mcp", "exec", target]);
        assert_eq!(o.status.code(), Some(1), "{target}: {}", err(&o));
        assert!(err(&o).contains(needle), "{target}: {}", err(&o));
        assert!(chats(&seen).is_empty(), "{target}");
    }
}

#[test]
fn chat_error_body_is_not_echoed() {
    let (st, _) = stub(String::new(), 500);
    let o = run(&st, &["mcp", "exec", "law/get"]);
    assert_ne!(o.status.code(), Some(0));
    assert!(!err(&o).contains("SECRET-SERVER-BODY") && !out(&o).contains("SECRET-SERVER-BODY"));
}
