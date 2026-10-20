//! `delete_thread` against a raw one-shot TCP stub (the shared stub cannot
//! expose request headers): method, path, Authorization, error sanitising.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use monocle_cli::jarvice_chats::delete_thread;
use monocle_cli::net::Client;

/// Serve one canned response; returns (base url, receiver of the request head).
fn once(status: u16, body: &'static str) -> (String, mpsc::Receiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap();
        tx.send(String::from_utf8_lossy(&buf[..n]).into_owned())
            .ok();
        let _ = write!(
            s,
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (url, rx)
}

fn del(url: &str, id: &str) -> Result<(), String> {
    delete_thread(&Client::new(), url, "Bearer tok", id).map_err(|e| e.to_string())
}

#[test]
fn delete_sends_delete_with_auth_and_accepts_true() {
    let (url, rx) = once(200, "true");
    del(&url, "c1").unwrap();
    let head = rx.recv().unwrap().to_ascii_lowercase();
    assert!(
        head.starts_with("delete /api/v1/chats/c1 http/1.1"),
        "{head}"
    );
    assert!(head.contains("authorization: bearer tok"), "{head}");
}

#[test]
fn delete_false_body_is_failure() {
    let (url, _rx) = once(200, "false");
    let err = del(&url, "c1").unwrap_err();
    assert!(err.contains("did not confirm"), "{err}");
}

#[test]
fn delete_error_never_echoes_body() {
    let (url, _rx) = once(500, "SECRET-STACK");
    let err = del(&url, "c1").unwrap_err();
    assert_eq!(
        err,
        "Deleting the chat failed (HTTP 500); the chat may not exist or may not be yours."
    );

    let (url, _rx) = once(401, r#"{"detail":{"code":"NOT_FOUND","msg":"SECRET"}}"#);
    let err = del(&url, "c1").unwrap_err();
    assert!(err.contains("HTTP 401 (NOT_FOUND)"), "{err}");
    assert!(err.contains("not found or is not yours"), "{err}");
    assert!(!err.contains("SECRET"), "{err}");
}

#[test]
fn delete_rejects_bad_id_without_request() {
    assert_eq!(
        del("http://127.0.0.1:1", "../x").unwrap_err(),
        "Invalid chat thread ID."
    );
}
