//! `monocle knowledge create|upload|list|delete` against a stateful stub jarvice
//! (binary e2e: HOME tempdir + credentials pointing at the stub). The stub also
//! records every request so tests can assert order and absence of calls.

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::jarvice_knowledge::collect_files;

/// (kb id, name, [(file id, file name)])
type Kb = (String, String, Vec<(String, String)>);

#[derive(Default)]
struct State {
    kbs: Vec<Kb>,
    /// every uploaded file: (id, name)
    files: Vec<(String, String)>,
    log: Vec<String>,
    /// kb id -> description (set by `/update`)
    descs: std::collections::HashMap<String, String>,
    /// last `/update` body
    last_update: Option<serde_json::Value>,
}

type Shared = Arc<Mutex<State>>;

fn kb_json(kb: &Kb, desc: &str) -> String {
    let files: Vec<String> =
        kb.2.iter()
            .map(|(id, n)| format!(r#"{{"id":"{id}","meta":{{"name":"{n}"}}}}"#))
            .collect();
    format!(
        r#"{{"id":"{}","name":"{}","description":"{}","access_control":{{"read":{{"user_ids":["u1"]}}}},"data":{{"file_ids":[]}},"files":[{}]}}"#,
        kb.0,
        kb.1,
        desc,
        files.join(",")
    )
}

/// Stub jarvice. File names with special meaning: `bad.pdf` uploads with a
/// 200 + `error` envelope (unparseable), `addfail.txt` is rejected by `file/add`,
/// `stuck.txt` cannot be deleted.
fn jarvice(state: &Shared) -> common::Stub {
    let st = state.clone();
    common::stub(move |_, method, url, body| {
        let mut s = st.lock().unwrap();
        s.log.push(format!("{method} {url}"));
        match (method, url) {
            ("GET", "/api/v1/knowledge/") | ("GET", "/api/v1/knowledge/list") => {
                let all: Vec<String> = s
                    .kbs
                    .iter()
                    .map(|k| kb_json(k, s.descs.get(&k.0).map_or("", |d| d.as_str())))
                    .collect();
                (200, format!("[{}]", all.join(",")))
            }
            ("POST", "/api/v1/knowledge/create") => {
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                let id = format!("kb{}", s.kbs.len() + 1);
                let name = v["name"].as_str().unwrap().to_string();
                s.kbs.push((id.clone(), name.clone(), vec![]));
                (
                    200,
                    format!(r#"{{"id":"{id}","name":"{name}","description":""}}"#),
                )
            }
            ("POST", "/api/v1/files/") => {
                let name = body
                    .split("filename=\"")
                    .nth(1)
                    .and_then(|r| r.split('"').next())
                    .unwrap_or("?")
                    .to_string();
                let id = format!("f{}", s.files.len() + 1);
                s.files.push((id.clone(), name.clone()));
                if name == "bad.pdf" {
                    (
                        200,
                        format!(
                            r#"{{"id":"{id}","error":{{"category":"extract.no_content","code":"no_text"}}}}"#
                        ),
                    )
                } else {
                    (200, format!(r#"{{"id":"{id}"}}"#))
                }
            }
            ("POST", u) if u.starts_with("/api/v1/knowledge/") && u.ends_with("/file/add") => {
                let kid = u.split('/').nth(4).unwrap().to_string();
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                let fid = v["file_id"].as_str().unwrap().to_string();
                let fname = s.files.iter().find(|f| f.0 == fid).unwrap().1.clone();
                if fname == "addfail.txt" {
                    return (
                        400,
                        r#"{"detail":{"code":"file_not_processed","secret":"x"}}"#.into(),
                    );
                }
                s.kbs
                    .iter_mut()
                    .find(|k| k.0 == kid)
                    .unwrap()
                    .2
                    .push((fid, fname));
                (200, "{}".into())
            }
            ("POST", u)
                if u.starts_with("/api/v1/knowledge/")
                    && u.ends_with("/update")
                    && !u.ends_with("/file/update") =>
            {
                let kid = u.split('/').nth(4).unwrap().to_string();
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                let k = s.kbs.iter_mut().find(|k| k.0 == kid).unwrap();
                k.1 = v["name"].as_str().unwrap().to_string();
                s.descs
                    .insert(kid, v["description"].as_str().unwrap().to_string());
                s.last_update = Some(v);
                (200, "{}".into())
            }
            ("POST", u) if u.starts_with("/api/v1/knowledge/") && u.ends_with("/file/remove") => {
                let kid = u.split('/').nth(4).unwrap().to_string();
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                let fid = v["file_id"].as_str().unwrap().to_string();
                s.kbs
                    .iter_mut()
                    .find(|k| k.0 == kid)
                    .unwrap()
                    .2
                    .retain(|f| f.0 != fid);
                s.files.retain(|f| f.0 != fid);
                (200, "{}".into())
            }
            ("POST", u) if u.starts_with("/api/v1/knowledge/") && u.ends_with("/file/update") => {
                (200, "{}".into())
            }
            ("POST", u) if u.starts_with("/api/v1/files/") && u.contains("/reprocess") => {
                if u.ends_with("force=true") {
                    (200, "{}".into())
                } else {
                    (409, r#"{"detail":"already processed"}"#.into())
                }
            }
            ("GET", u) if u.starts_with("/api/v1/files/") && u.ends_with("/data/content") => {
                let fid = u.split('/').nth(4).unwrap();
                match s.files.iter().find(|f| f.0 == fid) {
                    Some(f) => (200, format!(r#"{{"content":"extracted {}"}}"#, f.1)),
                    None => (404, "{}".into()),
                }
            }
            ("DELETE", u) if u.starts_with("/api/v1/files/") => {
                let fid = u.rsplit('/').next().unwrap().to_string();
                let name = s.files.iter().find(|f| f.0 == fid).map(|f| f.1.clone());
                if name.as_deref() == Some("stuck.txt") {
                    return (500, "boom".into());
                }
                s.files.retain(|f| f.0 != fid);
                (200, r#"{"message":"File deleted successfully"}"#.into())
            }
            ("DELETE", u) if u.starts_with("/api/v1/knowledge/") && u.ends_with("/delete") => {
                let kid = u.split('/').nth(4).unwrap().to_string();
                s.kbs.retain(|k| k.0 != kid);
                (200, "true".into())
            }
            _ => (404, "{}".into()),
        }
    })
}

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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn setup() -> (Shared, tempfile::TempDir, tempfile::TempDir) {
    let state = Shared::default();
    let stub = jarvice(&state);
    let home = logged_in_home(&stub);
    let docs = tempfile::tempdir().unwrap();
    (state, home, docs)
}

fn write(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), format!("content of {name}")).unwrap();
}

#[test]
fn create_prints_id_and_refuses_duplicate_name() {
    let (state, home, _docs) = setup();
    let o = run(home.path(), &["knowledge", "create", "kb-a"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "kb1\n");
    let dup = run(home.path(), &["knowledge", "create", "kb-a"]);
    assert_eq!(dup.status.code(), Some(1));
    assert!(
        err(&dup).contains("already exists (id kb1)"),
        "{}",
        err(&dup)
    );
    assert_eq!(state.lock().unwrap().kbs.len(), 1);
}

#[test]
fn upload_reports_per_file_failures_and_continues() {
    let (state, home, docs) = setup();
    for f in ["a.md", "b.txt", "bad.pdf", "addfail.txt", "ignored.png"] {
        write(docs.path(), f);
    }
    run(home.path(), &["knowledge", "create", "kb-a"]);
    let d = docs.path().to_str().unwrap();
    let o = run(home.path(), &["knowledge", "upload", "kb-a", d]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    // stdout: only the stored files, as `id<TAB>name`
    let stdout = out(&o);
    assert!(
        stdout.contains("\tb.txt") && stdout.contains("\ta.md"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("bad.pdf") && !stdout.contains("ignored"),
        "{stdout}"
    );
    let e = err(&o);
    assert!(
        e.contains("bad.pdf: FAILED") && e.contains("no_text"),
        "{e}"
    );
    assert!(
        e.contains("addfail.txt: FAILED") && e.contains("file_not_processed"),
        "{e}"
    );
    assert!(!e.contains("secret"), "server body echoed: {e}");
    assert!(e.contains("Uploaded 2, skipped 0, failed 2 (of 4)"), "{e}");
    // failures left no orphan files on the server
    let s = state.lock().unwrap();
    let mut names: Vec<_> = s.files.iter().map(|f| f.1.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a.md", "b.txt"]);
    assert_eq!(s.kbs[0].2.len(), 2);
}

#[test]
fn upload_is_idempotent_by_filename() {
    let (state, home, docs) = setup();
    write(docs.path(), "a.md");
    run(home.path(), &["knowledge", "create", "kb-a"]);
    let d = docs.path().to_str().unwrap();
    assert_eq!(
        run(home.path(), &["knowledge", "upload", "kb-a", d])
            .status
            .code(),
        Some(0)
    );
    let again = run(home.path(), &["knowledge", "upload", "kb-a", d]);
    assert_eq!(again.status.code(), Some(0), "{}", err(&again));
    assert!(err(&again).contains("skipped (already in collection)"));
    assert_eq!(state.lock().unwrap().files.len(), 1);
}

#[test]
fn upload_rejects_bad_input_before_any_request() {
    let (state, home, docs) = setup();
    write(docs.path(), "a.png");
    let png = docs.path().join("a.png");
    let missing = docs.path().join("nope.md");
    for arg in [png.to_str().unwrap(), missing.to_str().unwrap()] {
        let o = run(home.path(), &["knowledge", "upload", "kb-a", arg]);
        assert_eq!(o.status.code(), Some(1));
        assert!(out(&o).is_empty());
    }
    let empty = tempfile::tempdir().unwrap();
    let o = run(
        home.path(),
        &[
            "knowledge",
            "upload",
            "kb-a",
            empty.path().to_str().unwrap(),
        ],
    );
    assert!(err(&o).contains("No supported files"), "{}", err(&o));
    assert!(state.lock().unwrap().log.is_empty(), "requests were made");
}

#[test]
fn upload_to_unknown_collection_fails() {
    let (_state, home, docs) = setup();
    write(docs.path(), "a.md");
    let o = run(
        home.path(),
        &["knowledge", "upload", "nope", docs.path().to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("Collection 'nope' was not found"),
        "{}",
        err(&o)
    );
}

#[test]
fn list_shows_collections_then_files() {
    let (_state, home, docs) = setup();
    write(docs.path(), "a.md");
    run(home.path(), &["knowledge", "create", "kb-a"]);
    run(
        home.path(),
        &["knowledge", "upload", "kb-a", docs.path().to_str().unwrap()],
    );
    let all = run(home.path(), &["knowledge", "list"]);
    let t = out(&all);
    assert!(t.starts_with("COLLECTION ID"), "{t}");
    assert!(
        t.contains("kb1") && t.contains("kb-a") && t.trim_end().ends_with('1'),
        "{t}"
    );
    let one = run(home.path(), &["knowledge", "list", "kb-a"]);
    assert!(
        out(&one).contains("f1") && out(&one).contains("a.md"),
        "{}",
        out(&one)
    );
}

#[test]
fn delete_removes_files_then_collection() {
    let (state, home, docs) = setup();
    write(docs.path(), "a.md");
    write(docs.path(), "b.md");
    run(home.path(), &["knowledge", "create", "kb-a"]);
    run(
        home.path(),
        &["knowledge", "upload", "kb-a", docs.path().to_str().unwrap()],
    );
    let o = run(home.path(), &["knowledge", "delete", "kb-a"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("and 2 file(s)"), "{}", err(&o));
    let s = state.lock().unwrap();
    assert!(s.kbs.is_empty() && s.files.is_empty());
    let kb_delete = s
        .log
        .iter()
        .position(|l| l.starts_with("DELETE /api/v1/knowledge/"))
        .unwrap();
    let last_file_delete = s
        .log
        .iter()
        .rposition(|l| l.starts_with("DELETE /api/v1/files/"))
        .unwrap();
    assert!(
        last_file_delete < kb_delete,
        "files must go before the collection"
    );
}

#[test]
fn delete_keeps_collection_when_a_file_cannot_be_deleted() {
    let (state, home, docs) = setup();
    write(docs.path(), "stuck.txt");
    run(home.path(), &["knowledge", "create", "kb-a"]);
    run(
        home.path(),
        &["knowledge", "upload", "kb-a", docs.path().to_str().unwrap()],
    );
    let o = run(home.path(), &["knowledge", "delete", "kb-a"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("collection was kept"), "{}", err(&o));
    assert_eq!(state.lock().unwrap().kbs.len(), 1);
}

#[test]
fn ambiguous_name_is_an_error_listing_ids() {
    let (state, home, _docs) = setup();
    state.lock().unwrap().kbs = vec![
        ("k1".into(), "dup".into(), vec![]),
        ("k2".into(), "dup".into(), vec![]),
    ];
    let o = run(home.path(), &["knowledge", "delete", "dup"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("ambiguous") && err(&o).contains("k1, k2"),
        "{}",
        err(&o)
    );
    // addressing by id still works
    assert_eq!(
        run(home.path(), &["knowledge", "delete", "k2"])
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn not_logged_in_is_reported() {
    let empty = tempfile::tempdir().unwrap();
    let o = run(empty.path(), &["knowledge", "list"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("Not logged in"), "{}", err(&o));
}

#[test]
fn collect_files_filters_by_extension_case_insensitively() {
    let d = tempfile::tempdir().unwrap();
    for f in ["B.PDF", "a.docx", "c.Md", "d.txt", "e.csv"] {
        write(d.path(), f);
    }
    std::fs::create_dir(d.path().join("sub")).unwrap();
    write(&d.path().join("sub"), "nested.md");
    let got = collect_files(&[d.path().to_str().unwrap().to_string()]).unwrap();
    let names: Vec<_> = got
        .iter()
        .map(|f| f.path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["B.PDF", "a.docx", "c.Md", "d.txt"]);
    assert_eq!(got[0].content_type, "application/pdf");
}

// ── no automatic retry for non-idempotent POSTs ──────────────────────────────

/// A server that accepts connections and drops them without answering (the
/// "response lost" transport failure). Returns its base URL and the accept count.
fn dropping_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (url, accepts)
}

#[test]
fn create_and_upload_make_a_single_attempt_on_transport_failure() {
    use monocle_cli::jarvice_knowledge::{add_file, create_knowledge, upload_file, UploadFile};
    use monocle_cli::net::Client;
    use std::sync::atomic::Ordering;

    let docs = tempfile::tempdir().unwrap();
    write(docs.path(), "a.md");
    let file = UploadFile {
        path: docs.path().join("a.md"),
        content_type: "text/markdown",
    };

    let (url, accepts) = dropping_server();
    let client = Client::new();
    assert!(create_knowledge(&client, &url, "Bearer t", "kb").is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 1, "create was retried");
    assert!(upload_file(&client, &url, "Bearer t", "a.md", &file).is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 2, "upload was retried");
    assert!(add_file(&client, &url, "Bearer t", "kb1", "f1").is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 3, "file/add was retried");
}

#[test]
fn shared_post_still_retries_once() {
    // Guards the contrast: the single attempts above are the opt-out, not a
    // change to the default policy other commands rely on.
    use monocle_cli::net::Client;
    use std::sync::atomic::Ordering;

    let (url, accepts) = dropping_server();
    let body = serde_json::json!({});
    assert!(Client::new().post_json(&url, &[], &body).is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

fn seeded() -> (Shared, tempfile::TempDir, tempfile::TempDir) {
    let (state, home, docs) = setup();
    write(docs.path(), "a.md");
    write(docs.path(), "b.txt");
    run(home.path(), &["knowledge", "create", "kb-a"]);
    let d = docs.path().to_str().unwrap();
    run(home.path(), &["knowledge", "upload", "kb-a", d]);
    (state, home, docs)
}

#[test]
fn update_renames_and_keeps_access_control() {
    let (state, home, _docs) = seeded();
    let o = run(
        home.path(),
        &[
            "knowledge",
            "update",
            "kb-a",
            "--name",
            "kb-b",
            "--description",
            "d",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = state.lock().unwrap();
    let u = s.last_update.as_ref().expect("update was called");
    assert_eq!(u["name"], "kb-b");
    assert_eq!(u["description"], "d");
    assert_eq!(u["access_control"]["read"]["user_ids"][0], "u1");
    drop(s);
    assert!(out(&run(home.path(), &["knowledge", "list"])).contains("kb-b"));
}

#[test]
fn update_needs_a_change_and_a_free_name() {
    let (state, home, _docs) = seeded();
    run(home.path(), &["knowledge", "create", "kb-c"]);
    let none = run(home.path(), &["knowledge", "update", "kb-a"]);
    assert_eq!(none.status.code(), Some(1));
    let dup = run(
        home.path(),
        &["knowledge", "update", "kb-a", "--name", "kb-c"],
    );
    assert_eq!(dup.status.code(), Some(1));
    assert!(err(&dup).contains("already exists"), "{}", err(&dup));
    assert!(state.lock().unwrap().last_update.is_none());
}

#[test]
fn remove_detaches_one_file_and_keeps_collection() {
    let (state, home, _docs) = seeded();
    let o = run(home.path(), &["knowledge", "remove", "kb-a", "a.md"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = state.lock().unwrap();
    assert_eq!(s.kbs.len(), 1);
    let names: Vec<&str> = s.kbs[0].2.iter().map(|f| f.1.as_str()).collect();
    assert_eq!(names, ["b.txt"]);
    assert!(s.log.iter().any(|l| l.ends_with("/file/remove")));
    drop(s);
    let miss = run(home.path(), &["knowledge", "remove", "kb-a", "nope.md"]);
    assert_eq!(miss.status.code(), Some(1));
    assert!(err(&miss).contains("not in collection"), "{}", err(&miss));
}

#[test]
fn replace_swaps_the_same_named_file() {
    let (state, home, docs) = seeded();
    let old = state.lock().unwrap().kbs[0].2[0].0.clone();
    let p = docs.path().join("a.md");
    let o = run(
        home.path(),
        &["knowledge", "replace", "kb-a", p.to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let s = state.lock().unwrap();
    let files = &s.kbs[0].2;
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.0 != old), "old a.md still attached");
    assert_eq!(files.iter().filter(|f| f.1 == "a.md").count(), 1);
    drop(s);
    write(docs.path(), "new.md");
    let p = docs.path().join("new.md");
    let miss = run(
        home.path(),
        &["knowledge", "replace", "kb-a", p.to_str().unwrap()],
    );
    assert_eq!(miss.status.code(), Some(1));
}

#[test]
fn reprocess_requires_force_when_already_processed() {
    let (state, home, _docs) = seeded();
    let o = run(home.path(), &["knowledge", "reprocess", "kb-a", "a.md"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--force"), "{}", err(&o));
    let o = run(
        home.path(),
        &["knowledge", "reprocess", "kb-a", "a.md", "--force"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let log = state.lock().unwrap().log.join("\n");
    assert!(
        log.contains("/reprocess?force=true") && log.contains("/file/update"),
        "{log}"
    );
}

#[test]
fn content_prints_extracted_text_to_stdout() {
    let (_state, home, _docs) = seeded();
    let o = run(home.path(), &["knowledge", "content", "kb-a", "b.txt"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "extracted b.txt");
}

/// Stub for `ask`: one collection `kb1` ("kb-a"); `/api/chat/completions` answers
/// with SSE — a `sources` event first (unless the question says "ungrounded"),
/// then content deltas. Records the chat request body.
fn ask_stub(seen: &Arc<Mutex<Vec<serde_json::Value>>>) -> common::Stub {
    let seen = seen.clone();
    common::stub_with_headers(move |_, method, url, body| match (method, url) {
        ("GET", "/api/v1/knowledge/") => (
            200,
            r#"[{"id":"kb1","name":"kb-a","files":[]}]"#.into(),
            vec![],
        ),
        ("POST", "/api/chat/completions") => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let grounded = !v["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("ungrounded");
            seen.lock().unwrap().push(v);
            let sources = if grounded {
                "data: {\"sources\":[{\"source\":{\"type\":\"collection\",\"name\":\"kb-a\"},\"document\":[\"x\",\"y\",\"z\",\"q\"],\"metadata\":[{\"name\":\"a.md\"},{\"name\":\"a.md\"},{\"source\":\"b.txt\"},{\"name\":\"\"}]}]}\n\n"
            } else {
                ""
            };
            let sse = format!(
                "{sources}data: {{\"choices\":[{{\"delta\":{{\"content\":\"The \"}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{\"content\":\"answer\"}}}}]}}\n\ndata: [DONE]\n\n"
            );
            (
                200,
                sse,
                vec![("Content-Type", "text/event-stream".to_string())],
            )
        }
        _ => (404, "{}".into(), vec![]),
    })
}

#[test]
fn ask_attaches_the_collection_and_reports_answer_and_sources() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stub = ask_stub(&seen);
    let home = logged_in_home(&stub);
    let o = run(home.path(), &["knowledge", "ask", "kb-a", "what is x?"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "The answer\n");
    assert!(err(&o).contains("a.md (2 passage(s))"), "{}", err(&o));
    let body = seen.lock().unwrap()[0].clone();
    assert_eq!(body["stream"], true);
    assert_eq!(body["files"][0]["type"], "collection");
    assert_eq!(body["files"][0]["id"], "kb1");
    assert!(body.get("chat_id").is_none(), "must stay headless");
}

#[test]
fn ask_json_and_ungrounded_answers() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stub = ask_stub(&seen);
    let home = logged_in_home(&stub);
    let o = run(home.path(), &["knowledge", "ask", "kb-a", "q", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["answer"], "The answer");
    assert_eq!(v["sources"][0]["name"], "a.md");
    assert_eq!(v["sources"][1]["name"], "b.txt");
    assert!(
        v["sources"].as_array().unwrap().len() == 2,
        "collection name is not a file"
    );
    let o = run(home.path(), &["knowledge", "ask", "kb-a", "ungrounded?"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("Sources: none"), "{}", err(&o));
}

#[test]
fn ask_unknown_collection_sends_no_chat_turn() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stub = ask_stub(&seen);
    let home = logged_in_home(&stub);
    let o = run(home.path(), &["knowledge", "ask", "nope", "q"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(seen.lock().unwrap().is_empty());
}
