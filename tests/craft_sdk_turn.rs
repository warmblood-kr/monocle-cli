//! `monocle craft --lane sdk turn`: one loopback server speaks the jarvice REST
//! routes and the Socket.IO WebSocket (engine.io v4, namespace `/agent`).

#![allow(clippy::result_large_err)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use monocle_cli::credentials::{Credentials, CredentialsData};
use tungstenite::{Message, WebSocket};

const ACCESS: &str = "STARK-ACCESS-TOK";
const SOCKTOK: &str = "FAKE-SOCKET-TOKEN.xyz";

type Log = Arc<Mutex<Vec<String>>>;
type Sock = WebSocket<TcpStream>;
type Script = Arc<dyn Fn(&mut Sock, &Log) + Send + Sync>;

fn peek_head(s: &TcpStream) -> String {
    let mut buf = [0u8; 4096];
    loop {
        let n = s.peek(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        if n == 0 || head.contains("\r\n\r\n") {
            return head;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn serve_rest(mut s: TcpStream, log: &Log, stall: Option<&str>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let key = line
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let (mut len, mut auth) = (0, String::new());
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        let low = h.to_ascii_lowercase();
        if let Some(v) = low.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if low.starts_with("authorization:") {
            auth = h.split_once(':').unwrap().1.trim().to_string();
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    log.lock().unwrap().push(format!(
        "{key} auth={auth} body={}",
        String::from_utf8_lossy(&body)
    ));
    if stall == Some(key.as_str()) {
        thread::sleep(Duration::from_secs(10));
    }
    let (status, resp) = match key.as_str() {
        "POST /api/v1/agent/sessions" => (200, r#"{"id":"sess-1","title":"t"}"#.to_string()),
        "GET /api/v1/auths/" => (200, format!(r#"{{"token":"{SOCKTOK}","role":"admin"}}"#)),
        _ => (404, "{}".to_string()),
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
        resp.len()
    );
}

fn start(script: Script) -> (String, Log) {
    start_with(script, None)
}

/// `stall`: a `METHOD /path` whose response is delayed 10s.
fn start_with(script: Script, stall: Option<&'static str>) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let log = Log::default();
    let l = log.clone();
    thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (log, script) = (l.clone(), script.clone());
            thread::spawn(move || {
                if peek_head(&s)
                    .to_ascii_lowercase()
                    .contains("upgrade: websocket")
                {
                    let cb =
                        |req: &tungstenite::handshake::server::Request,
                         resp: tungstenite::handshake::server::Response| {
                            log.lock()
                                .unwrap()
                                .push(format!("WS connect {}", req.uri()));
                            Ok(resp)
                        };
                    if let Ok(mut sock) = tungstenite::accept_hdr(s, cb) {
                        script(&mut sock, &log);
                    }
                } else {
                    serve_rest(s, &log, stall);
                }
            });
        }
    });
    (addr, log)
}

fn send(s: &mut Sock, v: &str) {
    s.send(Message::text(v)).unwrap();
}

fn ev(s: &mut Sock, name: &str, data: &str) {
    send(s, &format!(r#"42/agent,["{name}",{data}]"#));
}

fn recv(s: &mut Sock, log: &Log) -> Option<String> {
    let Ok(Message::Text(t)) = s.read() else {
        return None;
    };
    log.lock().unwrap().push(format!("WS recv {t}"));
    Some(t.as_str().to_string())
}

const OPEN: &str = r#"0{"sid":"e1","upgrades":[],"pingInterval":25000,"pingTimeout":20000}"#;

/// Engine open, auth check, namespace ack.
fn handshake(s: &mut Sock, log: &Log) {
    send(s, OPEN);
    recv(s, log);
    send(s, r#"40/agent,{"sid":"n1"}"#);
}

fn status(s: &mut Sock, st: &str) {
    ev(
        s,
        "session_status",
        &format!(r#"{{"session_id":"sess-1","status":"{st}"}}"#),
    );
}

fn delta(s: &mut Sock, text: &str) {
    ev(
        s,
        "text_delta",
        &format!(r#"{{"session_id":"sess-1","content":"{text}"}}"#),
    );
}

fn happy() -> Script {
    Arc::new(|s, log| {
        handshake(s, log);
        send(s, "2"); // engine ping mid-handshake-aftermath
        recv(s, log); // start_session
        recv(s, log); // expects "3"
        status(s, "connecting");
        status(s, "active");
        recv(s, log); // message
        ev(
            s,
            "thinking",
            r#"{"session_id":"sess-1","content":"THINKING"}"#,
        );
        delta(s, "Hel");
        delta(s, "lo");
        ev(s, "text", r#"{"session_id":"sess-1","content":"Hello"}"#);
        ev(
            s,
            "tool_use",
            r#"{"session_id":"sess-1","tool_name":"Bash","tool_input":{"cmd":"ls"}}"#,
        );
        ev(
            s,
            "tool_result",
            r#"{"session_id":"sess-1","content":"BIGRESULT"}"#,
        );
        ev(
            s,
            "text_delta",
            r#"{"session_id":"other","content":"WRONG"}"#,
        );
        // ack-id form of an event
        send(
            s,
            r#"42/agent,7["result",{"session_id":"sess-1","success":true,"cost":0.1}]"#,
        );
        let _ = s.close(None);
    })
}

fn setup(tenant: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: tenant.into(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: ACCESS.into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: None,
        })
        .unwrap();
    home
}

fn turn(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(["craft", "--lane", "sdk", "turn"])
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
fn lines(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}
/// The server thread logs asynchronously; wait for a line.
fn wait_for(log: &Log, needle: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if lines(log).iter().any(|l| l.contains(needle)) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}
fn no_secret(o: &Output) {
    for t in [out(o), err(o)] {
        assert!(!t.contains(SOCKTOK) && !t.contains(ACCESS), "leak in {t}");
    }
}

#[test]
fn streams_deltas_authenticates_with_socket_token_and_never_leaks_it() {
    let (addr, log) = start(happy());
    let home = setup(&addr);
    let o = turn(home.path(), &["hi there"]);
    assert!(o.status.success(), "{}", err(&o));
    // Deltas live; the whole `text` is not repeated; thinking/other session ignored.
    assert_eq!(out(&o), "Hello\n");
    let e = err(&o);
    assert!(e.contains("Session: sess-1"), "{e}");
    assert!(e.contains("[session_status] connecting"), "{e}");
    assert!(e.contains("[tool_use] Bash"), "{e}");
    assert!(!e.contains("BIGRESULT"), "{e}");
    no_secret(&o);
    let l = lines(&log);
    assert!(
        l.iter()
            .any(|x| x.starts_with("WS connect /ws/socket.io/?EIO=4&transport=websocket")),
        "{l:?}"
    );
    let expect = [
        format!(r#"WS recv 40/agent,{{"token":"{SOCKTOK}"}}"#),
        r#"WS recv 42/agent,["start_session",{"session_id":"sess-1"}]"#.to_string(),
        "WS recv 3".to_string(), // pong to the engine ping
        r#"WS recv 42/agent,["message",{"session_id":"sess-1","content":"hi there"}]"#.to_string(),
    ];
    let got: Vec<&String> = l.iter().filter(|x| x.starts_with("WS recv")).collect();
    assert_eq!(got, expect.iter().collect::<Vec<_>>(), "{l:?}");
}

#[test]
fn creates_session_with_title_and_fetches_token_with_bearer() {
    let (addr, log) = start(happy());
    let home = setup(&addr);
    let o = turn(home.path(), &["x", "--title", "My title"]);
    assert!(o.status.success(), "{}", err(&o));
    let l = lines(&log);
    let bearer = format!("auth=Bearer {ACCESS}");
    assert!(
        l.iter()
            .any(|x| x.starts_with("POST /api/v1/agent/sessions")
                && x.contains(&bearer)
                && x.ends_with(r#"body={"title":"My title"}"#)),
        "{l:?}"
    );
    assert!(
        l.iter()
            .any(|x| x.starts_with("GET /api/v1/auths/ ") && x.contains(&bearer)),
        "{l:?}"
    );
}

#[test]
fn default_title_and_existing_session_skips_creation() {
    let (addr, log) = start(happy());
    let home = setup(&addr);
    assert!(turn(home.path(), &["x"]).status.success());
    assert!(lines(&log)
        .iter()
        .any(|x| x.ends_with(r#"body={"title":"CLI turn"}"#)));
    let (addr, log) = start(happy());
    let home = setup(&addr);
    // The stub's events carry session_id sess-1, so reuse that id.
    let o = turn(home.path(), &["x", "--session", "sess-1"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(!lines(&log)
        .iter()
        .any(|x| x.starts_with("POST /api/v1/agent/sessions")));
}

#[test]
fn connect_error_fails_without_echoing_its_body() {
    let (addr, _) = start(Arc::new(|s, log| {
        send(s, OPEN);
        recv(s, log);
        send(s, r#"44/agent,{"message":"SECRET-DETAIL"}"#);
    }));
    let home = setup(&addr);
    let o = turn(home.path(), &["x"]);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("authentication was refused"),
        "{}",
        err(&o)
    );
    assert!(!err(&o).contains("SECRET-DETAIL"));
    no_secret(&o);
}

fn error_run(payload: &'static str) -> Output {
    let (addr, _) = start(Arc::new(move |s, log| {
        handshake(s, log);
        recv(s, log);
        ev(s, "error", payload);
    }));
    let home = setup(&addr);
    turn(home.path(), &["x"])
}

#[test]
fn error_event_fails_with_known_message_only() {
    let o = error_run(r#"{"message":"Access denied"}"#);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("Turn failed: Access denied."),
        "{}",
        err(&o)
    );
    let o = error_run(r#"{"message":"Traceback internal-path"}"#);
    assert!(!o.status.success());
    assert!(!err(&o).contains("internal-path"), "{}", err(&o));
}

#[test]
fn unsuccessful_result_and_reconnect_failure_exit_nonzero() {
    for last in [
        r#"["result",{"session_id":"sess-1","success":false}]"#,
        r#"["reconnect_failed",{"session_id":"sess-1","message":"x"}]"#,
    ] {
        let (addr, _) = start(Arc::new(move |s, log| {
            handshake(s, log);
            recv(s, log);
            status(s, "active");
            recv(s, log);
            send(s, &format!("42/agent,{last}"));
        }));
        let home = setup(&addr);
        assert!(!turn(home.path(), &["x"]).status.success(), "{last}");
    }
}

#[test]
fn timeout_interrupts_and_fails() {
    let (addr, log) = start(Arc::new(|s, log| {
        handshake(s, log);
        recv(s, log);
        status(s, "sandbox_starting"); // never becomes active
        while recv(s, log).is_some() {}
    }));
    let home = setup(&addr);
    let o = turn(home.path(), &["x", "--timeout", "1"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("Timed out after 1s"), "{}", err(&o));
    assert!(wait_for(
        &log,
        r#"WS recv 42/agent,["interrupt",{"session_id":"sess-1"}]"#
    ));
    assert!(!lines(&log).iter().any(|l| l.contains(r#""message""#)));
}

#[test]
fn json_prints_each_event_as_a_line_without_the_token() {
    let (addr, _) = start(happy());
    let home = setup(&addr);
    let o = turn(home.path(), &["x", "--json"]);
    assert!(o.status.success(), "{}", err(&o));
    let ls: Vec<serde_json::Value> = out(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(ls[0]["event"], "session_status");
    assert_eq!(ls[0]["data"]["status"], "connecting");
    assert!(ls.iter().any(|v| v["event"] == "thinking"));
    assert_eq!(ls.last().unwrap()["event"], "result");
    // The foreign-session event is filtered, text is not printed raw.
    assert!(!out(&o).contains("WRONG") && !out(&o).starts_with("Hel"));
    no_secret(&o);
}

#[test]
fn non_allowlisted_host_is_refused_before_any_request() {
    let (addr, log) = start(happy());
    let home = setup("evil.example:8443");
    let o = turn(home.path(), &["x"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("Refusing socket URL"), "{}", err(&o));
    no_secret(&o);
    thread::sleep(Duration::from_millis(200));
    assert!(lines(&log).is_empty(), "{addr}: {:?}", lines(&log));
}

// ---- redaction and the overall deadline ----

/// Both secrets, a few spellings: whole in a delta, in a `text`, in an error message.
fn reflect_script(secret: &'static str) -> Script {
    Arc::new(move |s, log| {
        handshake(s, log);
        recv(s, log);
        status(s, "active");
        recv(s, log);
        delta(s, &format!("a {secret} b"));
        ev(
            s,
            "text",
            &format!(r#"{{"session_id":"sess-1","content":"t {secret}"}}"#),
        );
        ev(
            s,
            "thinking",
            &format!(r#"{{"session_id":"sess-1","content":"{secret}"}}"#),
        );
        ev(
            s,
            "error",
            &format!(r#"{{"session_id":"sess-1","message":"boom {secret}"}}"#),
        );
    })
}

#[test]
fn reflected_tokens_never_reach_stdout_or_stderr() {
    for secret in [SOCKTOK, ACCESS] {
        for json in [false, true] {
            let (addr, _) = start(reflect_script(secret));
            let home = setup(&addr);
            let args: &[&str] = if json { &["x", "--json"] } else { &["x"] };
            let o = turn(home.path(), args);
            no_secret(&o);
            let (so, se) = (out(&o), err(&o));
            assert!(so.contains("[REDACTED]"), "{secret} json={json}: {so}");
            if json {
                for l in so.lines() {
                    serde_json::from_str::<serde_json::Value>(l).unwrap();
                }
            }
            assert!(!se.contains(secret));
        }
    }
}

/// Token split in two across `text_delta` frames: whole token never printed.
#[test]
fn token_split_across_deltas_is_redacted() {
    for secret in [SOCKTOK, ACCESS] {
        for json in [false, true] {
            let (a, b) = secret.split_at(secret.len() / 2);
            let (a, b) = (a.to_string(), b.to_string());
            let (addr, _) = start(Arc::new(move |s, log| {
                handshake(s, log);
                recv(s, log);
                status(s, "active");
                recv(s, log);
                delta(s, &format!("pre {a}"));
                delta(s, &format!("{b} post"));
                ev(s, "result", r#"{"session_id":"sess-1","success":true}"#);
            }));
            let home = setup(&addr);
            let args: &[&str] = if json { &["x", "--json"] } else { &["x"] };
            let o = turn(home.path(), args);
            assert!(o.status.success(), "{}", err(&o));
            no_secret(&o);
            let so = out(&o);
            if json {
                let joined: String = so
                    .lines()
                    .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                    .filter(|v| v["event"] == "text_delta")
                    .map(|v| v["data"]["content"].as_str().unwrap().to_string())
                    .collect();
                assert_eq!(joined, "pre [REDACTED] post");
            } else {
                assert_eq!(so, "pre [REDACTED] post\n");
            }
        }
    }
}

/// Secret prefix streamed in a delta, its rest in an `error` message (--json line).
#[test]
fn prefix_in_delta_suffix_in_error_message_is_redacted() {
    for json in [false, true] {
        let (a, b) = SOCKTOK.split_at(6);
        let (a, b) = (a.to_string(), b.to_string());
        let (addr, _) = start(Arc::new(move |s, log| {
            handshake(s, log);
            recv(s, log);
            status(s, "active");
            recv(s, log);
            delta(s, &format!("pre {a}"));
            ev(
                s,
                "error",
                &format!(r#"{{"session_id":"sess-1","message":"{b} tail"}}"#),
            );
        }));
        let home = setup(&addr);
        let args: &[&str] = if json { &["x", "--json"] } else { &["x"] };
        let o = turn(home.path(), args);
        assert!(!o.status.success());
        no_secret(&o);
        let all = format!("{}{}", out(&o), err(&o));
        assert!(!all.contains(&SOCKTOK[6..]), "json={json}: {all}");
        assert!(all.contains("[REDACTED]"), "json={json}: {all}");
    }
}

/// A held-back tail is emitted at timeout (the hold-back must not eat the text).
#[test]
fn timeout_flushes_held_back_tail() {
    for json in [false, true] {
        let (addr, _) = start(Arc::new(|s, log| {
            handshake(s, log);
            recv(s, log);
            status(s, "active");
            recv(s, log);
            delta(s, "partial answer");
            while recv(s, log).is_some() {}
        }));
        let home = setup(&addr);
        let args: &[&str] = if json {
            &["x", "--timeout", "1", "--json"]
        } else {
            &["x", "--timeout", "1"]
        };
        let o = turn(home.path(), args);
        assert!(!o.status.success());
        assert!(err(&o).contains("Timed out after 1s"), "{}", err(&o));
        assert!(
            out(&o).contains("partial answer") || {
                // --json re-chunks: the pieces must add up
                out(&o)
                    .lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v["data"]["content"].as_str().map(str::to_string))
                    .collect::<String>()
                    .contains("partial answer")
            },
            "json={json}: {}",
            out(&o)
        );
    }
}

#[test]
fn stalled_http_calls_respect_the_overall_deadline() {
    for stall in ["POST /api/v1/agent/sessions", "GET /api/v1/auths/"] {
        let (addr, log) = start_with(happy(), Some(stall));
        let home = setup(&addr);
        let t0 = Instant::now();
        let o = turn(home.path(), &["x", "--timeout", "2"]);
        let took = t0.elapsed();
        assert!(!o.status.success());
        assert!(took < Duration::from_millis(3500), "{stall}: {took:?}");
        assert!(
            err(&o).contains("Timed out after 2s"),
            "{stall}: {}",
            err(&o)
        );
        assert!(
            !lines(&log).iter().any(|l| l.starts_with("WS connect")),
            "{stall}: {:?}",
            lines(&log)
        );
    }
}
