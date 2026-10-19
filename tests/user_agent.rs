//! Every request through `net::Client` must carry `monocle-cli/<version>` so
//! server logs can tell CLI versions apart.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use monocle_cli::net::Client;

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 64 * 1024;

/// Read one request head (until `\r\n\r\n`, capped) from a fresh connection.
fn read_head(l: &TcpListener) -> Result<String, String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut s = loop {
        match l.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(format!("accept: {e}")),
        }
    };
    s.set_nonblocking(false).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > MAX_HEAD {
            return Err("request head too large".into());
        }
        match s.read(&mut buf) {
            Ok(0) => return Err("connection closed before end of headers".into()),
            Ok(n) => head.extend_from_slice(&buf[..n]),
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
    Ok(String::from_utf8_lossy(&head).to_string())
}

/// Two-request raw server; yields each request head (or an error) on a channel.
fn raw_server() -> (String, mpsc::Receiver<Result<String, String>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let _ = tx.send(read_head(&l));
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
        let raw = rx.recv_timeout(TIMEOUT * 2).unwrap().unwrap();
        assert!(has_ua(&raw), "missing versioned UA in:\n{raw}");
    }
}
