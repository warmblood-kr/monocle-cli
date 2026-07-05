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
            .build()
            .expect("failed to build HTTP client");
        Self { inner }
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Resp> {
        let mut req = self.inner.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send(req)
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
        send(req)
    }

    /// POST `application/json`.
    pub fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Resp> {
        let mut req = self.inner.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send(req)
    }

    /// POST `multipart/form-data` with one binary file part plus text fields.
    pub fn post_multipart(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        file: FilePart,
        text_fields: &[(&str, &str)],
    ) -> Result<Resp> {
        let part = reqwest::blocking::multipart::Part::bytes(file.data)
            .file_name(file.filename)
            .mime_str(&file.content_type)
            .map_err(|e| AppError(e.to_string()))?;
        let mut form = reqwest::blocking::multipart::Form::new().part(file.field, part);
        for (k, v) in text_fields {
            form = form.text(k.to_string(), v.to_string());
        }
        let mut req = self.inner.post(url).multipart(form);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        send(req)
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
        send(req)
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
        let resp = req.send().map_err(|e| AppError(e.to_string()))?;
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
        })
    }
}

fn send(req: reqwest::blocking::RequestBuilder) -> Result<Resp> {
    // Total request timeout — non-streaming only (see `Client::new`). Streaming
    // (`post_json_stream`) builds and sends its own request without this, so a
    // long generation is never cut short.
    let req = req.timeout(std::time::Duration::from_secs(120));
    let resp = req.send().map_err(|e| AppError(e.to_string()))?;
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let body = resp.bytes().map_err(|e| AppError(e.to_string()))?.to_vec();
    Ok(Resp {
        status,
        status_text,
        body,
    })
}

/// A streaming HTTP response — a blocking line reader over the body. Keeps the
/// `reqwest` type private so streaming stays behind the net boundary; the caller
/// parses SSE (`data:` lines) itself.
pub struct EventStream {
    pub status: u16,
    pub content_type: String,
    reader: BufReader<reqwest::blocking::Response>,
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
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|e| AppError(e.to_string()))?;
        Ok(if n == 0 { None } else { Some(line) })
    }

    /// Drain the remaining body to a string (error bodies / non-SSE responses).
    pub fn read_all(&mut self) -> String {
        let mut s = String::new();
        let _ = self.reader.read_to_string(&mut s);
        s
    }
}
