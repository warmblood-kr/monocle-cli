//! W3C `traceparent` on requests to Monocle hosts (monocle-cli#177): one
//! loopback stub speaks jarvice REST + the sdk-lane Socket.IO WebSocket and
//! records every request head.

#![allow(clippy::result_large_err)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use monocle_cli::credentials::{Credentials, CredentialsData};
use tungstenite::{Message, WebSocket};

type Heads = Arc<Mutex<Vec<String>>>;

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

fn ws_script(s: &mut WebSocket<TcpStream>) {
    let ev = |s: &mut WebSocket<TcpStream>, name: &str, data: &str| {
        s.send(Message::text(format!(r#"42/agent,["{name}",{data}]"#)))
            .unwrap();
    };
    let st = |s: &mut WebSocket<TcpStream>, v: &str| {
        ev(
            s,
            "session_status",
            &format!(r#"{{"session_id":"sess-1","status":"{v}"}}"#),
        )
    };
    s.send(Message::text(
        r#"0{"sid":"e1","upgrades":[],"pingInterval":25000,"pingTimeout":20000}"#,
    ))
    .unwrap();
    let _ = s.read(); // connect auth
    s.send(Message::text(r#"40/agent,{"sid":"n1"}"#)).unwrap();
    s.send(Message::text("2")).unwrap();
    let _ = s.read(); // start_session
    let _ = s.read(); // pong
    st(s, "connecting");
    st(s, "active");
    let _ = s.read(); // message
    ev(s, "text", r#"{"session_id":"sess-1","content":"Hello"}"#);
    s.send(Message::text(
        r#"42/agent,7["result",{"session_id":"sess-1","success":true,"cost":0.1}]"#,
    ))
    .unwrap();
    let _ = s.close(None);
}

/// Serves jarvice REST + the sdk WebSocket; `redirect` answers every REST call
/// with a 302 to that URL. Returns `(host:port, recorded lowercase heads)`.
fn start(redirect: Option<String>) -> (String, Heads) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let heads = Heads::default();
    let h = heads.clone();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (h, redirect) = (h.clone(), redirect.clone());
            thread::spawn(move || {
                let head = peek_head(&s);
                h.lock().unwrap().push(head.to_ascii_lowercase());
                if head.to_ascii_lowercase().contains("upgrade: websocket") {
                    if let Ok(mut sock) = tungstenite::accept(s) {
                        ws_script(&mut sock);
                    }
                    return;
                }
                let mut s = s;
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let key = line
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut len = 0;
                loop {
                    let mut x = String::new();
                    r.read_line(&mut x).unwrap();
                    if x.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = x.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                r.read_exact(&mut vec![0; len]).unwrap();
                if let Some(to) = redirect {
                    let _ = write!(
                        s,
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                }
                let (status, body) = match key.as_str() {
                    "POST /api/v1/agent/sessions" => (200, r#"{"id":"sess-1","title":"t"}"#),
                    "GET /api/v1/agent/sessions/sess-1" => (200, r#"{"id":"sess-1"}"#),
                    "GET /api/v1/auths/" => (200, r#"{"token":"SOCK.tok","role":"admin"}"#),
                    _ => (404, "{}"),
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (addr, heads)
}

fn setup(tenant: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: tenant.into(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "ACCESS-TOK".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: None,
        })
        .unwrap();
    home
}

fn monocle(home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .env_remove("MONOCLE_NO_TRACEPARENT")
        .envs(env.iter().copied())
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// `(trace, span)` of the head's `traceparent`, validated against
/// `^00-[0-9a-f]{32}-[0-9a-f]{16}-01$`; `None` if the header is absent.
fn traceparent(head: &str) -> Option<(String, String)> {
    let v = head
        .lines()
        .find_map(|l| l.strip_prefix("traceparent:"))?
        .trim();
    let p: Vec<&str> = v.split('-').collect();
    let hex = |s: &str, n: usize| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    assert!(
        p.len() == 4 && p[0] == "00" && hex(p[1], 32) && hex(p[2], 16) && p[3] == "01",
        "bad traceparent {v}"
    );
    assert_ne!(p[1], "0".repeat(32));
    Some((p[1].into(), p[2].into()))
}

fn timings_ids(e: &str) -> Vec<String> {
    e.lines()
        .filter(|l| l.starts_with(r#"{"timings""#))
        .map(|l| {
            let t = &serde_json::from_str::<serde_json::Value>(l).unwrap()["timings"];
            assert_eq!(t["trace_id"], t["run_id"]);
            t["trace_id"].as_str().unwrap().to_string()
        })
        .collect()
}

const SDK: [&str; 4] = ["craft", "--lane", "sdk", "turn"];

#[test]
fn header_matches_timings_and_diagnostics_and_spans_differ() {
    let (addr, heads) = start(None);
    let home = setup(&addr);
    let diag = home.path().join("d.jsonl");
    let mut args = SDK.to_vec();
    args.extend([
        "hi",
        "--timings",
        "--diagnostics-file",
        diag.to_str().unwrap(),
    ]);
    let o = monocle(home.path(), &args, "", &[]);
    assert!(o.status.success(), "{}", stderr(&o));

    let sent: Vec<_> = heads
        .lock()
        .unwrap()
        .iter()
        .map(|h| traceparent(h).expect("every request to the tenant carries traceparent"))
        .collect();
    assert!(sent.len() >= 3, "REST + WS handshake: {sent:?}");
    let ids = timings_ids(&stderr(&o));
    assert_eq!(ids.len(), 1);
    assert!(
        sent.iter().all(|(t, _)| *t == ids[0]),
        "{sent:?} vs {ids:?}"
    );
    let mut spans: Vec<_> = sent.iter().map(|(_, s)| s.clone()).collect();
    spans.sort();
    spans.dedup();
    assert_eq!(spans.len(), sent.len(), "span ids must differ per request");
    let lines = std::fs::read_to_string(&diag).unwrap();
    assert!(!lines.is_empty());
    for l in lines.lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap();
        assert_eq!(v["trace_id"], ids[0].as_str());
        assert_eq!(v["run_id"], ids[0].as_str());
    }
}

#[test]
fn no_traceparent_when_disabled_by_env() {
    let (addr, heads) = start(None);
    let home = setup(&addr);
    let mut args = SDK.to_vec();
    args.push("hi");
    let o = monocle(home.path(), &args, "", &[("MONOCLE_NO_TRACEPARENT", "1")]);
    assert!(o.status.success(), "{}", stderr(&o));
    let heads = heads.lock().unwrap();
    assert!(!heads.is_empty());
    assert!(
        heads.iter().all(|h| !h.contains("traceparent")),
        "{heads:?}"
    );
}

#[test]
fn interactive_turns_each_get_a_new_trace() {
    let (addr, heads) = start(None);
    let home = setup(&addr);
    let mut args = SDK.to_vec();
    args.extend(["-i", "--timings"]);
    let o = monocle(home.path(), &args, "one\ntwo\n", &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    let ids = timings_ids(&stderr(&o));
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let mut sent: Vec<String> = heads
        .lock()
        .unwrap()
        .iter()
        .map(|h| traceparent(h).unwrap().0)
        .collect();
    sent.sort();
    sent.dedup();
    let mut want = ids.clone();
    want.sort();
    assert_eq!(sent, want, "header traces == per-turn run_ids");
}

#[test]
fn redirect_to_another_host_does_not_carry_traceparent() {
    let (b_addr, b_heads) = start(None);
    let b_port = b_addr.rsplit(':').next().unwrap();
    // A is registered as `localhost`; B is the same machine as `127.0.0.1`.
    let (a_addr, a_heads) = start(Some(format!("http://127.0.0.1:{b_port}/api/v1/auths/")));
    let a_port = a_addr.rsplit(':').next().unwrap();
    let home = setup(&format!("localhost:{a_port}"));
    let o = monocle(home.path(), &["whoami", "--server"], "", &[]);
    let _ = o;
    let a = a_heads.lock().unwrap();
    assert!(a.iter().any(|h| traceparent(h).is_some()), "{a:?}");
    thread::sleep(Duration::from_millis(200));
    assert!(b_heads.lock().unwrap().is_empty(), "redirect was followed");
}
