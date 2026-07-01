//! A tiny synchronous stub HTTP server for exercising the `net` facade end to
//! end without mocking reqwest. Mirrors the `deps.fetch` injection the TS tests
//! used, but with a real loopback server (keeps everything sync).
//!
//! (Each test binary compiles its own copy and uses a different subset, so
//! unused helpers here are expected.)
#![allow(dead_code)]

use std::thread;

use tiny_http::{Response, Server};

pub struct Stub {
    /// `host:port` — feed as a tenant domain (resolves to http on 127.0.0.1) or
    /// prefix with `http://` for a router URL.
    pub addr: String,
}

impl Stub {
    pub fn router_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Spawn a stub server. `handler(addr, method, url, body) -> (status, body)`,
/// where `addr` is this server's own `host:port` (for building absolute
/// endpoint URLs in discovery docs). The thread runs until the process exits.
pub fn stub<F>(handler: F) -> Stub
where
    F: Fn(&str, &str, &str, &str) -> (u16, String) + Send + 'static,
{
    let server = Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let addr_for_thread = addr.clone();
    thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let method = req.method().as_str().to_string();
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, resp_body) = handler(&addr_for_thread, &method, &url, &body);
            let _ = req.respond(Response::from_string(resp_body).with_status_code(status));
        }
    });
    Stub { addr }
}

/// Like [`stub`] but responds with `Content-Type: text/event-stream` so the
/// provider takes its SSE-parsing path. Handler returns the full SSE body.
pub fn stub_sse<F>(handler: F) -> Stub
where
    F: Fn(&str, &str, &str, &str) -> (u16, String) + Send + 'static,
{
    let server = Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let addr_for_thread = addr.clone();
    thread::spawn(move || {
        let ct =
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap();
        for mut req in server.incoming_requests() {
            let method = req.method().as_str().to_string();
            let url = req.url().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let (status, resp_body) = handler(&addr_for_thread, &method, &url, &body);
            let _ = req.respond(
                Response::from_string(resp_body)
                    .with_status_code(status)
                    .with_header(ct.clone()),
            );
        }
    });
    Stub { addr }
}
