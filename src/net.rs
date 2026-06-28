//! The HTTP facade — the ONE place that touches `reqwest`.
//!
//! Everything above this module is plain sync code that speaks only in
//! `net::Resp`. Today the implementation is `reqwest::blocking`; if we ever need
//! async (concurrency, streaming), a single function here can spin up a
//! `tokio::Runtime` and `block_on` internally while keeping these sync
//! signatures — so "go async later" never colors the call tree. Keep `reqwest`
//! types from leaking past this boundary.

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
        let inner = reqwest::blocking::Client::builder()
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
}

fn send(req: reqwest::blocking::RequestBuilder) -> Result<Resp> {
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
