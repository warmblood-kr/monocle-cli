//! `monocle craft turn`: a single loopback server speaks both the Craft REST routes
//! and the session WebSocket (it peeks each connection for an `Upgrade: websocket`
//! header). Binary tests run with `--no-delegation` (the delegation host check
//! refuses loopback by design); delegation order is covered at library level.

// tungstenite's handshake Callback signature forces a large Err type.
#![allow(clippy::result_large_err)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use monocle_cli::commands::craft_turn::{run_turn, Approve, Target, TurnOpts};
use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::net::Client;
use tungstenite::handshake::server::{Request, Response};
use tungstenite::http::HeaderValue;
use tungstenite::{Message, WebSocket};

const TOKEN: &str = "SECRET-ACCESS-TOK.abc_def";

type Log = Arc<Mutex<Vec<String>>>;
type WsScript = Arc<dyn Fn(&mut WebSocket<TcpStream>, &Log) + Send + Sync>;
type Rest = Arc<dyn Fn(&str, usize) -> (u16, String) + Send + Sync>;

fn rest_default(path: &str, _n: usize) -> (u16, String) {
    match path {
        "GET /v1/workspaces/recent" => (
            200,
            r#"{"workspaces":[{"path":"/gone","exists":false},{"path":"/w ws","exists":true}]}"#
                .into(),
        ),
        "POST /v1/workspaces/open" => (200, r#"{"ok":true}"#.into()),
        _ if path.ends_with("/messages") => (200, r#"{"messages":[]}"#.into()),
        _ => (404, "{}".into()),
    }
}

/// Peek the request head without consuming it.
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

fn serve_rest(mut s: TcpStream, log: &Log, rest: &Rest, counts: &Mutex<Vec<String>>) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let mut it = line.split_whitespace();
    let key = format!("{} {}", it.next().unwrap_or(""), it.next().unwrap_or(""));
    let mut len = 0;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    log.lock().unwrap().push(key.clone());
    let n = {
        let mut c = counts.lock().unwrap();
        c.push(key.clone());
        c.iter().filter(|k| **k == key).count()
    };
    let (status, body) = rest(&key, n);
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Start the stub; returns its `host:port`.
fn start(ws: WsScript, rest: Rest) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let log = Log::default();
    let l = log.clone();
    thread::spawn(move || {
        let counts = Arc::new(Mutex::new(Vec::new()));
        for s in listener.incoming().flatten() {
            let (log, ws, rest, counts) = (l.clone(), ws.clone(), rest.clone(), counts.clone());
            thread::spawn(move || {
                if peek_head(&s)
                    .to_ascii_lowercase()
                    .contains("upgrade: websocket")
                {
                    let (l2, l3) = (log.clone(), log.clone());
                    let cb = move |req: &Request, mut resp: Response| {
                        let p = req.headers().get("Sec-WebSocket-Protocol");
                        l2.lock().unwrap().push(format!(
                            "WS connect {} proto={}",
                            req.uri(),
                            p.and_then(|v| v.to_str().ok()).unwrap_or("-")
                        ));
                        resp.headers_mut().insert(
                            "Sec-WebSocket-Protocol",
                            HeaderValue::from_static("openworker"),
                        );
                        Ok(resp)
                    };
                    if let Ok(mut sock) = tungstenite::accept_hdr(s, cb) {
                        ws(&mut sock, &l3);
                    }
                } else {
                    serve_rest(s, &log, &rest, &counts);
                }
            });
        }
    });
    (addr, log)
}

fn frame(sock: &mut WebSocket<TcpStream>, v: &str) {
    sock.send(Message::text(v)).unwrap();
}

/// Read client frames until a `user_message`; logs every frame as `WS recv <json>`.
fn recv(sock: &mut WebSocket<TcpStream>, log: &Log) -> Option<serde_json::Value> {
    let Ok(Message::Text(t)) = sock.read() else {
        return None;
    };
    log.lock().unwrap().push(format!("WS recv {t}"));
    serde_json::from_str(t.as_str()).ok()
}

const READY: &str = r#"{"type":"ready","data":{"session_id":"s","agent":"cowork","model":"m","mode":"interactive","workspace":"/w ws"}}"#;

fn ws_script(f: impl Fn(&mut WebSocket<TcpStream>, &Log) + Send + Sync + 'static) -> WsScript {
    Arc::new(f)
}

fn happy() -> WsScript {
    ws_script(|s, log| {
        frame(s, READY);
        assert_eq!(recv(s, log).unwrap()["text"], "hi there");
        frame(s, r#"{"type":"turn_start","data":{}}"#);
        frame(
            s,
            r#"{"type":"reasoning_delta","data":{"text":"THINKING"}}"#,
        );
        frame(s, r#"{"type":"assistant_delta","data":{"text":"Hel"}}"#);
        frame(s, r#"{"type":"assistant_delta","data":{"delta":"lo"}}"#);
        frame(s, r#"{"type":"assistant_message","data":{"text":"Hello"}}"#);
        frame(s, r#"{"type":"turn_end","data":{}}"#);
        let _ = s.close(None);
    })
}

fn setup(addr: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: addr.into(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: TOKEN.into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(format!("http://{addr}")),
        })
        .unwrap();
    home
}

fn turn(home: &Path, url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(["craft", "turn"])
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .env("MONOCLE_CRAFT_URL", url)
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
fn no_token(o: &Output) {
    assert!(!out(o).contains("SECRET-ACCESS-TOK") && !err(o).contains("SECRET-ACCESS-TOK"));
}

#[test]
fn streams_text_sends_token_as_subprotocol_and_never_leaks_it() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["hi there", "--no-delegation"],
    );
    assert!(o.status.success(), "{}", err(&o));
    // Deltas printed live; the final assistant_message is not printed again; reasoning ignored.
    assert_eq!(out(&o), "Hello\n");
    assert!(err(&o).contains("Session: cli-"), "{}", err(&o));
    no_token(&o);
    let l = lines(&log);
    let c = l.iter().find(|x| x.starts_with("WS connect")).unwrap();
    assert!(c.contains(&format!("proto=openworker, {TOKEN}")), "{c}");
    // Default agent, and the first *existing* recent workspace, percent-encoded.
    assert!(c.contains("?agent=cowork&workspace=%2Fw%20ws"), "{c}");
    assert!(c.contains("/ws/session/cli-"), "{c}");
}

#[test]
fn assistant_message_is_printed_when_no_delta_was_seen() {
    let (addr, _) = start(
        ws_script(|s, log| {
            frame(s, READY);
            recv(s, log);
            frame(
                s,
                r#"{"type":"assistant_message","data":{"content":"Whole"}}"#,
            );
            frame(s, r#"{"type":"turn_done","data":{}}"#);
        }),
        Arc::new(rest_default),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation"],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "Whole\n");
}

fn permission_run(extra: &[&str]) -> (Output, Vec<String>) {
    let (addr, log) = start(
        ws_script(|s, log| {
            frame(s, READY);
            recv(s, log);
            frame(
                s,
                r#"{"type":"permission_required","data":{"tool":"bash"}}"#,
            );
            recv(s, log); // the approval answer
            frame(s, r#"{"type":"turn_end","data":{}}"#);
        }),
        Arc::new(rest_default),
    );
    let home = setup(&addr);
    let mut a = vec!["go", "--no-delegation"];
    a.extend_from_slice(extra);
    let o = turn(home.path(), &format!("http://{addr}"), &a);
    (o, lines(&log))
}

#[test]
fn permission_is_denied_by_default() {
    let (o, l) = permission_run(&[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        l.contains(&r#"WS recv {"type":"approval","decision":"deny"}"#.to_string()),
        "{l:?}"
    );
    assert!(err(&o).contains("denied"));
}

#[test]
fn permission_is_approved_once_with_flag() {
    let (o, l) = permission_run(&["--approve", "once"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        l.contains(&r#"WS recv {"type":"approval","decision":"once"}"#.to_string()),
        "{l:?}"
    );
}

#[test]
fn question_is_answered_with_interrupt_and_fails() {
    let (addr, log) = start(
        ws_script(|s, log| {
            frame(s, READY);
            recv(s, log);
            frame(s, r#"{"type":"question_requested","data":{}}"#);
            recv(s, log);
            frame(s, r#"{"type":"interrupted","data":{}}"#);
        }),
        Arc::new(rest_default),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["q", "--no-delegation"],
    );
    assert!(!o.status.success());
    assert!(lines(&log).contains(&r#"WS recv {"type":"interrupt"}"#.to_string()));
}

#[test]
fn error_frame_fails_with_its_message() {
    let (addr, _) = start(
        ws_script(|s, log| {
            frame(s, READY);
            recv(s, log);
            frame(
                s,
                r#"{"type":"error","data":{"error":"provider exploded"}}"#,
            );
        }),
        Arc::new(rest_default),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation"],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("provider exploded"), "{}", err(&o));
    assert_eq!(out(&o), "");
}

#[test]
fn json_prints_every_frame_as_a_line_and_no_plain_text() {
    let (addr, _) = start(happy(), Arc::new(rest_default));
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["hi there", "--no-delegation", "--json"],
    );
    assert!(o.status.success(), "{}", err(&o));
    let vals: Vec<serde_json::Value> = out(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(vals.iter().all(|v| v["source"] == "ws"));
    let kinds: Vec<&str> = vals
        .iter()
        .map(|v| v["frame"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[0], "ready");
    assert!(kinds.contains(&"reasoning_delta") && kinds.last() == Some(&"turn_end"));
    assert!(!out(&o).lines().any(|l| l == "Hello"));
}

fn accepted_script() -> WsScript {
    ws_script(|s, log| {
        frame(s, READY);
        recv(s, log);
        frame(
            s,
            r#"{"type":"turn_accepted","data":{"status":202,"turn_id":"t1","command_id":"c","state":"queued"}}"#,
        );
        while s.read().is_ok() {} // control-only socket: stay quiet until the client closes
    })
}

/// /messages answers per call number: 1 = baseline, then the scripted polls.
fn polled(steps: &'static [&'static str]) -> Rest {
    Arc::new(move |p, n| {
        if p.ends_with("/messages") {
            return (200, steps[(n - 1).min(steps.len() - 1)].into());
        }
        rest_default(p, n)
    })
}

#[test]
fn turn_accepted_switches_to_polling_messages() {
    let (addr, log) = start(
        accepted_script(),
        polled(&[
            r#"{"messages":[{"role":"user","content":"old"},{"role":"assistant","content":"OLD ANSWER"}]}"#,
            r#"{"messages":[{"role":"user","content":"old"},{"role":"assistant","content":"OLD ANSWER"},{"role":"user","content":"x"}]}"#,
            r#"{"messages":[{"role":"user","content":"old"},{"role":"assistant","content":"OLD ANSWER"},{"role":"user","content":"x"},{"role":"assistant","text":"Polled answer"}]}"#,
        ]),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation"],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "Polled answer\n");
    let polls = lines(&log)
        .iter()
        .filter(|l| l.ends_with("/messages"))
        .count();
    assert!(polls >= 4, "baseline + 3 polls, got {polls}");
    no_token(&o);
}

#[test]
fn polling_reports_error_notice() {
    let (addr, _) = start(
        accepted_script(),
        polled(&[
            r#"{"messages":[]}"#,
            r#"{"messages":[{"role":"user","content":"x"},{"role":"notice","kind":"error","text":"quota hit"}]}"#,
        ]),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation"],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("quota hit"), "{}", err(&o));
    assert_eq!(out(&o), "");
}

#[test]
fn polling_times_out() {
    let (addr, _) = start(accepted_script(), polled(&[r#"{"messages":[]}"#]));
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation", "--timeout", "3"],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("Timed out"), "{}", err(&o));
}

#[test]
fn silent_socket_times_out() {
    let (addr, _) = start(
        ws_script(|s, _| {
            frame(s, READY);
            while s.read().is_ok() {}
        }),
        Arc::new(rest_default),
    );
    let home = setup(&addr);
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation", "--timeout", "2"],
    );
    assert!(!o.status.success());
    assert!(err(&o).contains("Timed out"), "{}", err(&o));
}

#[test]
fn non_allowlisted_host_is_refused_before_any_connection() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let home = setup(&addr);
    for url in [
        "http://evil.example:9",
        "https://craft.monocle-ai.com:8443",
        "ws://127.0.0.1:1",
    ] {
        let o = turn(home.path(), url, &["x", "--no-delegation"]);
        assert!(!o.status.success(), "{url}");
        assert!(err(&o).contains("Refusing"), "{url}: {}", err(&o));
        no_token(&o);
    }
    assert!(
        lines(&log).is_empty(),
        "stub was contacted: {:?}",
        lines(&log)
    );
}

#[test]
fn delegation_failure_aborts_before_the_socket() {
    // Without --no-delegation the binary refuses loopback for the id_token; no WS follows.
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let home = setup(&addr);
    let o = turn(home.path(), &format!("http://{addr}"), &["hi there"]);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("Delegation is only allowed"),
        "{}",
        err(&o)
    );
    assert!(
        lines(&log).iter().all(|l| !l.starts_with("WS ")),
        "{:?}",
        lines(&log)
    );
}

#[test]
fn sdk_lane_is_refused() {
    let home = setup("127.0.0.1:1");
    let o = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(["craft", "--lane", "sdk", "turn", "x"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(
        err(&o).contains("only supported on the openworker lane"),
        "{}",
        err(&o)
    );
}

fn opts() -> TurnOpts {
    TurnOpts {
        text: "hi there".into(),
        session: Some("sess-1".into()),
        workspace: None,
        create_workspace: false,
        agent: "cowork".into(),
        timeout: Duration::from_secs(10),
        approve: Approve::Deny,
        json: false,
        poll_every: Duration::from_millis(50),
    }
}

#[test]
fn library_delegates_before_connecting_and_before_workspace_calls() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let client = Client::new();
    let (base, ws_base) = (format!("http://{addr}"), format!("ws://{addr}"));
    let t = Target {
        client: &client,
        base: &base,
        ws_base: &ws_base,
        bearer: &format!("Bearer {TOKEN}"),
    };
    let l = log.clone();
    run_turn(&t, &opts(), |id| {
        assert_eq!(id, "sess-1");
        l.lock().unwrap().push("delegate".into());
        Ok(())
    })
    .unwrap();
    let l = lines(&log);
    assert_eq!(l[0], "delegate", "{l:?}");
    let ws_at = l.iter().position(|x| x.starts_with("WS connect")).unwrap();
    assert!(ws_at > 0 && l[1] == "GET /v1/workspaces/recent", "{l:?}");
}

#[test]
fn library_delegation_error_stops_everything() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let client = Client::new();
    let (base, ws_base) = (format!("http://{addr}"), format!("ws://{addr}"));
    let t = Target {
        client: &client,
        base: &base,
        ws_base: &ws_base,
        bearer: "Bearer x",
    };
    let r = run_turn(&t, &opts(), |_| {
        Err(monocle_cli::error::AppError::new("nope"))
    });
    assert!(r.is_err());
    assert!(lines(&log).is_empty());
}

#[test]
fn library_explicit_workspace_is_opened_with_create_only_if_asked() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let client = Client::new();
    let (base, ws_base) = (format!("http://{addr}"), format!("ws://{addr}"));
    let t = Target {
        client: &client,
        base: &base,
        ws_base: &ws_base,
        bearer: "Bearer x",
    };
    let mut o = opts();
    o.workspace = Some("/mine".into());
    run_turn(&t, &o, |_| Ok(())).unwrap();
    let l = lines(&log);
    assert!(l.contains(&"POST /v1/workspaces/open".to_string()));
    assert!(!l.contains(&"GET /v1/workspaces/recent".to_string()));
    assert!(l.iter().any(|x| x.contains("workspace=%2Fmine")), "{l:?}");
}

#[test]
fn library_refuses_non_loopback_plain_ws_before_connecting() {
    let (addr, log) = start(happy(), Arc::new(rest_default));
    let client = Client::new();
    let base = format!("http://{addr}");
    let t = Target {
        client: &client,
        base: &base,
        ws_base: "ws://evil.example:80",
        bearer: "Bearer SECRET-ACCESS-TOK",
    };
    let e = run_turn(&t, &opts(), |_| Ok(())).unwrap_err().to_string();
    assert!(e.contains("URL refused") && !e.contains("SECRET"), "{e}");
    assert!(!lines(&log).iter().any(|l| l.starts_with("WS ")));
}

#[test]
fn handshake_http_error_shows_only_the_status_code() {
    // Plain HTTP 403 (no upgrade) from a server that echoes the request back.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let echo = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = write!(
                s,
                "HTTP/1.1 403 No\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{echo}",
                echo.len()
            );
        }
    });
    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let ws_base = format!("ws://127.0.0.1:{port}");
    let t = Target {
        client: &client,
        base: &base,
        ws_base: &ws_base,
        bearer: "Bearer SECRET-ACCESS-TOK",
    };
    let mut o = opts();
    o.workspace = None;
    // workspace listing fails against this server (non-fatal), then the handshake is refused
    let e = run_turn(&t, &o, |_| Ok(())).unwrap_err().to_string();
    assert!(
        e.contains("HTTP 403") && !e.contains("SECRET") && !e.contains("Sec-WebSocket"),
        "{e}"
    );
}

// ---- token reflection: a server echoing the access token must never reach the terminal ----

fn reflecting_ws(kind_and_data: String) -> WsScript {
    ws_script(move |s, log| {
        frame(s, READY);
        recv(s, log);
        frame(s, &kind_and_data);
        frame(s, r#"{"type":"turn_end","data":{}}"#);
    })
}

fn assert_redacted(o: &Output) {
    no_token(o);
    let all = format!("{}{}", out(o), err(o));
    assert!(all.contains("[REDACTED]"), "{all}");
}

fn run_reflect(ws: WsScript, rest: Rest, extra: &[&str]) -> Output {
    let (addr, _) = start(ws, rest);
    let home = setup(&addr);
    let mut a = vec!["x", "--no-delegation"];
    a.extend_from_slice(extra);
    turn(home.path(), &format!("http://{addr}"), &a)
}

#[test]
fn token_in_assistant_delta_is_redacted() {
    let f = format!(r#"{{"type":"assistant_delta","data":{{"text":"leak {TOKEN} end"}}}}"#);
    let o = run_reflect(reflecting_ws(f), Arc::new(rest_default), &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "leak [REDACTED] end\n");
    assert_redacted(&o);
}

#[test]
fn token_in_error_frame_is_redacted() {
    let f = format!(r#"{{"type":"error","data":{{"error":"bad {TOKEN}"}}}}"#);
    let o = run_reflect(reflecting_ws(f), Arc::new(rest_default), &[]);
    assert!(!o.status.success());
    assert_redacted(&o);
}

#[test]
fn token_in_json_frame_is_redacted() {
    let f = format!(r#"{{"type":"assistant_delta","data":{{"text":"leak {TOKEN}"}}}}"#);
    let o = run_reflect(reflecting_ws(f), Arc::new(rest_default), &["--json"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_redacted(&o);
    for l in out(&o).lines() {
        serde_json::from_str::<serde_json::Value>(l).unwrap(); // still valid JSON
    }
}

#[test]
fn token_in_polled_assistant_message_is_redacted() {
    let m = format!(
        r#"{{"messages":[{{"role":"user","content":"x"}},{{"role":"assistant","content":"leak {TOKEN}"}}]}}"#
    );
    let m: &'static str = Box::leak(m.into_boxed_str());
    let o = run_reflect(
        accepted_script(),
        polled(Box::leak(Box::new([r#"{"messages":[]}"#, m]))),
        &[],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(out(&o), "leak [REDACTED]\n");
    assert_redacted(&o);
}

#[test]
fn token_in_polled_error_notice_is_redacted() {
    let m = format!(
        r#"{{"messages":[{{"role":"user","content":"x"}},{{"role":"notice","kind":"error","text":"bad {TOKEN}"}}]}}"#
    );
    let m: &'static str = Box::leak(m.into_boxed_str());
    let o = run_reflect(
        accepted_script(),
        polled(Box::leak(Box::new([r#"{"messages":[]}"#, m]))),
        &[],
    );
    assert!(!o.status.success());
    assert_redacted(&o);
}

// ---- one overall deadline ----

fn assert_times_out_fast(o: &Output, started: std::time::Instant) {
    assert!(!o.status.success());
    assert!(err(o).contains("Timed out after 2s"), "{}", err(o));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(3500), "took {took:?}");
}

#[test]
fn stalled_messages_endpoint_times_out_at_the_deadline() {
    // Baseline read (call 1) answers; the first poll then stalls past --timeout.
    let rest: Rest = Arc::new(|p, n| {
        if p.ends_with("/messages") && n > 1 {
            thread::sleep(Duration::from_secs(6));
        }
        rest_default(p, n)
    });
    let (addr, _) = start(accepted_script(), rest);
    let home = setup(&addr);
    let t0 = std::time::Instant::now();
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation", "--timeout", "2"],
    );
    assert_times_out_fast(&o, t0);
}

#[test]
fn stalled_workspace_listing_times_out_at_the_deadline() {
    let rest: Rest = Arc::new(|p, n| {
        if p == "GET /v1/workspaces/recent" {
            thread::sleep(Duration::from_secs(6));
        }
        rest_default(p, n)
    });
    let (addr, log) = start(happy(), rest);
    let home = setup(&addr);
    let t0 = std::time::Instant::now();
    let o = turn(
        home.path(),
        &format!("http://{addr}"),
        &["x", "--no-delegation", "--timeout", "2"],
    );
    assert_times_out_fast(&o, t0);
    assert!(!lines(&log).iter().any(|l| l.starts_with("WS connect")));
}
