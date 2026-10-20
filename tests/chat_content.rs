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
    fetch_content(
        &Client::new(),
        &stub.router_url(),
        "Bearer tok",
        id,
        format,
        false,
        false,
    )
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

#[test]
fn invalid_chat_ids_are_rejected_before_any_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let stub = common::stub(move |_, _, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        (200, FIXTURE.into())
    });
    let home = logged_in_home(&stub);
    for bad in ["../users/me", "%2e%2e/users/me", "c1?x=1", "c1#frag"] {
        let out = run(home.path(), &["chat", "content", bad]);
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(out.stdout.is_empty(), "{bad}");
        let err = stderr(&out);
        assert!(
            err.contains("Error: Invalid chat thread ID."),
            "{bad}: {err}"
        );
        assert!(!err.contains(bad), "raw id echoed: {err}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "stub received requests");
}

// ── --show-tools ─────────────────────────────────────────────────────────────

/// Shape jarvice really stores: attribute values HTML-escaped, `arguments` and
/// `result` a JSON string that itself holds JSON.
const TOOL_BLOCKS: &str = r#"Looking it up.
<details type="tool_calls" done="true" id="call_1" name="web_search" arguments="&quot;{\&quot;query\&quot;:\&quot;a &amp; b\&quot;}&quot;" result="&quot;{}&quot;" files="">
<summary>Tool Executed</summary>
</details>
<details type="tool_calls" done="true" id="call_2" name="model_designer__delegate_model" arguments="&quot;{\&quot;model_id\&quot;:\&quot;law-search\&quot;,\&quot;task\&quot;:\&quot;find it\&quot;}&quot;" result="&quot;{\&quot;report_to_parent\&quot;:\&quot;found 3 items\&quot;}&quot;" files="">
<summary>Tool Executed</summary>
</details>
Done."#;

fn tools_stub() -> common::Stub {
    let chat = serde_json::json!({
        "id": "c1", "title": "T",
        "chat": {"history": {"currentId": "m2", "messages": {
            "m1": {"id": "m1", "parentId": null, "role": "user", "content": "hi"},
            "m2": {"id": "m2", "parentId": "m1", "role": "assistant", "content": TOOL_BLOCKS},
        }}}
    })
    .to_string();
    common::stub(move |_, method, url, _| match (method, url) {
        ("GET", "/api/v1/chats/c1") => (200, chat.clone()),
        _ => (401, NOT_FOUND_BODY.into()),
    })
}

#[test]
fn show_tools_summarizes_tool_calls_in_text() {
    let stub = tools_stub();
    let out = fetch_content(
        &Client::new(),
        &stub.router_url(),
        "Bearer tok",
        "c1",
        ContentFormat::Text,
        true,
        false,
    )
    .unwrap();
    assert!(!out.contains("<details"), "{out}");
    assert!(
        out.contains("Looking it up.\n[tool] web_search args={\"query\":\"a & b\"}\n"),
        "{out}"
    );
    assert!(
        out.contains("[tool] model_designer__delegate_model model_id=law-search report_to_parent=found 3 items args="),
        "{out}"
    );
    assert!(out.trim_end().ends_with("Done."), "{out}");
}

#[test]
fn default_output_is_unchanged_without_show_tools() {
    let stub = tools_stub();
    let out = fetch_content(
        &Client::new(),
        &stub.router_url(),
        "Bearer tok",
        "c1",
        ContentFormat::Text,
        false,
        false,
    )
    .unwrap();
    assert!(out.contains("<details type=\"tool_calls\""), "{out}");
    assert!(!out.contains("[tool]"), "{out}");
}

#[test]
fn show_tools_json_has_structured_tool_calls() {
    let stub = tools_stub();
    let out = fetch_content(
        &Client::new(),
        &stub.router_url(),
        "Bearer tok",
        "c1",
        ContentFormat::Json,
        true,
        false,
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let calls = &v["messages"][1]["tool_calls"];
    assert_eq!(calls[0]["name"], "web_search");
    assert_eq!(calls[0]["model_id"], serde_json::Value::Null);
    assert_eq!(calls[0]["truncated"], false);
    assert_eq!(calls[0]["parse_error"], serde_json::Value::Null);
    assert_eq!(calls[1]["name"], "model_designer__delegate_model");
    assert_eq!(calls[1]["model_id"], "law-search");
    assert_eq!(calls[1]["report_to_parent"], "found 3 items");
    // user message has none
    assert_eq!(v["messages"][0]["tool_calls"], serde_json::json!([]));
}

#[test]
fn chat_content_show_tools_binary_e2e() {
    let stub = tools_stub();
    let home = logged_in_home(&stub);
    let o = run(home.path(), &["chat", "content", "c1", "--show-tools"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(String::from_utf8_lossy(&o.stdout).contains("[tool] web_search"));
}

// ── <user_memory> ────────────────────────────────────────────────────────────

const MEMORY_FIXTURE: &str = r#"{"id":"c1","title":"T","chat":{"history":{"currentId":"m2","messages":{"m1":{"id":"m1","parentId":null,"role":"user","content":"<past_conversations>\nPERSONAL-PAST-SNIPPET\n</past_conversations>\n<user_memory>\n1. [2026-01-01]. PERSONAL-FACT-ONE\n2. PERSONAL-FACT-TWO\n</user_memory>\n\nhi"},"m2":{"id":"m2","parentId":"m1","role":"assistant","content":"hello"}}}}}"#;

fn memory_stub() -> common::Stub {
    common::stub(|_, method, url, _| match (method, url) {
        ("GET", "/api/v1/chats/c1") => (200, MEMORY_FIXTURE.into()),
        _ => (401, NOT_FOUND_BODY.into()),
    })
}

#[test]
fn chat_content_strips_user_memory_by_default_and_include_memory_restores_it() {
    let stub = memory_stub();
    let home = logged_in_home(&stub);

    for args in [
        &["chat", "content", "c1"][..],
        &["chat", "content", "c1", "--format", "json"][..],
    ] {
        let out = run(home.path(), args);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains("PERSONAL-FACT"), "{args:?}: {text}");
        assert!(!text.contains("user_memory"), "{args:?}: {text}");
        assert!(!text.contains("past_conversations"), "{args:?}: {text}");
        assert!(!text.contains("PERSONAL-PAST"), "{args:?}: {text}");
        assert!(text.contains("hi") && text.contains("hello"), "{text}");
    }

    let out = run(home.path(), &["chat", "content", "c1", "--include-memory"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("PERSONAL-FACT-ONE") && text.contains("</user_memory>"),
        "{text}"
    );
}
