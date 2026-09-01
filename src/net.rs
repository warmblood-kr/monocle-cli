//! The HTTP facade — the ONE place that touches `reqwest`.
//!
//! Everything above this module is plain sync code that speaks only in
//! `net::Resp`. Today the implementation is `reqwest::blocking`; if we ever need
//! async (concurrency, streaming), a single function here can spin up a
//! `tokio::Runtime` and `block_on` internally while keeping these sync
//! signatures — so "go async later" never colors the call tree. Keep `reqwest`
//! types from leaking past this boundary.

use std::io::{BufRead, BufReader, Read};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{AppError, Result};

/// An owned, already-buffered HTTP response. No streaming, no borrows on the
/// client — the network is fully done by the time you hold one of these.
pub struct Resp {
    pub status: u16,
    pub status_text: String,
    body: Vec<u8>,
    /// Response headers, name lowercased (HTTP header names are case-insensitive)
    /// — read via `header()`. Most callers only need `status`/`body`; this
    /// exists for the few responses that carry state in a header instead of
    /// the JSON body (e.g. jarvice's `/api/responses` returning the server-
    /// created thread id via `X-Thread-Id`).
    headers: Vec<(String, String)>,
}

impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// Look up a response header by name (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A multipart file part (the binary payload of an audio upload).
pub struct FilePart {
    pub field: String,
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

pub struct Client {
    inner: reqwest::blocking::Client,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        // Timeout split: `connect_timeout` bounds only connection establishment,
        // so a hung/unreachable endpoint fails fast — but it does NOT limit body
        // read time, which keeps it safe for long streaming generations. The total
        // per-request `.timeout()` is applied only to non-streaming calls in
        // `send()`; the streaming path (`post_json_stream`) deliberately gets no
        // total timeout, since one would cut a long generation mid-stream.
        let inner = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            // TCP keepalive lets the OS surface a dead peer on the streaming path
            // (which has no total timeout) without capping a healthy long stream.
            .tcp_keepalive(std::time::Duration::from_secs(60))
            // Kept below AWS ALB's default 60s idle timeout (our chat-proxy/
            // jarvice endpoints sit behind one) so THIS client evicts an idle
            // pooled connection before the peer does. Without this, reqwest's
            // own default (90s) leaves a ~30s window where we trust a
            // connection the peer already closed — the next request's send
            // then fails with a connection reset (surfaced as "error sending
            // request for url", not a timeout). `send_with_retry` below is
            // the backstop for whatever this doesn't catch.
            .pool_idle_timeout(std::time::Duration::from_secs(50))
            .build()
            .expect("failed to build HTTP client");
        Self { inner }
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Resp> {
        let mut req = self.inner.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send("GET", url, req)
    }

    /// GET a large file download (e.g. the `monocle upgrade` release binary).
    ///
    /// This deliberately does NOT route through the shared `send()`: a multi-MB
    /// binary on a slow link must not inherit the short total timeout that the
    /// shared API path may carry (it would abort a legitimately slow transfer).
    /// Instead it applies its own generous per-request timeout (600s).
    pub fn get_download(&self, url: &str, headers: &[(&str, &str)]) -> Result<Resp> {
        let mut req = self
            .inner
            .get(url)
            .timeout(std::time::Duration::from_secs(600));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        // Inline send (not the shared `send()`) so the 600s timeout above is the
        // only cap that applies to the download — but still goes through
        // `send_with_retry` for the same stale-connection backstop.
        let resp = send_with_retry("GET", url, req).map_err(|e| AppError(e.to_string()))?;
        let status = resp.status().as_u16();
        let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
        let headers = collect_headers(resp.headers());
        let body = resp.bytes().map_err(|e| AppError(e.to_string()))?.to_vec();
        Ok(Resp {
            status,
            status_text,
            body,
            headers,
        })
    }

    /// POST `application/x-www-form-urlencoded` (token / device-code requests).
    pub fn post_form(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        form: &[(&str, &str)],
    ) -> Result<Resp> {
        let mut req = self.inner.post(url).form(form);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send("POST", url, req)
    }

    /// POST `application/json`.
    pub fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Resp> {
        let mut req = self.inner.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send("POST", url, req)
    }

    /// POST `multipart/form-data` with one or more binary file parts plus text
    /// fields (e.g. an audio upload sends one `file` part; an image edit sends
    /// an `image` part and an optional `mask` part).
    pub fn post_multipart(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        files: Vec<FilePart>,
        text_fields: &[(&str, &str)],
    ) -> Result<Resp> {
        let mut form = reqwest::blocking::multipart::Form::new();
        for file in files {
            let part = reqwest::blocking::multipart::Part::bytes(file.data)
                .file_name(file.filename)
                .mime_str(&file.content_type)
                .map_err(|e| AppError(e.to_string()))?;
            form = form.part(file.field, part);
        }
        for (k, v) in text_fields {
            form = form.text(k.to_string(), v.to_string());
        }
        let mut req = self.inner.post(url).multipart(form);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send("POST", url, req)
    }

    /// POST a raw body with an explicit `Content-Type` (Azure SSML).
    pub fn post_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Resp> {
        let mut req = self
            .inner
            .post(url)
            .header("Content-Type", content_type)
            .body(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send("POST", url, req)
    }

    /// POST `application/json` and return a streaming line reader over the (SSE)
    /// response body — no buffering. For `stream: true` chat completions.
    pub fn post_json_stream(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<EventStream> {
        let mut req = self.inner.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = send_with_retry("POST", url, req).map_err(|e| AppError(e.to_string()))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        Ok(EventStream {
            status,
            content_type,
            reader: BufReader::new(resp),
            request_url: url.to_string(),
        })
    }
}

/// Send a request, retrying exactly once with a fresh connection if the
/// first attempt fails before we get a response back at all.
///
/// No backoff: the overwhelmingly likely cause is a stale pooled connection
/// (a peer — e.g. an AWS ALB — idle-closed it after we last used it; see
/// `Client::new`'s `pool_idle_timeout` comment), and a brand-new connection
/// clears that instantly. Waiting wouldn't help a dead socket, and a genuine
/// outage fails the retry just as fast as the original attempt — so this is
/// not the "retry a flaky API with growing backoff" policy (that's a
/// different problem — real transient 5xx/429 responses from a server that
/// *did* answer — and would need its own, separately-scoped retry policy).
///
/// Safe to retry unconditionally here (not just for idempotent methods):
/// every call site through this module is an LLM/media API call (chat
/// completion, token exchange, audio upload/synthesis) where, at worst, a
/// retry re-dispatches a request the first attempt never got a response for
/// — wasteful if the server did somehow start work, never corrupting.
///
/// `try_clone()` only fails for a non-replayable body (a raw stream); every
/// body sent through this module (JSON, form, in-memory multipart) is
/// buffered and clones fine, so this silently skips the retry only in a case
/// that doesn't occur here.
fn send_with_retry(
    method: &str,
    url: &str,
    req: reqwest::blocking::RequestBuilder,
) -> reqwest::Result<reqwest::blocking::Response> {
    let retry = req.try_clone();
    match req.send() {
        Ok(resp) => Ok(resp),
        Err(first_err) => {
            crate::diag::log_network_error(
                method,
                url,
                &format!("retrying once after: {first_err}"),
            );
            match retry {
                Some(retry_req) => retry_req.send(),
                None => Err(first_err),
            }
        }
    }
}

fn send(method: &str, url: &str, req: reqwest::blocking::RequestBuilder) -> Result<Resp> {
    // Total request timeout — non-streaming only (see `Client::new`). Streaming
    // (`post_json_stream`) builds and sends its own request without this, so a
    // long generation is never cut short.
    let req = req.timeout(std::time::Duration::from_secs(120));
    let resp = send_with_retry(method, url, req).map_err(|e| {
        let msg = e.to_string();
        crate::diag::log_network_error(method, url, &msg);
        AppError(msg)
    })?;
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let headers = collect_headers(resp.headers());
    let body = resp
        .bytes()
        .map_err(|e| {
            let msg = e.to_string();
            crate::diag::log_network_error(method, url, &msg);
            AppError(msg)
        })?
        .to_vec();
    Ok(Resp {
        status,
        status_text,
        body,
        headers,
    })
}

/// Snapshot a `reqwest` header map into owned, lowercased `(name, value)`
/// pairs — keeps `reqwest::header::HeaderMap` from leaking past this module.
/// A non-UTF-8 header value is dropped rather than erroring the whole
/// response over one exotic header.
fn collect_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_ascii_lowercase(), v.to_string()))
        })
        .collect()
}

/// A streaming HTTP response — a blocking line reader over the body. Keeps the
/// `reqwest` type private so streaming stays behind the net boundary; the caller
/// parses SSE (`data:` lines) itself.
pub struct EventStream {
    pub status: u16,
    pub content_type: String,
    reader: BufReader<reqwest::blocking::Response>,
    /// The request URL, kept only for diagnostic logging on a mid-stream read
    /// failure (see `next_line`).
    request_url: String,
}

impl EventStream {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether the body is a Server-Sent Events stream.
    pub fn is_event_stream(&self) -> bool {
        self.content_type.contains("event-stream")
    }

    /// Next line (including the trailing newline), or `None` at end of stream.
    pub fn next_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(|e| {
            let msg = e.to_string();
            crate::diag::log_network_error("POST", &self.request_url, &msg);
            AppError(msg)
        })?;
        Ok(if n == 0 { None } else { Some(line) })
    }

    /// Drain the remaining body to a string (error bodies / non-SSE responses).
    pub fn read_all(&mut self) -> String {
        let mut s = String::new();
        let _ = self.reader.read_to_string(&mut s);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::send_with_retry;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Simulates the actual bug: a first attempt that fails, then a
    /// perfectly normal one. Both connections run a full, ordinary
    /// accept/read/write/close cycle — the only difference is that the
    /// first writes content hyper's h1 parser will reject.
    ///
    /// (An earlier version of this test instead had the first connection
    /// die with no response at all, mimicking a stale pooled connection
    /// exactly — but that made the test itself flaky, independent of
    /// `send_with_retry`: hyper's client occasionally read the *second*
    /// connection's perfectly valid response as `UnexpectedMessage` (seen
    /// even with the first connection's request fully drained before close,
    /// and even with the retry using a wholly separate `Client`, ruling out
    /// connection-pool reuse as the cause — some hyper/loopback timing
    /// artifact specific to two rapid-fire real connections to the same
    /// address, not a defect in this module. This version sidesteps it
    /// entirely, at zero cost to what's actually under test: `send_with_retry`
    /// only cares that the first `send()` returned `Err`, not why.)
    #[test]
    fn send_with_retry_recovers_from_a_failed_first_attempt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"not an http response at all\r\n\r\n");
            }
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                );
            }
        });

        let client = reqwest::blocking::Client::new();
        let url = format!("http://{addr}/");
        let resp = send_with_retry("GET", &url, client.get(&url)).expect("retry should recover");
        assert_eq!(resp.status(), 200);
    }

    /// When even the retry fails (the peer never comes back at all), the
    /// original error surfaces rather than hanging or panicking.
    #[test]
    fn send_with_retry_gives_up_after_one_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // Drop both the first attempt and the retry.
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        });

        let client = reqwest::blocking::Client::new();
        let url = format!("http://{addr}/");
        assert!(send_with_retry("GET", &url, client.get(&url)).is_err());
    }
}
