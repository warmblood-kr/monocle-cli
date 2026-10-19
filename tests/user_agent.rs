//! Every request through `net::Client` must carry `monocle-cli/<version>` so
//! server logs can tell CLI versions apart.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use monocle_cli::net::Client;

/// One-shot raw server; returns the URL and a channel yielding the raw request head.
fn raw_server() -> (String, mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            tx.send(String::from_utf8_lossy(&buf[..n]).to_string())
                .unwrap();
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        }
    });
    (url, rx)
}

fn has_ua(raw: &str) -> bool {
    let want = format!("user-agent: monocle-cli/{}", env!("CARGO_PKG_VERSION"));
    raw.lines().any(|l| l.to_lowercase() == want)
}

#[test]
fn get_and_post_json_send_versioned_user_agent() {
    let (url, rx) = raw_server();
    let c = Client::new();
    c.get(&url, &[]).unwrap();
    c.post_json(&url, &[("x-monocle-origin", "cli")], &serde_json::json!({}))
        .unwrap();
    for _ in 0..2 {
        let raw = rx.recv().unwrap();
        assert!(has_ua(&raw), "missing versioned UA in:\n{raw}");
    }
}
