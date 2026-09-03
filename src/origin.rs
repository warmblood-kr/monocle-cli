//! Identifies this CLI as the call surface ("origin") so chat-proxy can
//! attribute LLM usage/billing per surface (chat/craft/cli).
//! Tracking: warmblood-kr/chat-proxy#216

use base64::Engine as _;
use rand::RngCore;
use std::sync::OnceLock;

pub const MONOCLE_ORIGIN: &str = "cli";

pub const ORIGIN_HEADER_NAME: &str = "x-monocle-origin";

/// The origin header as a `(name, value)` pair, ready to spread into request
/// headers hitting chat-proxy.
pub fn origin_header() -> (&'static str, &'static str) {
    (ORIGIN_HEADER_NAME, MONOCLE_ORIGIN)
}

/// The standard headers for an authenticated chat-proxy call: bearer auth + the
/// origin attribution header. `bearer` must be the full `Bearer <token>` value.
/// One place owns this contract so a new header is added once, not at every site.
pub fn auth_headers(bearer: &str) -> [(&str, &str); 2] {
    [("Authorization", bearer), origin_header()]
}

pub const SESSION_HEADER_NAME: &str = "x-session-id";

/// A stable id for this CLI process.
///
/// Deliberately NOT part of `auth_headers`: that pair goes to chat-proxy from a
/// dozen call sites, while this header only changes behaviour at jarvice's
/// `/api/responses`. Keeping it out of the common pair keeps the blast radius
/// at the one endpoint that reads it.
///
/// Why it has to be sent at all: jarvice gates a turn's post-response work —
/// persisting the assistant reply and generating the chat title — on having a
/// session id, so a caller that omits the header gets its answer but the
/// conversation is never stored. Measured on staging, same request twice:
/// with the header the assistant content is saved and a title is generated;
/// without it the thread stays "New Chat" with an empty assistant message.
/// (warmblood-kr/jarvice#1455, #1398 — the server-side fix is to key that work
/// on durability rather than on delivery; until then the header is what a
/// non-browser client must send.)
///
/// The value only needs to identify this caller. Nothing is streamed back to
/// it: this client is non-streaming, and the id names a delivery channel that
/// simply has no listener.
pub fn session_id() -> &'static str {
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        format!(
            "{}-{}",
            MONOCLE_ORIGIN,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        )
    })
}

/// Headers for a jarvice `/api/responses` call: the standard pair plus the
/// session id its persistence is gated on.
pub fn responses_headers(bearer: &str) -> [(&str, &str); 3] {
    [
        ("Authorization", bearer),
        origin_header(),
        (SESSION_HEADER_NAME, session_id()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_stable_within_the_process() {
        assert_eq!(session_id(), session_id(), "the id names one session");
    }

    #[test]
    fn responses_headers_carry_the_session_id() {
        let h = responses_headers("Bearer t");
        let found = h.iter().find(|(k, _)| *k == SESSION_HEADER_NAME);
        assert_eq!(found.map(|(_, v)| *v), Some(session_id()));
    }

    #[test]
    fn auth_headers_stays_free_of_it() {
        // The regression guard: this pair goes to chat-proxy from many sites.
        let h = auth_headers("Bearer t");
        assert!(h.iter().all(|(k, _)| *k != SESSION_HEADER_NAME));
    }
}
