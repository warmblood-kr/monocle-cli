//! `monocle workspace-model list|show|create|update|toggle|delete` against a
//! stateful stub jarvice (binary e2e: HOME tempdir + credentials pointing at the
//! stub). Every test asserts the request the stub saw, so removing a call fails it.

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};
use serde_json::{json, Value};

#[derive(Default)]
struct State {
    models: Vec<Value>,
    /// (method, url, body) of every request
    log: Vec<(String, String, String)>,
    /// writes answer 401 like jarvice does without `workspace.models`
    deny_writes: bool,
}

type Shared = Arc<Mutex<State>>;

fn query_id(url: &str) -> String {
    url.split("?id=").nth(1).unwrap_or("").replace("%3A", ":")
}

fn jarvice(state: &Shared) -> common::Stub {
    let st = state.clone();
    common::stub(move |_, method, url, body| {
        let mut s = st.lock().unwrap();
        s.log.push((method.into(), url.into(), body.into()));
        let write = method != "GET";
        if write && s.deny_writes && url.starts_with("/api/v1/models/") {
            return (401, r#"{"detail":"Unauthorized: secret-text"}"#.into());
        }
        let id = query_id(url);
        let pos = s.models.iter().position(|m| m["id"] == id);
        match (method, url.split('?').next().unwrap()) {
            ("GET", "/api/v1/models/") => (200, Value::Array(s.models.clone()).to_string()),
            ("GET", "/api/v1/knowledge/") => {
                (200, r#"[{"id":"kb1","name":"fixtures","files":[]}]"#.into())
            }
            ("GET", "/api/v1/models/model") => match pos {
                Some(i) => (200, s.models[i].to_string()),
                None => (401, r#"{"detail":"Not found"}"#.into()),
            },
            ("POST", "/api/v1/models/create") => {
                let mut m: Value = serde_json::from_str(body).unwrap();
                m["user_id"] = json!("u1");
                m["created_at"] = json!(1);
                s.models.push(m.clone());
                (200, m.to_string())
            }
            ("POST", "/api/v1/models/model/update") => {
                let i = pos.unwrap();
                let mut m: Value = serde_json::from_str(body).unwrap();
                m["user_id"] = json!("u1");
                s.models[i] = m.clone();
                (200, m.to_string())
            }
            ("POST", "/api/v1/models/model/toggle") => {
                let i = pos.unwrap();
                let on = s.models[i]["is_active"].as_bool().unwrap();
                s.models[i]["is_active"] = json!(!on);
                (200, s.models[i].to_string())
            }
            ("DELETE", "/api/v1/models/model/delete") => {
                s.models.remove(pos.unwrap());
                (200, "true".into())
            }
            _ => (404, "{}".into()),
        }
    })
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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn setup() -> (Shared, tempfile::TempDir) {
    let state = Shared::default();
    let stub = jarvice(&state);
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
    (state, home)
}

fn seed(state: &Shared, id: &str, active: bool) {
    state.lock().unwrap().models.push(json!({
        "id": id, "user_id": "u1", "name": "Seeded", "base_model_id": "gpt-x",
        "meta": {"description": "keep me", "toolIds": ["mcp:old"], "custom": 1},
        "params": {"system": "old prompt", "temperature": 0.2},
        "access_control": null, "is_active": active,
        "created_at": 1, "updated_at": 1
    }));
}

/// The last request to `method url-prefix`, body parsed.
fn last_body(state: &Shared, method: &str, prefix: &str) -> Value {
    let s = state.lock().unwrap();
    let (_, _, b) = s
        .log
        .iter()
        .rev()
        .find(|(m, u, _)| m == method && u.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {method} {prefix} request"));
    serde_json::from_str(b).unwrap()
}

fn saw(state: &Shared, method: &str, prefix: &str) -> usize {
    let s = state.lock().unwrap();
    s.log
        .iter()
        .filter(|(m, u, _)| m == method && u.starts_with(prefix))
        .count()
}

#[test]
fn list_prints_table_and_show_prints_json() {
    let (state, home) = setup();
    seed(&state, "m1", false);
    let o = run(home.path(), &["workspace-model", "list"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("MODEL ID") && t.contains("m1") && t.contains("gpt-x"),
        "{t}"
    );
    assert!(t.contains("no"), "inactive shown: {t}");

    let o = run(home.path(), &["workspace-model", "show", "m1"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["params"]["system"], "old prompt");
    assert_eq!(saw(&state, "GET", "/api/v1/models/model?id=m1"), 1);
}

#[test]
fn create_from_flags_sends_full_private_form_and_prints_id() {
    let (state, home) = setup();
    let o = run(
        home.path(),
        &[
            "workspace-model",
            "create",
            "e2e-1",
            "--base",
            "gpt-x",
            "--system",
            "Be terse.",
            "--tool",
            "mcp:law",
            "--tool",
            "mcp:docs",
            "--knowledge",
            "fixtures",
            "--description",
            "e2e",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "e2e-1\n");
    let b = last_body(&state, "POST", "/api/v1/models/create");
    assert_eq!(b["id"], "e2e-1");
    assert_eq!(b["name"], "e2e-1");
    assert_eq!(b["base_model_id"], "gpt-x");
    assert_eq!(b["params"]["system"], "Be terse.");
    assert_eq!(b["meta"]["toolIds"], json!(["mcp:law", "mcp:docs"]));
    assert_eq!(
        b["meta"]["knowledge"],
        json!([{"id": "kb1", "name": "fixtures", "type": "collection"}])
    );
    assert_eq!(b["access_control"], json!({}));
    assert_eq!(b["is_active"], true);
}

#[test]
fn create_from_file_lets_flags_override() {
    let (state, home) = setup();
    let f = home.path().join("m.json");
    std::fs::write(
        &f,
        json!({"base_model_id": "from-file", "name": "N", "params": {"system": "file", "top_p": 1}})
            .to_string(),
    )
    .unwrap();
    let o = run(
        home.path(),
        &[
            "workspace-model",
            "create",
            "e2e-2",
            "--file",
            f.to_str().unwrap(),
            "--system",
            "flag",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let b = last_body(&state, "POST", "/api/v1/models/create");
    assert_eq!(b["base_model_id"], "from-file");
    assert_eq!(b["name"], "N");
    assert_eq!(b["params"]["system"], "flag");
    assert_eq!(b["params"]["top_p"], 1);
}

#[test]
fn create_rejects_duplicate_missing_base_and_unknown_collection_without_posting() {
    let (state, home) = setup();
    seed(&state, "m1", true);
    let dup = run(
        home.path(),
        &["workspace-model", "create", "m1", "--base", "b"],
    );
    assert_eq!(dup.status.code(), Some(1));
    assert!(err(&dup).contains("already exists"), "{}", err(&dup));
    let nobase = run(home.path(), &["workspace-model", "create", "n"]);
    assert!(err(&nobase).contains("--base"), "{}", err(&nobase));
    let nokb = run(
        home.path(),
        &[
            "workspace-model",
            "create",
            "n",
            "--base",
            "b",
            "--knowledge",
            "nope",
        ],
    );
    assert!(
        err(&nokb).contains("'nope' was not found"),
        "{}",
        err(&nokb)
    );
    assert_eq!(saw(&state, "POST", "/api/v1/models/create"), 0);
}

#[test]
fn update_changes_only_given_fields() {
    let (state, home) = setup();
    seed(&state, "m1", true);
    let o = run(
        home.path(),
        &[
            "workspace-model",
            "update",
            "m1",
            "--system",
            "new prompt",
            "--tool",
            "mcp:law",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let b = last_body(&state, "POST", "/api/v1/models/model/update?id=m1");
    assert_eq!(b["params"]["system"], "new prompt");
    assert_eq!(b["params"]["temperature"], 0.2, "unrelated params kept");
    assert_eq!(b["meta"]["toolIds"], json!(["mcp:law"]));
    assert_eq!(b["meta"]["custom"], 1, "unknown meta kept");
    assert_eq!(b["meta"]["description"], "keep me");
    assert_eq!(b["name"], "Seeded");
    assert_eq!(b["base_model_id"], "gpt-x");
    assert!(b.get("user_id").is_none() && b.get("created_at").is_none());
    assert_eq!(b["access_control"], Value::Null);
}

#[test]
fn toggle_and_delete_hit_their_endpoints() {
    let (state, home) = setup();
    seed(&state, "m1", true);
    let o = run(home.path(), &["workspace-model", "toggle", "m1"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("inactive"), "{}", err(&o));
    assert_eq!(saw(&state, "POST", "/api/v1/models/model/toggle?id=m1"), 1);
    assert_eq!(state.lock().unwrap().models[0]["is_active"], false);

    let o = run(home.path(), &["workspace-model", "delete", "m1"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        saw(&state, "DELETE", "/api/v1/models/model/delete?id=m1"),
        1
    );
    assert!(state.lock().unwrap().models.is_empty());
}

#[test]
fn unknown_id_is_reported_client_side() {
    let (state, home) = setup();
    for cmd in ["show", "toggle", "delete"] {
        let o = run(home.path(), &["workspace-model", cmd, "ghost"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            err(&o).contains("'ghost' was not found"),
            "{cmd}: {}",
            err(&o)
        );
    }
    assert_eq!(saw(&state, "DELETE", "/api/v1/models/"), 0);
}

#[test]
fn permission_denied_explains_workspace_models_and_hides_body() {
    let (state, home) = setup();
    seed(&state, "m1", true);
    state.lock().unwrap().deny_writes = true;
    for args in [
        vec!["workspace-model", "create", "n", "--base", "b"],
        vec!["workspace-model", "update", "m1", "--name", "x"],
        vec!["workspace-model", "toggle", "m1"],
        vec!["workspace-model", "delete", "m1"],
    ] {
        let o = run(home.path(), &args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let e = err(&o);
        assert!(e.contains("`workspace.models` permission"), "{args:?}: {e}");
        assert!(!e.contains("secret-text"), "body echoed: {e}");
    }
}

/// Writes may have taken effect before the response was lost: never replay them.
#[test]
fn writes_make_a_single_attempt_on_transport_failure() {
    use monocle_cli::jarvice_models as m;
    use monocle_cli::net::Client;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let c = Client::new();
    let form = json!({"id": "x"});
    assert!(m::create_model(&c, &base, "t", &form).is_err());
    assert!(m::update_model(&c, &base, "t", "x", &form).is_err());
    assert!(m::toggle_model(&c, &base, "t", "x").is_err());
    assert!(m::delete_model(&c, &base, "t", "x").is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 4, "a write was replayed");
}
