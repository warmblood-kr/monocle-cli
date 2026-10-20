//! Binary e2e for scope 1b: plain `monocle chat` is hosted Chat
//! (`POST /api/responses`), router-only flags are ignored with a hint (no
//! router fallback), `--responses` is a silent no-op, and `monocle router chat`
//! still goes to `/v1/chat/completions`. Same stub + HOME pattern as
//! `tests/chat_content.rs`.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use monocle_cli::credentials::{Credentials, CredentialsData};

const ANSWER: &str = "hosted answer";
const RESPONSES_REPLY: &str =
    r#"{"choices":[{"message":{"role":"assistant","content":"hosted answer"}}]}"#;
const COMPLETIONS_REPLY: &str = r#"{"choices":[{"message":{"role":"assistant","content":"router answer"},"finish_reason":"stop"}]}"#;

type Hits = Arc<Mutex<HashMap<String, usize>>>;

/// Counts hits per path; answers `/api/responses` (with `x-thread-id`) and
/// `/v1/chat/completions`, 404 for anything else.
fn counting_stub() -> (common::Stub, Hits) {
    let hits: Hits = Arc::default();
    let counter = hits.clone();
    let stub = common::stub_with_headers(move |_, method, url, _| {
        *counter.lock().unwrap().entry(url.to_string()).or_default() += 1;
        match (method, url) {
            ("POST", "/api/responses") => (
                200,
                RESPONSES_REPLY.into(),
                vec![("x-thread-id", "thread-1b".to_string())],
            ),
            ("POST", "/v1/chat/completions") => (200, COMPLETIONS_REPLY.into(), vec![]),
            _ => (404, String::new(), vec![]),
        }
    });
    (stub, hits)
}

fn hits(h: &Hits, path: &str) -> usize {
    h.lock().unwrap().get(path).copied().unwrap_or(0)
}

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

/// Run the binary with `q` piped on stdin (`echo q | monocle …`).
fn run_piped(home: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    child.wait_with_output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn hint(flag: &str) -> String {
    format!("Warning: {flag} ignored; use `monocle router chat` to keep the old behaviour")
}

fn warning_lines(o: &Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.contains("Warning:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn plain_chat_creates_hosted_thread() {
    let (stub, h) = counting_stub();
    let home = logged_in_home(&stub);
    let out = run_piped(home.path(), &["chat"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert!(
        stderr(&out).contains("Thread: thread-1b"),
        "{}",
        stderr(&out)
    );
    assert_eq!(hits(&h, "/api/responses"), 1);
    assert_eq!(hits(&h, "/v1/chat/completions"), 0);
}

#[test]
fn router_flags_print_hint_and_stay_hosted() {
    let (stub, h) = counting_stub();
    let home = logged_in_home(&stub);
    let out = run_piped(
        home.path(),
        &["chat", "--system-prompt", "s", "--max-tokens", "5"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        warning_lines(&out),
        vec![hint("--system-prompt"), hint("--max-tokens")]
    );
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert!(!stdout(&out).contains("Warning:"));
    assert_eq!(hits(&h, "/api/responses"), 1);
    assert_eq!(hits(&h, "/v1/chat/completions"), 0);

    // `--system-prompt-file` gets its own line (the file need not exist: ignored).
    let out = run_piped(home.path(), &["chat", "--system-prompt-file", "f.txt"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(warning_lines(&out), vec![hint("--system-prompt-file")]);
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert_eq!(hits(&h, "/api/responses"), 2);
    assert_eq!(hits(&h, "/v1/chat/completions"), 0);
}

#[test]
fn responses_flag_is_silent_noop() {
    let (stub, _) = counting_stub();
    let home = logged_in_home(&stub);
    let plain = run_piped(home.path(), &["chat"]);
    let flagged = run_piped(home.path(), &["chat", "--responses"]);
    assert_eq!(flagged.status.code(), Some(0), "{}", stderr(&flagged));
    assert_eq!(flagged.stdout, plain.stdout);
    assert!(
        !stderr(&flagged).contains("Warning:"),
        "{}",
        stderr(&flagged)
    );
}

#[test]
fn router_chat_still_uses_completions() {
    let (stub, h) = counting_stub();
    let home = logged_in_home(&stub);
    let out = run_piped(home.path(), &["router", "chat"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "router answer");
    assert_eq!(hits(&h, "/v1/chat/completions"), 1);
    assert_eq!(hits(&h, "/api/responses"), 0);
}

#[test]
fn root_help_chat_line() {
    let out = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        stdout(&out).contains(
            "chat     Chat with Monocle hosted Chat (server-side threads, visible in the web UI; REPL or pipe from stdin)"
        ),
        "{}",
        stdout(&out)
    );
}

// ── --show-tools ─────────────────────────────────────────────────────────────

fn tools_stub(body: &'static str) -> common::Stub {
    common::stub_with_headers(move |_, _, _, _| {
        (
            200,
            body.into(),
            vec![("x-thread-id", "thread-t".to_string())],
        )
    })
}

const WITH_TOOLS: &str = r#"{"choices":[{"message":{"role":"assistant","content":"hosted answer"}}],"tools_used":[{"name":"web_search"},{"name":"model_designer__delegate_model"}]}"#;
const EMPTY_TOOLS: &str =
    r#"{"choices":[{"message":{"role":"assistant","content":"hosted answer"}}],"tools_used":[]}"#;

#[test]
fn show_tools_prints_tool_names_to_stderr_only() {
    let stub = tools_stub(WITH_TOOLS);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--show-tools"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), format!("{ANSWER}\n"));
    assert!(
        stderr(&o).contains("Tools: web_search, model_designer__delegate_model"),
        "{}",
        stderr(&o)
    );
    // without the flag nothing is added
    let plain = run_piped(home.path(), &["chat"]);
    assert!(!stderr(&plain).contains("Tools:"), "{}", stderr(&plain));
}

#[test]
fn show_tools_distinguishes_none_from_unknown() {
    let none = tools_stub(EMPTY_TOOLS);
    let home = logged_in_home(&none);
    assert!(stderr(&run_piped(home.path(), &["chat", "--show-tools"])).contains("Tools: none"));

    let unknown = tools_stub(RESPONSES_REPLY);
    let home = logged_in_home(&unknown);
    assert!(stderr(&run_piped(home.path(), &["chat", "--show-tools"])).contains("Tools: unknown"));
}

#[test]
fn chat_timings_line_on_stderr_only() {
    let (stub, _) = counting_stub();
    let home = logged_in_home(&stub);
    let plain = run_piped(home.path(), &["chat"]);
    let timed = run_piped(home.path(), &["chat", "--timings"]);
    assert_eq!(timed.status.code(), Some(0), "{}", stderr(&timed));
    assert_eq!(stdout(&plain), stdout(&timed));
    assert!(!stderr(&plain).contains(r#"{"timings""#));
    let e = stderr(&timed);
    let l = e
        .lines()
        .find(|l| l.starts_with(r#"{"timings""#))
        .expect(&e);
    let t = &serde_json::from_str::<serde_json::Value>(l).unwrap()["timings"];
    assert_eq!(t["lane"], "chat");
    let n = |k: &str| {
        t[k].as_u64()
            .unwrap_or_else(|| panic!("{k} missing in {t}"))
    };
    assert!(n("ttfe_ms") <= n("ttft_ms") && n("ttft_ms") <= n("total_ms"));
    assert!(!l.contains(ANSWER) && !l.contains("Bearer"));
}

#[test]
fn chat_diagnostics_file_has_no_content() {
    let (stub, _) = counting_stub();
    let home = logged_in_home(&stub);
    let path = home.path().join("d.jsonl");
    let o = run_piped(
        home.path(),
        &["chat", "--diagnostics-file", path.to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains(ANSWER) && !raw.contains("Bearer"), "{raw}");
    let ev: Vec<serde_json::Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let names: Vec<_> = ev.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(names, ["first_event", "first_text", "done"]);
    assert_eq!(ev[0]["lane"], "chat");
}

#[test]
fn router_chat_timings_line_on_stderr_only() {
    let (stub, _) = counting_stub();
    let home = logged_in_home(&stub);
    let plain = run_piped(home.path(), &["router", "chat"]);
    let timed = run_piped(home.path(), &["router", "chat", "--timings"]);
    assert_eq!(timed.status.code(), Some(0), "{}", stderr(&timed));
    assert_eq!(stdout(&plain), stdout(&timed));
    assert!(!stderr(&plain).contains(r#"{"timings""#));
    let e = stderr(&timed);
    let l = e
        .lines()
        .find(|l| l.starts_with(r#"{"timings""#))
        .expect(&e);
    let t = &serde_json::from_str::<serde_json::Value>(l).unwrap()["timings"];
    assert_eq!(t["lane"], "router");
    let n = |k: &str| {
        t[k].as_u64()
            .unwrap_or_else(|| panic!("{k} missing in {t}"))
    };
    assert!(n("ttfe_ms") <= n("ttft_ms") && n("ttft_ms") <= n("total_ms"));
    assert!(!l.contains("router answer") && !l.contains("Bearer"));
}

// ── --stream (headless SSE) ──────────────────────────────────────────────────

fn sse(v: serde_json::Value) -> String {
    format!("data: {v}\n\n")
}

fn delta(t: &str) -> String {
    sse(serde_json::json!({"choices":[{"delta":{"content":t}}]}))
}

const DONE: &str = "data: [DONE]\n\n";

fn thread_hdr() -> Vec<(&'static str, String)> {
    vec![("x-thread-id", "thread-s".to_string())]
}

fn timings(o: &Output) -> serde_json::Value {
    let e = stderr(o);
    let l = e
        .lines()
        .find(|l| l.starts_with(r#"{"timings""#))
        .expect(&e);
    serde_json::from_str::<serde_json::Value>(l).unwrap()["timings"].clone()
}

#[test]
fn stream_prints_chunks_and_omits_session_header() {
    let chunks = vec![
        (0, delta("Hel")),
        (0, delta("lo ")),
        (0, delta("world")),
        (0, sse(serde_json::json!({"tools_used": []}))),
        (0, DONE.to_string()),
    ];
    let (stub, reqs) = common::stub_sse_chunked(200, thread_hdr(), chunks);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "Hello world\n");
    assert!(stderr(&o).contains("Thread: thread-s"), "{}", stderr(&o));
    let r = reqs.lock().unwrap()[0].to_lowercase();
    assert!(!r.contains("x-session-id"), "{r}");
    let body: serde_json::Value =
        serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["stream"], true);
}

#[test]
fn non_stream_default_still_sends_session_header_and_stream_false() {
    let (stub, reqs) =
        common::stub_sse_chunked(200, thread_hdr(), vec![(0, RESPONSES_REPLY.to_string())]);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), format!("{ANSWER}\n"));
    let r = reqs.lock().unwrap()[0].to_lowercase();
    assert!(r.contains("x-session-id"), "{r}");
    let body: serde_json::Value =
        serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["stream"], false);
}

#[test]
fn stream_timings_order_with_sources_first_and_reads_to_eof() {
    let chunks = vec![
        (0, sse(serde_json::json!({"sources": []}))),
        (250, delta("hi")),
        (0, DONE.to_string()),
        (300, ": trailing\n\n".to_string()),
    ];
    let (stub, _) = common::stub_sse_chunked(200, thread_hdr(), chunks);
    let home = logged_in_home(&stub);
    let path = home.path().join("d.jsonl");
    let o = run_piped(
        home.path(),
        &[
            "chat",
            "--stream",
            "--timings",
            "--diagnostics-file",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let t = timings(&o);
    let n = |k: &str| t[k].as_u64().unwrap();
    assert!(n("ttfe_ms") < 200, "{t}");
    assert!(n("ttft_ms") >= 200 && n("ttft_ms") >= n("ttfe_ms"), "{t}");
    // total includes the bytes after [DONE]: the body was read to EOF.
    assert!(n("total_ms") >= n("ttft_ms") + 250, "{t}");
    let raw = std::fs::read_to_string(&path).unwrap();
    let names: Vec<String> = raw
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["first_event", "first_text", "done"]);
}

#[test]
fn stream_redacts_token_split_across_chunks() {
    // The test login's access token is "tok".
    let chunks = vec![
        (0, delta("a to")),
        (0, delta("k b")),
        (0, delta(" end to")),
        (0, DONE.to_string()),
    ];
    let (stub, _) = common::stub_sse_chunked(200, thread_hdr(), chunks);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "a [REDACTED] b end [REDACTED]\n");
}

#[test]
fn stream_mid_stream_error_exits_1_without_server_text() {
    let chunks = vec![
        (0, delta("part")),
        (
            0,
            sse(serde_json::json!({"error": {"content": "SECRET-SERVER-STRING"}})),
        ),
        (0, DONE.to_string()),
    ];
    let (stub, _) = common::stub_sse_chunked(200, thread_hdr(), chunks);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        !stderr(&o).contains("SECRET-SERVER-STRING"),
        "{}",
        stderr(&o)
    );
    assert!(!stdout(&o).contains("SECRET-SERVER-STRING"));
    assert!(stderr(&o).contains("HTTP 200"), "{}", stderr(&o));
}

#[test]
fn stream_pre_stream_http_error_shows_status_and_safe_code_only() {
    let (stub, _) = common::stub_sse_chunked(
        400,
        vec![],
        vec![(
            0,
            r#"{"detail":{"code":"bad_model","message":"LEAKY TEXT"}}"#.to_string(),
        )],
    );
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("HTTP 400 (bad_model)"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&o).contains("LEAKY TEXT"));
    assert_eq!(stdout(&o), "");
}

#[test]
fn stream_show_tools_and_verify_tool_firing_accumulate_rounds() {
    let round = |name: &str| {
        sse(
            serde_json::json!({"tools_used": [{"id":"c","name":name,"arguments":"{}","result":""}]}),
        )
    };
    let chunks = vec![
        (0, round("web_search")),
        (0, delta("ok")),
        (0, round("law__law_get")),
        (0, DONE.to_string()),
    ];
    let (stub, _) = common::stub_sse_chunked(200, thread_hdr(), chunks.clone());
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream", "--show-tools"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ok\n");
    assert!(
        stderr(&o).contains("Tools: web_search, law__law_get\n"),
        "{}",
        stderr(&o)
    );

    let ok = run_piped(
        home.path(),
        &["chat", "--stream", "--verify-tool-firing=law__law_get"],
    );
    assert_eq!(ok.status.code(), Some(0), "{}", stderr(&ok));
    let bad = run_piped(
        home.path(),
        &["chat", "--stream", "--verify-tool-firing=nope"],
    );
    assert_eq!(bad.status.code(), Some(1), "{}", stderr(&bad));
}

#[test]
fn stream_with_no_tool_round_fails_verification() {
    let chunks = vec![
        (0, delta("x")),
        (0, sse(serde_json::json!({"tools_used": []}))),
        (0, DONE.to_string()),
    ];
    let (stub, _) = common::stub_sse_chunked(200, thread_hdr(), chunks);
    let home = logged_in_home(&stub);
    let o = run_piped(home.path(), &["chat", "--stream", "--verify-tool-firing"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
}
