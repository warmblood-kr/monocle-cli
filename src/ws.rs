//! The WebSocket facade — the ONE place that touches `tungstenite` (and the
//! rustls glue for `wss://`). Like `net.rs`, callers see only plain sync types.
//!
//! The connection carries a secret in its `Sec-WebSocket-Protocol` header, so:
//! the URL is vetted before any socket is opened (`wss`, or `ws` to loopback
//! only; no userinfo), no redirect is followed (a non-101 answer is an error),
//! and every failure maps to a fixed message — never the underlying error, whose
//! text or `Debug` can echo request headers.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::error::{Error as WsError, UrlError};
use tungstenite::http::HeaderValue;
use tungstenite::{HandshakeError, Message, WebSocket};

use crate::error::{AppError, Result};

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

pub struct WsConn {
    sock: WebSocket<Stream>,
    /// Handle on the same socket, to change the read timeout per call.
    tcp: TcpStream,
}

fn fail(what: &str) -> AppError {
    AppError::new(format!("WebSocket {what}."))
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// `(tls, host, port)` of a vetted `ws(s)://host[:port]/...` URL.
fn vet(url: &str) -> Result<(bool, String, u16)> {
    let refuse = || fail("URL refused (need wss://, or ws:// to loopback)");
    let uri: tungstenite::http::Uri = url.parse().map_err(|_| refuse())?;
    let tls = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => return Err(refuse()),
    };
    if uri.authority().is_none_or(|a| a.as_str().contains('@')) {
        return Err(refuse());
    }
    let host = uri.host().ok_or_else(refuse)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if !tls && !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(refuse());
    }
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    Ok((tls, host.to_string(), port))
}

fn tls_stream(host: &str, tcp: TcpStream) -> Result<Stream> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| fail("TLS setup failed"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| fail("URL refused (bad host)"))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|_| fail("TLS setup failed"))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// Open a connection and complete the handshake: DNS and the TCP connect share
/// `timeout`; each handshake I/O step then gets what is left of it.
pub fn connect(url: &str, subprotocols: &[&str], timeout: Duration) -> Result<WsConn> {
    let started = Instant::now();
    let (tls, host, port) = vet(url)?;
    if subprotocols.is_empty()
        || subprotocols
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(is_token_char))
    {
        return Err(fail("subprotocol refused (invalid characters)"));
    }
    let mut request = url
        .into_client_request()
        .map_err(|_| fail("request could not be built"))?;
    let proto = HeaderValue::from_str(&subprotocols.join(", "))
        .map_err(|_| fail("subprotocol refused (invalid characters)"))?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", proto);

    // DNS has no timeout of its own: resolve on a helper thread and stop waiting
    // at the deadline (the thread is abandoned; the process exits soon after).
    let (tx, rx) = std::sync::mpsc::channel();
    let target = (host.clone(), port);
    std::thread::spawn(move || {
        let _ = tx.send(target.to_socket_addrs().ok().and_then(|mut a| a.next()));
    });
    let addr = rx
        .recv_timeout(timeout)
        .map_err(|_| fail("connect timed out"))?
        .ok_or_else(|| fail("host could not be resolved"))?;
    let timeout = timeout
        .saturating_sub(started.elapsed())
        .max(Duration::from_millis(1));
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|_| fail("connection failed"))?;
    tcp.set_read_timeout(Some(timeout))
        .and_then(|_| tcp.set_write_timeout(Some(timeout)))
        .map_err(|_| fail("connection failed"))?;
    let handle = tcp.try_clone().map_err(|_| fail("connection failed"))?;
    let stream = if tls {
        tls_stream(&host, tcp)?
    } else {
        Stream::Plain(tcp)
    };
    // The handshake error is mapped here and never formatted: its Display/Debug
    // may carry the request or response headers.
    let (sock, _) = tungstenite::client(request, stream).map_err(|e| match e {
        HandshakeError::Failure(WsError::Http(resp)) => AppError::new(format!(
            "WebSocket handshake refused: HTTP {}.",
            resp.status().as_u16()
        )),
        HandshakeError::Failure(WsError::Url(UrlError::UnsupportedUrlScheme)) => {
            fail("URL refused")
        }
        HandshakeError::Failure(WsError::Io(_)) => fail("handshake failed (I/O error or timeout)"),
        HandshakeError::Failure(WsError::Protocol(_)) => {
            fail("handshake failed (protocol violation by the server)")
        }
        _ => fail("handshake failed"),
    })?;
    Ok(WsConn { sock, tcp: handle })
}

impl WsConn {
    pub fn send_text(&mut self, text: &str) -> Result<()> {
        self.sock
            .send(Message::text(text))
            .map_err(|_| fail("send failed"))
    }

    /// Next text frame; `Ok(None)` if none arrived within `timeout`. Control
    /// frames are handled here; a server close or I/O failure is an error.
    pub fn recv_text(&mut self, timeout: Duration) -> Result<Option<String>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.tcp
                .set_read_timeout(Some(left))
                .map_err(|_| fail("receive failed"))?;
            match self.sock.read() {
                Ok(Message::Text(t)) => return Ok(Some(t.as_str().to_string())),
                Ok(Message::Close(_)) => return Err(fail("closed by the server")),
                Ok(Message::Ping(_)) => {
                    let _ = self.sock.flush();
                }
                Ok(_) => {}
                Err(WsError::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None)
                }
                Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                    return Err(fail("closed by the server"))
                }
                Err(_) => return Err(fail("receive failed")),
            }
        }
    }

    pub fn close(&mut self) {
        let _ = self.sock.close(None);
        let _ = self.sock.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vet_allows_wss_and_loopback_ws_only() {
        assert!(vet("wss://craft.monocle-ai.com/ws/x").is_ok());
        assert!(vet("ws://127.0.0.1:9/ws/x").is_ok());
        assert!(vet("ws://[::1]:9/x").is_ok());
        for bad in [
            "ws://craft.monocle-ai.com/x",
            "ws://127.0.0.1.evil.com/x",
            "wss://u:p@host/x",
            "http://127.0.0.1/x",
            "wss://",
            "nonsense",
        ] {
            assert!(vet(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn invalid_subprotocol_is_refused_before_connecting() {
        // Port 9 is never contacted: validation fails first.
        let e = connect(
            "ws://127.0.0.1:9/x",
            &["openworker", "a b"],
            Duration::from_secs(1),
        );
        assert!(e.is_err());
        let e = connect(
            "ws://127.0.0.1:9/x",
            &["openworker", "a\r\nX: y"],
            Duration::from_secs(1),
        );
        assert!(e.is_err());
    }
}
