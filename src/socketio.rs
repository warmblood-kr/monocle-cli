//! Minimal Socket.IO v4 client (WebSocket-only engine.io v4) over [`crate::ws`],
//! for jarvice's `/agent` namespace. Packets are plain text: the first char is the
//! engine.io type (0 open, 2 ping, 3 pong, 4 message); a message carries a
//! socket.io packet `<type>[/ns,][id]<json>` (0 connect, 2 event, 4 connect_error).
//!
//! The connect auth carries a credential: it is only ever written to the socket,
//! never formatted into an error, and every failure maps to a fixed message.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::craft_server::url_parts;
use crate::error::{AppError, Result};
use crate::ws::{self, WsConn};

const NS: &str = "/agent";

/// A decoded socket.io packet (the payload of an engine.io `4` message).
#[derive(Debug, PartialEq)]
pub struct Packet {
    pub kind: u8,
    pub ns: Option<String>,
    pub data: Option<Value>,
}

pub fn encode_connect(ns: &str, auth: &Value) -> String {
    format!("40{ns},{auth}")
}

pub fn encode_event(ns: &str, name: &str, data: &Value) -> String {
    format!("42{ns},{}", json!([name, data]))
}

/// Decode the socket.io part (after the leading engine.io `4`).
pub fn decode_packet(s: &str) -> Option<Packet> {
    let kind = s.chars().next()?.to_digit(10)? as u8;
    let mut rest = &s[1..];
    let mut ns = None;
    if rest.starts_with('/') {
        let (n, r) = rest.split_once(',').unwrap_or((rest, ""));
        ns = Some(n.to_string());
        rest = r;
    }
    // Optional ack id before the payload.
    rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
    let data = if rest.is_empty() {
        None
    } else {
        Some(serde_json::from_str(rest).ok()?)
    };
    Some(Packet { kind, ns, data })
}

/// `wss://host` / `ws://loopback:port` origin for a jarvice base URL. The socket
/// carries the HS256 token, so a non-loopback host must be https on the default port.
pub fn ws_origin(base: &str) -> Result<String> {
    let bad = || AppError::new("Refusing socket URL: need https on the default port, or loopback.");
    let (scheme, host, port) = url_parts(base).ok_or_else(bad)?;
    let authority = &base[scheme.len() + 3..];
    if authority.contains(['/', '?', '#', '\\', ' ']) || host.is_empty() {
        return Err(bad());
    }
    match (scheme, host, port) {
        ("http", "localhost" | "127.0.0.1" | "::1", _) => Ok(format!("ws://{authority}")),
        ("https", h, None | Some("443"))
            if h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') =>
        {
            Ok(format!("wss://{authority}"))
        }
        _ => Err(bad()),
    }
}

pub struct SocketIo {
    ws: WsConn,
}

fn fail(what: &str) -> AppError {
    AppError::new(format!("Socket.IO {what}."))
}

impl SocketIo {
    /// Connect `origin` (from [`ws_origin`]), open the engine, join `/agent` with
    /// `{"token": token}`. Fails on `connect_error` without echoing its body.
    pub fn connect(origin: &str, token: &str, step: Duration) -> Result<Self> {
        let url = format!("{origin}/ws/socket.io/?EIO=4&transport=websocket");
        let mut me = SocketIo {
            ws: ws::connect_with(&url, &[], step)?,
        };
        let deadline = Instant::now() + step;
        let mut sent = false;
        loop {
            let Some(raw) = me
                .ws
                .recv_text(deadline.saturating_duration_since(Instant::now()))?
            else {
                return Err(fail("handshake timed out"));
            };
            match (raw.chars().next(), sent) {
                (Some('0'), false) => {
                    me.ws
                        .send_text(&encode_connect(NS, &json!({ "token": token })))?;
                    sent = true;
                }
                (Some('2'), _) => me.ws.send_text("3")?,
                (Some('4'), true) => match decode_packet(&raw[1..]) {
                    Some(p) if p.kind == 0 && p.ns.as_deref() == Some(NS) => return Ok(me),
                    Some(p) if p.kind == 4 => return Err(fail("authentication was refused")),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    pub fn emit(&mut self, name: &str, data: &Value) -> Result<()> {
        self.ws.send_text(&encode_event(NS, name, data))
    }

    /// Next `/agent` event as `(name, first argument)`; `Ok(None)` after `timeout`.
    /// Engine pings are answered here.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<(String, Value)>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(raw) = self.ws.recv_text(left)? else {
                return Ok(None);
            };
            match raw.chars().next() {
                Some('2') => self.ws.send_text("3")?,
                Some('1') => return Err(fail("closed by the server")),
                Some('4') => {
                    let Some(p) = decode_packet(&raw[1..]) else {
                        continue;
                    };
                    if p.ns.as_deref().is_some_and(|n| n != NS) {
                        continue;
                    }
                    match (p.kind, p.data) {
                        (1, _) => return Err(fail("disconnected by the server")),
                        (2, Some(Value::Array(a))) => {
                            if let Some(name) = a.first().and_then(Value::as_str) {
                                let data = a.get(1).cloned().unwrap_or(Value::Null);
                                return Ok(Some((name.to_string(), data)));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub fn close(&mut self) {
        self.ws.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_connect_and_event() {
        assert_eq!(
            encode_connect("/agent", &json!({"token": "t"})),
            r#"40/agent,{"token":"t"}"#
        );
        assert_eq!(
            encode_event("/agent", "start_session", &json!({"session_id": "s"})),
            r#"42/agent,["start_session",{"session_id":"s"}]"#
        );
    }

    #[test]
    fn decodes_namespaced_plain_and_ack_id_forms() {
        let p = decode_packet(r#"2/agent,["text",{"a":1}]"#).unwrap();
        assert_eq!((p.kind, p.ns.as_deref()), (2, Some("/agent")));
        assert_eq!(p.data.unwrap()[0], "text");
        assert_eq!(
            decode_packet(r#"2/agent,17["x",1]"#).unwrap().data.unwrap()[0],
            "x"
        );
        let p = decode_packet(r#"2["x"]"#).unwrap();
        assert_eq!((p.kind, p.ns), (2, None));
        let p = decode_packet(r#"0/agent,{"sid":"z"}"#).unwrap();
        assert_eq!((p.kind, p.data.unwrap()["sid"].clone()), (0, json!("z")));
        assert_eq!(decode_packet("0/agent").unwrap().data, None);
        assert!(decode_packet("x").is_none() && decode_packet("2/agent,{bad").is_none());
    }

    #[test]
    fn origin_allows_default_port_https_and_loopback_only() {
        assert_eq!(
            ws_origin("https://t.example.com").unwrap(),
            "wss://t.example.com"
        );
        assert_eq!(
            ws_origin("http://127.0.0.1:8080").unwrap(),
            "ws://127.0.0.1:8080"
        );
        for bad in [
            "https://t.example.com:8443",
            "http://t.example.com",
            "https://u@t.example.com",
            "https://t.example.com/x",
            "https://t.example.com\\@evil.com",
            "ftp://t.example.com",
        ] {
            assert!(ws_origin(bad).is_err(), "{bad}");
        }
    }
}
